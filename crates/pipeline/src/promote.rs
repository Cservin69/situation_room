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
//! `content_hash` is SHA-256 over the canonical JSON serialization
//! of the `AssertedContent` value. `subject_hash` is SHA-256 over the
//! sorted topic strings on the Assertion's envelope — sorted because
//! `Subjects::topics` order is incidental.
//!
//! # Authoritative pathway — deferred
//!
//! ADR 0004 describes two pathways: consensus (this module) and
//! authoritative (config-driven fast-track). Authoritative promotion
//! depends on `config/authoritative.toml` (Phase 3 work) and is not
//! shipped in Session 81. Consensus is enough to demonstrate the
//! cross-source-dedup property today; the authoritative branch lands
//! when the operator decides which sources are dispositive for which
//! content kinds.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use situation_room_core::schema::content::{
    AssertedContent, EntityAttributeContent, EventContent, ObservationContent, RelationContent,
};
use situation_room_core::schema::envelope::{DerivationRole, DerivedFrom, Envelope, Provenance};
use situation_room_core::schema::records::{Assertion, Event, Observation, Relation};
use situation_room_core::vocab::Confidence;
use situation_room_storage::Store;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::research::ResearchPlan;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Configuration for one consensus-promotion run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteConfig {
    /// Minimum number of *distinct* claimants that must agree on a
    /// compatible claim before it promotes. ADR 0004 default is 3.
    pub min_independent_claimants: u32,
}

impl Default for PromoteConfig {
    fn default() -> Self {
        Self {
            min_independent_claimants: 3,
        }
    }
}

/// Summary of one consensus-promotion pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteReport {
    /// How many Assertion rows the pass considered.
    pub assertions_considered: u32,
    /// How many distinct `(content_hash, subject_hash)` groups met
    /// quorum and produced a promoted record this run.
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

/// Run the consensus-promotion pass for one plan.
///
/// Reads every persisted `Assertion` tied to the plan (via the
/// `records_for_plan` recipe-routing join), groups them by
/// `(content_hash, subject_hash)`, promotes any group with at least
/// `cfg.min_independent_claimants` distinct claimants, and inserts
/// the resulting records. Idempotent on re-run via the
/// `promotion:{content_hash}:{subject_hash}` dedup key.
///
/// Never returns the partial-batch error shape: per-row insert
/// failures are warn-logged and counted into `insert_failures`. The
/// outer `Result` covers only "the pass couldn't start" failures
/// (storage open, the per-plan records-load query itself).
pub fn promote_consensus_for_plan(
    store: &Store,
    plan: &ResearchPlan,
    cfg: &PromoteConfig,
) -> Result<PromoteReport, PromoteError> {
    let bucket = store.records_for_plan(plan.id)?;
    let assertions = bucket.assertions;
    Ok(promote_consensus_from_assertions(store, plan, &assertions, cfg))
}

/// Pure(-ish — still writes to `Store`) helper that promotes a
/// caller-supplied slice of `Assertion` rows. Split out so tests can
/// hand a synthetic Vec without standing up the records-for-plan
/// join.
pub fn promote_consensus_from_assertions(
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
        if (distinct_claimants.len() as u32) < cfg.min_independent_claimants {
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

        let outcome = match &representative.content {
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

/// Content-derived hash. Canonical-JSON serialization → 128-bit
/// `(SipHash13, SipHash13-with-salt)` pair, rendered as a 32-char
/// lowercase hex string. ADR 0021 explains why we use two SipHash
/// runs rather than reaching for SHA-256: the workspace doesn't
/// currently take a hashing-crate dependency, and adding one for a
/// content-identity key whose adversarial-collision surface is "an
/// LLM emits 2^64 attempts to forge a dedup_key" is overkill. The
/// 128-bit pair has a collision floor on the order of 2^64 — fine
/// for a within-session dedup table that won't grow past low
/// millions of rows.
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
/// chars. Combines two `DefaultHasher` runs — the second seeded with
/// a salt — so the output space is `2^128` rather than the bare
/// `u64.to_string()` output of one `DefaultHasher` would give.
fn hex128(bytes: &[u8]) -> String {
    let a = hash_u64(bytes, 0);
    let b = hash_u64(bytes, 0x9e37_79b9_7f4a_7c15);
    format!("{a:016x}{b:016x}")
}

fn hash_u64(bytes: &[u8], salt: u64) -> u64 {
    let mut h = DefaultHasher::new();
    salt.hash(&mut h);
    bytes.hash(&mut h);
    h.finish()
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

/// DuckDB surfaces a UNIQUE-constraint violation as a string-ish
/// error. We classify against the substring "dedup_key" / "duplicate"
/// / "unique" so the per-row-skip case doesn't show up as a runtime
/// failure on operator dashboards.
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
}
