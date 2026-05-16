//! Assertion → Observation / Event / Relation / EntityAttribute
//! promotion (Session 81; ADR 0004; ADR 0021).
//!
//! # What this stage does
//!
//! Walks the persisted `Assertion` rows produced by extraction
//! (Sessions 77 / 78 / 79 / 80) and, for groups of independent
//! claimants making compatible claims, emits a single *promoted*
//! record (`Observation` / `Event` / `Relation`) or — for
//! `AssertedContent::EntityAttribute` — re-emits the attribute as a
//! consensus-stamped Assertion the dashboard can mark as promoted.
//!
//! Per ADR 0004 the original Assertions are **preserved** — their
//! claim structure remains queryable for the anomaly-detection
//! surfaces and for the "show me everyone who made this claim" UI.
//! The promoted record's `Envelope::provenance.derived_from` carries
//! `DerivedFrom { record_id, role: DerivationRole::ConsensusSupport }`
//! for every supporting Assertion, so the link from promoted record
//! back to its consensus inputs is explicit.
//!
//! # Quorum
//!
//! The default quorum is **N=3 independent claimants** (ADR 0004's
//! choice). "Independent" means distinct `Assertion::claimant`
//! `EntityId`s — five assertions all claimed by `agency:reuters`
//! count as one. The per-call quorum is configurable so an operator
//! running a "preview consensus" sweep on a fresh plan can lower it
//! to 2.
//!
//! # Idempotency
//!
//! Each promoted record carries a `dedup_key` of the form
//! `promotion:{content_hash}:{subject_hash}`. ADR 0004 names this as
//! one of the few places where content-derived keys are appropriate
//! — the question "has this been promoted" is genuinely about
//! content identity, not source identity. On re-run, rows that would
//! produce the same dedup_key are skipped before the insert, so the
//! promotion stage is safe to run many times against a growing
//! assertion store.
//!
//! `content_hash` is SHA-256 (truncated to 128 bits) over the
//! canonical JSON serialization of the `AssertedContent` value.
//! `subject_hash` is SHA-256 (truncated to 128 bits) over the sorted
//! topic strings on the Assertion's envelope — sorted because
//! `Subjects::topics` order is incidental.
//!
//! Session 84 — ADR 0021 amendment 1. The original Session 81 code
//! used a `(DefaultHasher, DefaultHasher-with-salt)` 128-bit pair
//! because the workspace did not yet take a hashing-crate dependency.
//! Session 84 lands the named-swap path: `sha2` is now a workspace
//! dep, `hex128` truncates SHA-256 to the same 32-char lowercase hex
//! shape on disk, and the collision floor goes from "2^64 SipHash
//! pairs" to "cryptographically sound 2^64 birthday bound on the
//! truncated digest." Dedup keys produced before Session 84 will not
//! match dedup keys produced after; the migration story is "re-runs
//! produce fresh promoted records and the per-shape tables grow by
//! one row per group on the first post-swap pass." Acceptable
//! because consensus_promotion records are derived data, not
//! upstream truth.
//!
//! # Authoritative pathway — Session 82
//!
//! ADR 0004 describes two pathways: consensus (this module's original
//! surface) and authoritative (config-driven fast-track). Session 82
//! lands the authoritative half via
//! [`crate::authoritative::AuthorityRegistry`] +
//! `promote_authoritative_pass`. When `PromoteConfig::authoritative`
//! is non-empty `promote_from_assertions` runs an authoritative
//! pre-pass first: every Assertion whose claimant matches an entry
//! promotes at N=1 (single-source fast track) with `source_id =
//! "derived#authoritative"` + `tags = ["authoritative_promotion"]` +
//! `DerivationRole::Promotion`. Surviving Assertions then go through
//! the original consensus pass at N≥3.
//!
//! When the registry is empty (default), Session 81 behaviour is
//! preserved exactly — the authoritative pre-pass is a no-op and
//! consensus runs over every Assertion.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use situation_room_core::schema::content::{
    AssertedContent, EntityAttributeContent, EventContent, ObservationContent, RelationContent,
};
use situation_room_core::schema::envelope::{DerivationRole, DerivedFrom, Envelope, Provenance};
use situation_room_core::schema::records::{Assertion, Event, Observation, Relation};
use situation_room_core::vocab::Confidence;
use situation_room_storage::Store;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::authoritative::AuthorityRegistry;
use crate::research::ResearchPlan;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Configuration for one promotion run.
///
/// Carries the consensus quorum (ADR 0004 default 3) and the
/// authoritative-source registry (Session 82). An empty registry
/// (default) reproduces Session 81's consensus-only behaviour; a
/// populated registry adds an authoritative pre-pass that fast-tracks
/// matching Assertions at N=1.
#[derive(Debug, Clone)]
pub struct PromoteConfig {
    /// Minimum number of *distinct* claimants that must agree on a
    /// compatible claim before it promotes. ADR 0004 default is 3.
    pub min_independent_claimants: u32,
    /// ADR 0004 pathway 1 — Session 82. When non-empty, Assertions
    /// whose claimant matches an entry are fast-tracked at N=1; the
    /// remaining Assertions then go through the consensus quorum.
    /// Empty registry = Session 81 behaviour (consensus only).
    pub authoritative: AuthorityRegistry,
}

impl Default for PromoteConfig {
    fn default() -> Self {
        Self {
            min_independent_claimants: 3,
            authoritative: AuthorityRegistry::empty(),
        }
    }
}

/// Summary of one promotion pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteReport {
    /// How many Assertion rows the pass considered.
    pub assertions_considered: u32,
    /// How many distinct `(content_hash, subject_hash)` groups met
    /// quorum (consensus) AND were not pre-empted by an authoritative
    /// match for at least one claimant, and produced a promoted record
    /// this run via the consensus pathway.
    pub groups_promoted: u32,
    /// How many promoted-record inserts skipped because the
    /// `dedup_key` already existed in storage (idempotency hit).
    pub skipped_already_promoted: u32,
    /// How many promoted Observations were emitted this run.
    pub observations_emitted: u32,
    /// How many promoted Events were emitted this run.
    pub events_emitted: u32,
    /// How many promoted Relations were emitted this run.
    pub relations_emitted: u32,
    /// How many consensus-stamped EntityAttribute Assertions were
    /// emitted this run. (Per ADR 0004 EntityAttribute "promotion"
    /// updates the target entity's attributes — today we surface this
    /// as a synthesised Assertion whose claimant is `agency:consensus`
    /// and stance is `Asserted`. The original per-document
    /// Assertions stay preserved.)
    pub entity_attributes_emitted: u32,
    /// Per-Assertion insert failures (warn-logged in-band).
    pub insert_failures: u32,
    /// Session 82 — ADR 0004 pathway 1. How many Assertions promoted
    /// via the authoritative-source registry (N=1 fast-track). Counted
    /// against `observations_emitted` / `events_emitted` /
    /// `relations_emitted` / `entity_attributes_emitted` too — the
    /// per-shape counters cover both pathways. This field surfaces the
    /// fraction attributable to the authoritative half.
    pub authoritative_promoted: u32,
}

/// Errors that can propagate out of the promote stage.
///
/// Most failures (single-row insert errors, partial-batch hiccups)
/// don't reach this enum — the runtime path warn-logs and continues.
/// Only failures that prevent the pass from running at all surface
/// here.
#[derive(Debug, Error)]
pub enum PromoteError {
    #[error("storage error: {0}")]
    Storage(#[from] situation_room_storage::StorageError),
}

/// Run the promotion pass for one plan.
///
/// Reads every persisted `Assertion` tied to the plan (via the
/// `records_for_plan` recipe-routing join), runs the authoritative
/// fast-track pass (Session 82) if `cfg.authoritative` is non-empty,
/// then runs the consensus pass over the remaining Assertions.
/// Idempotent on re-run via the
/// `promotion:{content_hash}:{subject_hash}` dedup key.
///
/// Never returns the partial-batch error shape: per-row insert
/// failures are warn-logged and counted into `insert_failures`. The
/// outer `Result` covers only "the pass couldn't start" failures
/// (storage open, the per-plan records-load query itself).
///
/// **Naming note.** Function name kept stable for IPC compatibility
/// (the Tauri command surface refers to it by this name). Despite the
/// `_consensus_` infix the function runs both ADR 0004 pathways when
/// the registry is populated.
pub fn promote_consensus_for_plan(
    store: &Store,
    plan: &ResearchPlan,
    cfg: &PromoteConfig,
) -> Result<PromoteReport, PromoteError> {
    let bucket = store.records_for_plan(plan.id)?;
    let assertions = bucket.assertions;
    Ok(promote_from_assertions(store, plan, &assertions, cfg))
}

/// Same as [`promote_consensus_for_plan`] but takes a caller-supplied
/// slice of Assertions. Used by the auto-trigger hook in
/// `fetch_executor` (Session 82) so it doesn't double-read the
/// records_for_plan join the executor doesn't need to issue twice.
pub fn promote_from_assertions(
    store: &Store,
    plan: &ResearchPlan,
    assertions: &[Assertion],
    cfg: &PromoteConfig,
) -> PromoteReport {
    // Session 82 — authoritative pre-pass. When the registry is empty
    // (default), `promote_authoritative_pass` returns an empty
    // report + the full input slice as "remaining" Assertions, so the
    // consensus pass runs over everything (Session 81 behaviour).
    let (mut report, remaining) =
        promote_authoritative_pass(store, plan, assertions, &cfg.authoritative);
    let consensus_report =
        promote_consensus_from_assertions_with_report(store, plan, &remaining, cfg);
    merge_consensus_into(&mut report, consensus_report);
    report
}

/// Pure(-ish — still writes to `Store`) helper that promotes a
/// caller-supplied slice of `Assertion` rows via the consensus
/// pathway only (no authoritative pre-pass). Split out so tests can
/// hand a synthetic Vec without standing up the records-for-plan
/// join, and so the public [`promote_from_assertions`] can wire the
/// authoritative pre-pass in front of it.
pub fn promote_consensus_from_assertions(
    store: &Store,
    plan: &ResearchPlan,
    assertions: &[Assertion],
    cfg: &PromoteConfig,
) -> PromoteReport {
    promote_consensus_from_assertions_with_report(store, plan, assertions, cfg)
}

/// Consensus pass that returns a `PromoteReport` carrying only the
/// consensus-side counters (the auth-pass counters stay zero on the
/// returned shape). The orchestrator merges this into a possibly
/// already-populated auth report via [`merge_consensus_into`].
fn promote_consensus_from_assertions_with_report(
    store: &Store,
    plan: &ResearchPlan,
    assertions: &[Assertion],
    cfg: &PromoteConfig,
) -> PromoteReport {
    let mut report = PromoteReport {
        assertions_considered: assertions.len() as u32,
        ..PromoteReport::default()
    };

    let groups = group_assertions_for_consensus(assertions);
    let now = Utc::now();

    for (key, group) in groups {
        let distinct_claimants: BTreeSet<&str> =
            group.iter().map(|a| a.claimant.as_str()).collect();

        // Session 84 — per-claimant consensus quorum override (ADR
        // 0004 amendment). When any group member's claimant matches
        // an `[[authority]]` entry with a `consensus_quorum` set, the
        // group's effective quorum bar drops to that override (or the
        // minimum across multiple matching members). Falls back to
        // `cfg.min_independent_claimants` when no override applies.
        //
        // Entries with `consensus_quorum = 1` (or unset, i.e. the
        // Session-82 fast-track default) never reach this branch in
        // practice — the auth pre-pass already promoted them. We
        // honour Some(1) here too as defence in depth in case a
        // future caller invokes the consensus path directly.
        let effective_quorum = group
            .iter()
            .filter_map(|a| cfg.authoritative.quorum_override_for(a))
            .min()
            .map(|q| q.min(cfg.min_independent_claimants))
            .unwrap_or(cfg.min_independent_claimants);

        if (distinct_claimants.len() as u32) < effective_quorum {
            continue;
        }

        let dedup_key = format!("promotion:{}:{}", key.content_hash, key.subject_hash);
        let supports: Vec<DerivedFrom> = group
            .iter()
            .map(|a| DerivedFrom {
                record_id: a.id,
                role: DerivationRole::ConsensusSupport,
            })
            .collect();

        // Pick a representative for envelope-shape decisions (subjects
        // / topics / valid_at). The first member is a stable choice —
        // `records_for_plan` returns assertions observed_at-DESC, so
        // the first is the most recent. Future versions may want the
        // median of timestamps; today's "most recent" matches the
        // dashboard's "latest known value" framing.
        let representative = group[0];

        // Confidence on the promoted record: average across supporting
        // claimants, clamped. ADR 0004 leaves this to implementation;
        // averaging matches the operator-readable "the more sources
        // agree the more we trust it" intuition while not amplifying
        // a single zealous claimant.
        let avg_confidence = avg_confidence(&group);

        // Session 82 — explicit pre-insert idempotency check. The
        // per-shape tables' dedup_key indices are non-unique by
        // migration design (see ADR 0021 amendment notes); the DB
        // alone won't reject a duplicate. Check first.
        let outcome = if dedup_already_promoted(store, &representative.content, &dedup_key) {
            info!(
                dedup_key = %dedup_key,
                "consensus promote skipped — record with this dedup_key already exists"
            );
            InsertResult::SkippedDuplicate
        } else {
            match &representative.content {
                AssertedContent::Observation(c) => {
                    let obs = build_promoted_observation(
                        plan,
                        representative,
                        c,
                        &supports,
                        avg_confidence,
                        &dedup_key,
                    );
                    match store.insert_observation(&obs) {
                        Ok(()) => {
                            report.observations_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::Event(c) => {
                    let ev = build_promoted_event(
                        plan,
                        representative,
                        c,
                        &supports,
                        avg_confidence,
                        &dedup_key,
                    );
                    match store.insert_event(&ev) {
                        Ok(()) => {
                            report.events_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::Relation(c) => {
                    let rel = build_promoted_relation(
                        plan,
                        representative,
                        c,
                        &supports,
                        avg_confidence,
                        &dedup_key,
                    );
                    match store.insert_relation(&rel) {
                        Ok(()) => {
                            report.relations_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::EntityAttribute(c) => {
                    let assertion = build_promoted_entity_attribute(
                        plan,
                        representative,
                        c,
                        &supports,
                        avg_confidence,
                        &dedup_key,
                    );
                    match store.insert_assertion(&assertion) {
                        Ok(()) => {
                            report.entity_attributes_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
            }
        };

        match outcome {
            InsertResult::Inserted => {
                report.groups_promoted += 1;
            }
            InsertResult::SkippedDuplicate => {
                report.skipped_already_promoted += 1;
            }
            InsertResult::FailedOther => {
                report.insert_failures += 1;
            }
        }
    }

    info!(
        plan_id = %plan.id,
        considered = report.assertions_considered,
        promoted = report.groups_promoted,
        skipped = report.skipped_already_promoted,
        failed = report.insert_failures,
        ?now,
        "consensus promotion pass complete"
    );

    report
}

// ---------------------------------------------------------------------------
// Session 82 — authoritative pre-pass (ADR 0004 pathway 1)
// ---------------------------------------------------------------------------

/// Walk `assertions` and promote any whose claimant matches an entry
/// in `registry` at N=1. Returns the auth-side `PromoteReport` and a
/// Vec of the assertions that did NOT match (the consensus pass runs
/// over those).
///
/// **Why split.** Authoritative promotion is "this claim is dispositive
/// by configuration, promote immediately." Consensus is "this claim
/// needs corroboration from N independent claimants." Mixing them in
/// one pass would either lose the auth-N=1 semantics (auth would have
/// to wait for quorum) or surface confusing reports (a single USGS
/// row producing both an auth-promoted record AND a consensus group
/// of size 1 below the quorum bar). Splitting keeps the audit shape
/// clean.
///
/// **Idempotency.** Auth promotion shares the consensus pathway's
/// content-derived dedup_key (`promotion:{content_hash}:{subject_hash}`).
/// A claim that was already consensus-promoted on a prior run skips
/// here on the same key; a claim that auth-promoted on a prior run
/// then later met quorum doesn't double-promote either — the dedup
/// key collides.
///
/// **Empty registry shortcut.** When `registry.is_empty()` the
/// function returns an empty report + the full input slice cloned-by-
/// reference into the remaining Vec. No iteration of the registry per
/// Assertion happens.
fn promote_authoritative_pass(
    store: &Store,
    plan: &ResearchPlan,
    assertions: &[Assertion],
    registry: &AuthorityRegistry,
) -> (PromoteReport, Vec<Assertion>) {
    let mut report = PromoteReport {
        assertions_considered: assertions.len() as u32,
        ..PromoteReport::default()
    };
    if registry.is_empty() {
        // Hot-path: skip iteration when nothing's configured. Clone
        // the full slice into the remaining Vec; clone cost is small
        // for the typical Assertion shape (Envelope is a few hundred
        // bytes; the AssertedContent payload is the dominant cost
        // and stays bounded by the per-Document extraction caps).
        return (report, assertions.to_vec());
    }

    let mut remaining: Vec<Assertion> = Vec::with_capacity(assertions.len());
    let now = Utc::now();

    for a in assertions {
        if !registry.matches(a) {
            remaining.push(a.clone());
            continue;
        }

        // Auth-pathway match. Build a promoted record with a single-
        // claimant `derived_from` chain using DerivationRole::Promotion
        // (the ADR 0004 role for the authoritative path; the consensus
        // path uses ConsensusSupport).
        let dedup_key = format!(
            "promotion:{}:{}",
            content_hash_for(&a.content),
            subject_hash_for(&a.envelope),
        );
        let supports = vec![DerivedFrom {
            record_id: a.id,
            role: DerivationRole::Promotion,
        }];
        let confidence = a.envelope.confidence;

        // Session 82 — explicit pre-insert idempotency check (same
        // rationale as the consensus path above).
        let outcome = if dedup_already_promoted(store, &a.content, &dedup_key) {
            info!(
                dedup_key = %dedup_key,
                "authoritative promote skipped — record with this dedup_key already exists"
            );
            InsertResult::SkippedDuplicate
        } else {
            match &a.content {
                AssertedContent::Observation(c) => {
                    let obs = build_authoritative_observation(
                        plan,
                        a,
                        c,
                        &supports,
                        confidence,
                        &dedup_key,
                    );
                    match store.insert_observation(&obs) {
                        Ok(()) => {
                            report.observations_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::Event(c) => {
                    let ev = build_authoritative_event(
                        plan,
                        a,
                        c,
                        &supports,
                        confidence,
                        &dedup_key,
                    );
                    match store.insert_event(&ev) {
                        Ok(()) => {
                            report.events_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::Relation(c) => {
                    let rel = build_authoritative_relation(
                        plan,
                        a,
                        c,
                        &supports,
                        confidence,
                        &dedup_key,
                    );
                    match store.insert_relation(&rel) {
                        Ok(()) => {
                            report.relations_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
                AssertedContent::EntityAttribute(c) => {
                    let attr = build_authoritative_entity_attribute(
                        plan,
                        a,
                        c,
                        &supports,
                        confidence,
                        &dedup_key,
                    );
                    match store.insert_assertion(&attr) {
                        Ok(()) => {
                            report.entity_attributes_emitted += 1;
                            InsertResult::Inserted
                        }
                        Err(e) => classify_insert_error(&dedup_key, e),
                    }
                }
            }
        };

        match outcome {
            InsertResult::Inserted => {
                report.authoritative_promoted += 1;
            }
            InsertResult::SkippedDuplicate => {
                report.skipped_already_promoted += 1;
                // Already promoted on a prior run — exclude from the
                // consensus pass so we don't get a "skipped" log line
                // a second time on the same key.
            }
            InsertResult::FailedOther => {
                report.insert_failures += 1;
                // Insert failure for non-duplicate reasons. Surface it
                // to the consensus pass so a future retry might still
                // succeed via quorum. The warn log already named it.
                remaining.push(a.clone());
            }
        }
    }

    info!(
        plan_id = %plan.id,
        considered = report.assertions_considered,
        authoritative = report.authoritative_promoted,
        skipped = report.skipped_already_promoted,
        failed = report.insert_failures,
        remaining_for_consensus = remaining.len(),
        ?now,
        "authoritative promotion pass complete"
    );

    (report, remaining)
}

/// Merge a consensus-pass report into an auth-pass report. The auth
/// pass populated the per-shape emitted counters + `authoritative_promoted`;
/// the consensus pass adds its own emitted counters + `groups_promoted`
/// + `skipped_already_promoted` + `insert_failures`.
/// `assertions_considered` matches the original input — both passes
/// see the same total; we keep the auth-pass value (which is the input
/// slice length).
fn merge_consensus_into(report: &mut PromoteReport, consensus: PromoteReport) {
    report.groups_promoted = report.groups_promoted.saturating_add(consensus.groups_promoted);
    report.skipped_already_promoted = report
        .skipped_already_promoted
        .saturating_add(consensus.skipped_already_promoted);
    report.observations_emitted = report
        .observations_emitted
        .saturating_add(consensus.observations_emitted);
    report.events_emitted = report.events_emitted.saturating_add(consensus.events_emitted);
    report.relations_emitted = report
        .relations_emitted
        .saturating_add(consensus.relations_emitted);
    report.entity_attributes_emitted = report
        .entity_attributes_emitted
        .saturating_add(consensus.entity_attributes_emitted);
    report.insert_failures = report.insert_failures.saturating_add(consensus.insert_failures);
    // authoritative_promoted stays from the auth pass; consensus has none.
    // assertions_considered: keep auth-pass value (= input size).
}

// Authoritative-pathway record builders. Mirror the consensus
// builders but use `provenance.source_id = "derived#authoritative"`
// + `tags = ["authoritative_promotion"]` + the auth pathway's
// `DerivationRole::Promotion` (vs consensus's `ConsensusSupport`).

fn authoritative_provenance(supports: &[DerivedFrom]) -> Provenance {
    Provenance {
        source_id: "derived#authoritative".into(),
        source_url: None,
        source_published_at: None,
        license: "derived".into(),
        derived_from: supports.to_vec(),
    }
}

fn authoritative_envelope(
    plan: &ResearchPlan,
    representative: &Assertion,
    supports: &[DerivedFrom],
    confidence: Confidence,
) -> Envelope {
    Envelope {
        provenance: authoritative_provenance(supports),
        subjects: representative.envelope.subjects.clone(),
        tags: vec!["authoritative_promotion".into()],
        valid_at: representative.envelope.valid_at,
        observed_at: Utc::now(),
        confidence,
    }
    .with_plan_topics(plan)
}

fn build_authoritative_observation(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &ObservationContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Observation {
    let envelope = authoritative_envelope(plan, representative, supports, confidence);
    Observation {
        id: Uuid::now_v7(),
        dedup_key: Some(dedup_key.to_string()),
        envelope,
        content: content.clone(),
    }
}

fn build_authoritative_event(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &EventContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Event {
    let envelope = authoritative_envelope(plan, representative, supports, confidence);
    let mut ev = Event::new(envelope, content.clone());
    ev.dedup_key = Some(dedup_key.to_string());
    ev
}

fn build_authoritative_relation(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &RelationContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Relation {
    let envelope = authoritative_envelope(plan, representative, supports, confidence);
    let mut rel = Relation::new(envelope, content.clone());
    rel.dedup_key = Some(dedup_key.to_string());
    rel
}

fn build_authoritative_entity_attribute(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &EntityAttributeContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Assertion {
    use situation_room_core::vocab::{EntityId, Stance};
    let envelope = authoritative_envelope(plan, representative, supports, confidence);
    let claimant = EntityId::new("agency:authoritative")
        .expect("static EntityId `agency:authoritative` must parse");
    let mut a = Assertion::new(
        claimant,
        Stance::Asserted,
        AssertedContent::EntityAttribute(content.clone()),
        envelope,
    );
    a.dedup_key = Some(dedup_key.to_string());
    a
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/// The compound key consensus-grouping uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    content_hash: String,
    subject_hash: String,
}

/// Group assertions by `(content_hash, subject_hash)`. Returns a
/// `BTreeMap` so iteration order is deterministic across runs — same
/// dedup keys produced in the same order makes test assertions
/// stable.
fn group_assertions_for_consensus<'a>(
    assertions: &'a [Assertion],
) -> BTreeMap<GroupKey, Vec<&'a Assertion>> {
    let mut out: BTreeMap<GroupKey, Vec<&'a Assertion>> = BTreeMap::new();
    for a in assertions {
        let key = GroupKey {
            content_hash: content_hash_for(&a.content),
            subject_hash: subject_hash_for(&a.envelope),
        };
        out.entry(key).or_default().push(a);
    }
    out
}

/// Content-derived hash. Canonical-JSON serialization → SHA-256 →
/// truncate to 128 bits → render as a 32-char lowercase hex string.
/// Session 84 — ADR 0021 amendment 1. Replaced the original
/// `(DefaultHasher, DefaultHasher-with-salt)` 128-bit pair with
/// SHA-256-truncated-to-128 once the `sha2` workspace dep landed.
/// The 32-char hex shape is byte-for-byte compatible with the
/// pre-swap output shape so downstream `dedup_key` columns and the
/// `promotion:{content_hash}:{subject_hash}` format keep working;
/// only the hash values themselves change.
///
/// `serde_json::to_value` produces a deterministic shape but
/// `serde_json::to_string` does NOT sort object keys, so we
/// canonicalise via a recursive normalisation before hashing so the
/// same input produces the same hash regardless of serializer
/// implementation detail.
pub fn content_hash_for(content: &AssertedContent) -> String {
    let value = serde_json::to_value(content).unwrap_or(serde_json::Value::Null);
    let canon = canonicalize_json(&value);
    let bytes = serde_json::to_vec(&canon).unwrap_or_default();
    hex128(&bytes)
}

/// Subject-derived hash. Combines sorted topics + sorted entity
/// strings into a stable byte sequence and 128-bit hashes. Order is
/// incidental in `Subjects` so we sort before hashing.
pub fn subject_hash_for(env: &Envelope) -> String {
    let mut topics: Vec<&str> = env.subjects.topics.iter().map(|t| t.as_str()).collect();
    topics.sort_unstable();
    let mut entities: Vec<&str> = env.subjects.entities.iter().map(|e| e.as_str()).collect();
    entities.sort_unstable();

    let mut buf: Vec<u8> = Vec::new();
    for t in &topics {
        buf.extend_from_slice(b"t:");
        buf.extend_from_slice(t.as_bytes());
        buf.push(b'\n');
    }
    for e in &entities {
        buf.extend_from_slice(b"e:");
        buf.extend_from_slice(e.as_bytes());
        buf.push(b'\n');
    }
    hex128(&buf)
}

/// 128-bit fingerprint of `bytes`, rendered as 32 lowercase hex
/// chars. SHA-256 over the input, truncated to the first 16 bytes.
/// The 128-bit truncation matches the pre-Session-84 output width
/// exactly (same column shape on disk); SHA-256 over the same input
/// is collision-resistant up to a ~2^64 birthday bound on the
/// truncated digest, with no published shortcut better than that.
///
/// Why truncate. The dedup column doesn't need full 256-bit
/// pre-image resistance — it's a "did we already promote this exact
/// claim?" key, not a cryptographic identifier. 128 bits keeps the
/// per-row storage cost identical to the pre-swap shape (operators
/// can run the pre- and post-swap binaries against the same DB
/// without column-width fights) and is well clear of any plausible
/// collision rate the consensus pipeline could produce.
fn hex128(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    // First 16 bytes → 32 hex chars. The slice is fixed-size from the
    // GenericArray output of finalize() so the formatter cost is
    // bounded; we render manually for the 32-char-exact contract
    // rather than going through `hex::encode` (no new dep).
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Canonicalise a `serde_json::Value`: sort object keys recursively
/// so the byte representation is invariant under key-order shuffles
/// inside HashMap-backed serialisers.
fn canonicalize_json(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(String, serde_json::Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_json(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalize_json).collect())
        }
        _ => v.clone(),
    }
}

fn avg_confidence(group: &[&Assertion]) -> Confidence {
    if group.is_empty() {
        return Confidence::ZERO;
    }
    let total: f32 = group.iter().map(|a| a.envelope.confidence.value()).sum();
    Confidence::clamp(total / (group.len() as f32))
}

// ---------------------------------------------------------------------------
// Insert-outcome classification — distinguish duplicate-key from real failure
// ---------------------------------------------------------------------------

enum InsertResult {
    Inserted,
    SkippedDuplicate,
    FailedOther,
}

/// Session 82 — pre-insert idempotency check. The dedup_key column
/// on the per-shape tables (`observations`, `events`, `relations`,
/// `assertions`) is indexed but NOT marked UNIQUE in the migration
/// schema, so DuckDB does not reject a duplicate insert on its own.
/// ADR 0021's "rejected on UNIQUE" framing therefore requires an
/// explicit existence check at the application layer — this helper
/// is that check.
///
/// Returns `true` iff a record with the given dedup_key already
/// exists in the per-shape table. On storage-layer error, returns
/// `false` and warn-logs — falling through to the insert attempt is
/// the safer fault posture (an extra row is recoverable; refusing to
/// promote at all on a transient read error is not).
fn dedup_already_promoted(
    store: &Store,
    content: &AssertedContent,
    dedup_key: &str,
) -> bool {
    let result = match content {
        AssertedContent::Observation(_) => store.observation_exists_by_dedup_key(dedup_key),
        AssertedContent::Event(_) => store.event_exists_by_dedup_key(dedup_key),
        AssertedContent::Relation(_) => store.relation_exists_by_dedup_key(dedup_key),
        AssertedContent::EntityAttribute(_) => store.assertion_exists_by_dedup_key(dedup_key),
    };
    match result {
        Ok(b) => b,
        Err(e) => {
            warn!(
                dedup_key = %dedup_key,
                error = %e,
                "exists-by-dedup-key check failed; will attempt insert and let any UNIQUE-violation classifier route the outcome"
            );
            false
        }
    }
}

/// DuckDB surfaces a UNIQUE-constraint violation as a string-ish
/// error. We classify against the substring "dedup_key" / "duplicate"
/// / "unique" so the per-row-skip case doesn't show up as a runtime
/// failure on operator dashboards. With the Session-82 pre-insert
/// check above this path is now a fallback for the cases where the
/// existence query failed silently — but kept in place for defence
/// in depth.
///
/// This is intentionally lenient: any error mentioning duplication
/// counts as "already promoted." If a future migration changes the
/// constraint name, the classifier degrades to "real failure" — the
/// promote stage stays safe (records aren't double-inserted, errors
/// surface in logs) but the report's `skipped_already_promoted`
/// undercount until the substring is updated.
fn classify_insert_error(
    dedup_key: &str,
    e: situation_room_storage::StorageError,
) -> InsertResult {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("duplicate") || msg.contains("unique") || msg.contains("dedup_key") {
        info!(
            dedup_key = %dedup_key,
            "skipping consensus promotion — already promoted on a prior run"
        );
        InsertResult::SkippedDuplicate
    } else {
        warn!(
            dedup_key = %dedup_key,
            error = %e,
            "failed to persist consensus-promoted record; continuing with the rest of the batch"
        );
        InsertResult::FailedOther
    }
}

// ---------------------------------------------------------------------------
// Promoted-record builders
// ---------------------------------------------------------------------------

fn promoted_provenance(supports: &[DerivedFrom]) -> Provenance {
    Provenance {
        source_id: "derived#consensus".into(),
        source_url: None,
        source_published_at: None,
        license: "derived".into(),
        derived_from: supports.to_vec(),
    }
}

fn promoted_envelope(
    plan: &ResearchPlan,
    representative: &Assertion,
    supports: &[DerivedFrom],
    confidence: Confidence,
) -> Envelope {
    Envelope {
        provenance: promoted_provenance(supports),
        subjects: representative.envelope.subjects.clone(),
        tags: vec!["consensus_promotion".into()],
        valid_at: representative.envelope.valid_at,
        observed_at: Utc::now(),
        confidence,
    }
    .with_plan_topics(plan)
}

trait EnvelopeExt {
    fn with_plan_topics(self, plan: &ResearchPlan) -> Self;
}
impl EnvelopeExt for Envelope {
    fn with_plan_topics(mut self, plan: &ResearchPlan) -> Self {
        for t in &plan.topic_tags {
            if !self.subjects.topics.contains(t) {
                self.subjects.topics.push(t.clone());
            }
        }
        self
    }
}

fn build_promoted_observation(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &ObservationContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Observation {
    let envelope = promoted_envelope(plan, representative, supports, confidence);
    Observation {
        id: Uuid::now_v7(),
        dedup_key: Some(dedup_key.to_string()),
        envelope,
        content: content.clone(),
    }
}

fn build_promoted_event(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &EventContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Event {
    let envelope = promoted_envelope(plan, representative, supports, confidence);
    let mut ev = Event::new(envelope, content.clone());
    ev.dedup_key = Some(dedup_key.to_string());
    ev
}

fn build_promoted_relation(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &RelationContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Relation {
    let envelope = promoted_envelope(plan, representative, supports, confidence);
    let mut rel = Relation::new(envelope, content.clone());
    rel.dedup_key = Some(dedup_key.to_string());
    rel
}

fn build_promoted_entity_attribute(
    plan: &ResearchPlan,
    representative: &Assertion,
    content: &EntityAttributeContent,
    supports: &[DerivedFrom],
    confidence: Confidence,
    dedup_key: &str,
) -> Assertion {
    use situation_room_core::vocab::{EntityId, Stance};
    let envelope = promoted_envelope(plan, representative, supports, confidence);
    let claimant = EntityId::new("agency:consensus")
        .expect("static EntityId `agency:consensus` must parse");
    let mut a = Assertion::new(
        claimant,
        Stance::Asserted,
        AssertedContent::EntityAttribute(content.clone()),
        envelope,
    );
    a.dedup_key = Some(dedup_key.to_string());
    a
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use situation_room_core::schema::content::{
        AttributeValue, ObservationContent, ObservationPeriod,
    };
    use situation_room_core::schema::envelope::Subjects;
    use situation_room_core::vocab::{EntityId, Stance, Topic, Unit};

    fn sample_envelope(claimant_topic: &str, confidence: f32) -> Envelope {
        Envelope {
            provenance: Provenance {
                source_id: format!("{claimant_topic}#recipe:11111111-1111-7111-8111-111111111111@v1"),
                source_url: None,
                source_published_at: None,
                license: "extracted".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("lithium").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
            confidence: Confidence::clamp(confidence),
        }
    }

    fn obs_assertion(claimant: &str, value: f64, conf: f32) -> Assertion {
        Assertion::new(
            EntityId::new(claimant).unwrap(),
            Stance::Reported,
            AssertedContent::Observation(ObservationContent {
                metric: "production".into(),
                value,
                unit: Unit::new("t").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Annual,
                geometry: None,
            }),
            sample_envelope(claimant, conf),
        )
    }

    fn plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium supply".into(),
            interpretation: "test".into(),
            topic_tags: vec![Topic::new("lithium").unwrap()],
            geographic_scope: vec![],
            historical_window_days: 730,
            expectations: crate::research::RecordExpectations::default(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn hex128_is_exactly_32_lowercase_hex_chars() {
        // Session 84 — ADR 0021 amendment 1. Pin the output shape so a
        // future swap that changes the digest function still has to
        // honour the 32-char hex contract (the dedup_key column shape
        // and the `promotion:{a}:{b}` format both depend on it).
        let h = hex128(b"hello");
        assert_eq!(h.len(), 32, "hex128 must render exactly 32 chars");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex128 must render lowercase hex digits only, got `{h}`"
        );
        // Specific value pin: SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        // First 16 bytes → 2cf24dba5fb0a30e26e83b2ac5b9e29e.
        assert_eq!(h, "2cf24dba5fb0a30e26e83b2ac5b9e29e");
    }

    #[test]
    fn hex128_different_inputs_produce_different_outputs() {
        assert_ne!(hex128(b"hello"), hex128(b"hello world"));
        assert_ne!(hex128(b"hello"), hex128(b"Hello"));
        // Empty input is well-defined too (SHA-256("") =
        // e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        // → first 16 = e3b0c44298fc1c149afbf4c8996fb924).
        assert_eq!(hex128(b""), "e3b0c44298fc1c149afbf4c8996fb924");
    }

    #[test]
    fn content_hash_is_stable_for_equal_content() {
        let a = AssertedContent::Observation(ObservationContent {
            metric: "production".into(),
            value: 142_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: None,
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        });
        let b = a.clone();
        assert_eq!(content_hash_for(&a), content_hash_for(&b));
    }

    #[test]
    fn content_hash_differs_on_value_change() {
        let a = AssertedContent::Observation(ObservationContent {
            metric: "production".into(),
            value: 142_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: None,
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        });
        let b = AssertedContent::Observation(ObservationContent {
            metric: "production".into(),
            value: 143_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: None,
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        });
        assert_ne!(content_hash_for(&a), content_hash_for(&b));
    }

    #[test]
    fn subject_hash_is_topic_order_invariant() {
        let mut env_a = sample_envelope("agency:a", 0.9);
        let mut env_b = sample_envelope("agency:b", 0.9);
        env_a.subjects.topics = vec![
            Topic::new("lithium").unwrap(),
            Topic::new("battery_supply_chain").unwrap(),
        ];
        env_b.subjects.topics = vec![
            Topic::new("battery_supply_chain").unwrap(),
            Topic::new("lithium").unwrap(),
        ];
        assert_eq!(subject_hash_for(&env_a), subject_hash_for(&env_b));
    }

    #[test]
    fn grouping_clusters_by_content_and_subject() {
        // Three claimants, same content+subject — one group.
        let group = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.85),
            obs_assertion("agency:argus", 142_000.0, 0.7),
        ];
        let grouped = group_assertions_for_consensus(&group);
        assert_eq!(grouped.len(), 1);
        let only = grouped.values().next().unwrap();
        assert_eq!(only.len(), 3);
    }

    #[test]
    fn grouping_separates_distinct_values() {
        let group = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 143_000.0, 0.85),
        ];
        let grouped = group_assertions_for_consensus(&group);
        assert_eq!(grouped.len(), 2, "distinct values produce distinct groups");
    }

    #[test]
    fn quorum_not_met_when_one_claimant_repeats() {
        // Same claimant on three assertions counts as ONE
        // independent claimant — quorum is on distinct EntityIds,
        // not raw row counts (ADR 0004's "independent" definition).
        let assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:reuters", 142_000.0, 0.8),
        ];
        let grouped = group_assertions_for_consensus(&assertions);
        let only = grouped.values().next().unwrap();
        let distinct: BTreeSet<&str> =
            only.iter().map(|a| a.claimant.as_str()).collect();
        assert_eq!(distinct.len(), 1);
        // The promote pass would reject this group; we verify the
        // quorum predicate at the boundary so the test isolates the
        // independence rule from store I/O.
        assert!((distinct.len() as u32) < PromoteConfig::default().min_independent_claimants);
    }

    #[test]
    fn quorum_met_with_three_distinct_claimants() {
        let assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.85),
            obs_assertion("agency:argus", 142_000.0, 0.75),
        ];
        let grouped = group_assertions_for_consensus(&assertions);
        let only = grouped.values().next().unwrap();
        let distinct: BTreeSet<&str> =
            only.iter().map(|a| a.claimant.as_str()).collect();
        assert!((distinct.len() as u32) >= PromoteConfig::default().min_independent_claimants);
    }

    #[test]
    fn promoted_observation_carries_consensus_provenance() {
        let plan = plan();
        let group_assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.85),
            obs_assertion("agency:argus", 142_000.0, 0.75),
        ];
        let supports: Vec<DerivedFrom> = group_assertions
            .iter()
            .map(|a| DerivedFrom {
                record_id: a.id,
                role: DerivationRole::ConsensusSupport,
            })
            .collect();
        let representative = &group_assertions[0];
        let content = match &representative.content {
            AssertedContent::Observation(c) => c.clone(),
            _ => unreachable!(),
        };
        let obs = build_promoted_observation(
            &plan,
            representative,
            &content,
            &supports,
            Confidence::clamp(0.8),
            "promotion:abc:def",
        );

        assert_eq!(obs.envelope.provenance.source_id, "derived#consensus");
        assert_eq!(obs.envelope.provenance.license, "derived");
        assert_eq!(obs.envelope.provenance.derived_from.len(), 3);
        assert!(obs
            .envelope
            .provenance
            .derived_from
            .iter()
            .all(|d| matches!(d.role, DerivationRole::ConsensusSupport)));
        assert_eq!(obs.dedup_key.as_deref(), Some("promotion:abc:def"));
        // Plan topic propagates onto the promoted envelope so the
        // records-for-plan join surfaces it under the originating plan.
        assert!(obs
            .envelope
            .subjects
            .topics
            .iter()
            .any(|t| t.as_str() == "lithium"));
        // Promotion tag attached for dashboard distinguishability.
        assert!(obs.envelope.tags.contains(&"consensus_promotion".into()));
    }

    #[test]
    fn entity_attribute_promotion_synthesises_consensus_claimant() {
        let plan = plan();
        let env = sample_envelope("agency:doc", 0.85);
        let content = AssertedContent::EntityAttribute(EntityAttributeContent {
            entity_id: EntityId::new("company:tsla").unwrap(),
            key: "employee_count".into(),
            value: AttributeValue::Number {
                value: 140_473.0,
                unit: Some(Unit::new("persons").unwrap()),
            },
        });
        let a = Assertion::new(
            EntityId::new("agency:reuters").unwrap(),
            Stance::Reported,
            content.clone(),
            env,
        );
        let supports = vec![DerivedFrom {
            record_id: a.id,
            role: DerivationRole::ConsensusSupport,
        }];
        let inner = match &content {
            AssertedContent::EntityAttribute(c) => c.clone(),
            _ => unreachable!(),
        };
        let promoted = build_promoted_entity_attribute(
            &plan,
            &a,
            &inner,
            &supports,
            Confidence::clamp(0.9),
            "promotion:xyz:abc",
        );
        assert_eq!(promoted.claimant.as_str(), "agency:consensus");
        assert!(matches!(promoted.stance, Stance::Asserted));
        assert_eq!(promoted.dedup_key.as_deref(), Some("promotion:xyz:abc"));
    }

    #[test]
    fn empty_assertions_produces_zero_report() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let report = promote_consensus_from_assertions(
            &store,
            &plan(),
            &[],
            &PromoteConfig::default(),
        );
        assert_eq!(report.assertions_considered, 0);
        assert_eq!(report.groups_promoted, 0);
        assert_eq!(report.observations_emitted, 0);
    }

    #[test]
    fn promote_end_to_end_emits_one_observation_per_group() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        // Three independent claimants, same value, same subject → one
        // promoted Observation. Two more claimants on a different
        // value form a second group; second group has only two
        // claimants so does not meet quorum.
        let group_a = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.85),
            obs_assertion("agency:argus", 142_000.0, 0.75),
            obs_assertion("agency:reuters", 999_999.0, 0.5),
            obs_assertion("agency:bloomberg", 999_999.0, 0.5),
        ];

        let report = promote_consensus_from_assertions(
            &store,
            &plan,
            &group_a,
            &PromoteConfig::default(),
        );
        assert_eq!(report.assertions_considered, 5);
        assert_eq!(report.observations_emitted, 1);
        assert_eq!(report.groups_promoted, 1);
        assert_eq!(report.insert_failures, 0);

        // Second pass: idempotent — the same group's dedup_key collides
        // and the row is skipped (not double-promoted).
        let report2 = promote_consensus_from_assertions(
            &store,
            &plan,
            &group_a,
            &PromoteConfig::default(),
        );
        assert_eq!(report2.observations_emitted, 0);
        assert_eq!(report2.skipped_already_promoted, 1);
    }

    // -----------------------------------------------------------------
    // Session 82 — authoritative pass (ADR 0004 pathway 1)
    // -----------------------------------------------------------------

    use crate::authoritative::{AuthorityEntry, AuthorityRegistry};

    fn auth_cfg(entries: Vec<AuthorityEntry>) -> PromoteConfig {
        PromoteConfig {
            min_independent_claimants: 3,
            authoritative: AuthorityRegistry::from_entries(entries),
        }
    }

    #[test]
    fn auth_pass_promotes_single_claimant_when_registry_matches() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: None,
        }]);

        // Single USGS claimant — would fail consensus quorum (N=3) but
        // matches the authoritative registry, so it promotes at N=1.
        let assertions = vec![obs_assertion("agency:usgs_mcs", 142_000.0, 0.85)];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);
        assert_eq!(report.assertions_considered, 1);
        assert_eq!(report.authoritative_promoted, 1);
        assert_eq!(report.observations_emitted, 1);
        assert_eq!(report.groups_promoted, 0, "consensus path produced nothing");
        assert_eq!(report.skipped_already_promoted, 0);
    }

    #[test]
    fn auth_pass_with_empty_registry_falls_back_to_consensus_only() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = PromoteConfig::default(); // empty registry
        let assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.85),
            obs_assertion("agency:argus", 142_000.0, 0.75),
        ];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);
        assert_eq!(report.authoritative_promoted, 0);
        assert_eq!(report.observations_emitted, 1);
        assert_eq!(report.groups_promoted, 1);
    }

    #[test]
    fn auth_pass_idempotent_on_rerun() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: None,
        }]);

        let assertions = vec![obs_assertion("agency:usgs_mcs", 142_000.0, 0.85)];
        let r1 = promote_from_assertions(&store, &plan, &assertions, &cfg);
        assert_eq!(r1.authoritative_promoted, 1);
        assert_eq!(r1.observations_emitted, 1);

        let r2 = promote_from_assertions(&store, &plan, &assertions, &cfg);
        assert_eq!(r2.authoritative_promoted, 0);
        assert_eq!(r2.observations_emitted, 0);
        assert_eq!(r2.skipped_already_promoted, 1);
    }

    #[test]
    fn auth_pass_excludes_promoted_assertions_from_consensus_pass() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: None,
        }]);

        // Three assertions: one matches the registry (auth-promoted),
        // two non-matching (which alone wouldn't meet N=3 quorum).
        let assertions = vec![
            obs_assertion("agency:usgs_mcs", 142_000.0, 0.85),
            obs_assertion("agency:reuters", 142_000.0, 0.8),
            obs_assertion("agency:bloomberg", 142_000.0, 0.7),
        ];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);
        // Auth pass got one; consensus pass saw two remaining (below
        // N=3 quorum) and emitted none. But the dedup_key the
        // consensus pass would have used MATCHES the auth-promoted
        // record's key — so a consensus group with the right
        // claimants would either be skipped-already-promoted OR
        // (depending on quorum reaching) succeed. With only 2 non-auth
        // claimants in this fixture, consensus naturally doesn't reach
        // quorum; the test pins this case.
        assert_eq!(report.assertions_considered, 3);
        assert_eq!(report.authoritative_promoted, 1);
        assert_eq!(report.groups_promoted, 0);
        // Observation count = auth path's emission.
        assert_eq!(report.observations_emitted, 1);
    }

    #[test]
    fn auth_promoted_observation_carries_authoritative_provenance() {
        let plan = plan();
        let supports = vec![DerivedFrom {
            record_id: Uuid::now_v7(),
            role: DerivationRole::Promotion,
        }];
        let representative = obs_assertion("agency:usgs_mcs", 142_000.0, 0.9);
        let content = match &representative.content {
            AssertedContent::Observation(c) => c.clone(),
            _ => unreachable!(),
        };
        let obs = build_authoritative_observation(
            &plan,
            &representative,
            &content,
            &supports,
            Confidence::clamp(0.9),
            "promotion:abc:def",
        );
        assert_eq!(obs.envelope.provenance.source_id, "derived#authoritative");
        assert_eq!(obs.envelope.provenance.license, "derived");
        assert_eq!(obs.envelope.provenance.derived_from.len(), 1);
        assert!(matches!(
            obs.envelope.provenance.derived_from[0].role,
            DerivationRole::Promotion
        ));
        assert!(obs.envelope.tags.contains(&"authoritative_promotion".into()));
    }

    // -----------------------------------------------------------------
    // Session 84 — per-claimant consensus quorum override
    // -----------------------------------------------------------------

    #[test]
    fn consensus_quorum_override_lowers_bar_for_matching_groups() {
        // Registry entry sets consensus_quorum=2 for Reuters
        // production claims. Two distinct claimants (Reuters +
        // Bloomberg) form a group; with default global N=3 they'd
        // miss quorum, but the Reuters entry's override lowers the
        // bar to N=2 for groups containing a Reuters Assertion.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![AuthorityEntry {
            source_id: "reuters".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: Some(2),
        }]);

        let assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.85),
            obs_assertion("agency:bloomberg", 142_000.0, 0.8),
        ];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);

        // Auth pass: Reuters entry opts out of fast-track via
        // consensus_quorum=2, so authoritative_promoted stays 0.
        assert_eq!(report.authoritative_promoted, 0);
        // Consensus pass: 2 distinct claimants meet the lowered N=2
        // quorum → one observation promoted.
        assert_eq!(
            report.observations_emitted, 1,
            "consensus_quorum=2 should let the 2-claimant group promote"
        );
        assert_eq!(report.groups_promoted, 1);
    }

    #[test]
    fn consensus_quorum_override_does_not_promote_below_override() {
        // Registry entry sets consensus_quorum=2; only one matching
        // claimant present. The auth pass opts out (consensus_quorum
        // >= 2), and the consensus pass sees a single-claimant group
        // — below even the lowered N=2 bar. Nothing promotes.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![AuthorityEntry {
            source_id: "reuters".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: Some(2),
        }]);

        let assertions = vec![obs_assertion("agency:reuters", 142_000.0, 0.85)];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);

        assert_eq!(report.authoritative_promoted, 0);
        assert_eq!(report.observations_emitted, 0);
        assert_eq!(report.groups_promoted, 0);
    }

    #[test]
    fn consensus_quorum_override_min_wins_when_multiple_entries_match() {
        // Two overrides apply: Reuters consensus_quorum=3, Bloomberg
        // consensus_quorum=2 (under a metric-less entry). The group
        // {Reuters, Bloomberg} resolves to min(3, 2, cfg_default=3) =
        // 2. Two claimants meet it → promotion.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = plan();

        let cfg = auth_cfg(vec![
            AuthorityEntry {
                source_id: "reuters".into(),
                metric: None,
                topic: None,
                consensus_quorum: Some(3),
            },
            AuthorityEntry {
                source_id: "bloomberg".into(),
                metric: None,
                topic: None,
                consensus_quorum: Some(2),
            },
        ]);
        let assertions = vec![
            obs_assertion("agency:reuters", 142_000.0, 0.85),
            obs_assertion("agency:bloomberg", 142_000.0, 0.8),
        ];
        let report = promote_from_assertions(&store, &plan, &assertions, &cfg);
        assert_eq!(report.authoritative_promoted, 0);
        assert_eq!(report.observations_emitted, 1);
        assert_eq!(report.groups_promoted, 1);
    }
}
