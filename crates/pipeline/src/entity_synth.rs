//! Plan-accept-time Entity exemplar materialisation (Session 76).
//!
//! The Level-1 classifier already attaches `exemplars: Vec<EntityId>` to
//! each `EntityKindExpectation` — concrete business-keys like
//! `company:tsla`, `agency:fema`, `mine:greenbushes` that name actors
//! the topic revolves around. Pre-Session-76 those exemplars were
//! dead weight: the recipe author prompt explicitly forbids
//! `entity_kind` production bindings (see `recipe_author.md` ~L1405
//! and `recipe_apply.rs::build_record` `Err` branch), pointing at a
//! "registry lookup" path that doesn't exist anywhere in the
//! pipeline. The dashboard's Entities panel therefore stayed at 0
//! system-wide, regardless of how event-rich the topic was.
//!
//! This module closes the gap by promoting each exemplar to an
//! [`Entity`] row at plan-accept time, before any fetching runs. The
//! kind comes from the expectation, the `entity_id` is the exemplar
//! itself, the canonical_name is a humanised form (prefix stripped,
//! underscores → spaces) of the slug, and the envelope carries a
//! plan-keyed `source_id` so `records_for_plan` finds the row under
//! the originating plan.
//!
//! ## Scope (Session 76)
//!
//! - **Closed-vocabulary only.** Exemplars come from the classifier's
//!   structured output — no host-specific code, no source routing.
//! - **No LLM calls.** Free path. The cost is one `INSERT` per
//!   exemplar that isn't already in the `entities` table.
//! - **Idempotent.** Re-accepting a plan, re-classifying the same
//!   topic, or seeing the same exemplar in two different plans is
//!   safe — the `entities.entity_id` UNIQUE constraint plus an
//!   upfront `get_entity_by_business_id` check keep duplicates from
//!   ever reaching the storage layer.
//! - **Non-fatal on per-exemplar failure.** A malformed exemplar
//!   string or a transient DB error during one insert must not
//!   break `accept_plan`. Failures land in
//!   [`MaterializationReport::errors`] for operator visibility; the
//!   loop keeps going.
//!
//! ## What this module does NOT do
//!
//! - Entity *attribute* records (headquarters, employee counts,
//!   etc.). Those are
//!   [`EntityAttributeContent`](situation_room_core::schema::content::EntityAttributeContent)
//!   and require a fetched source. Out of scope for Session 76.
//! - Relation materialisation. Relations need two entities and a
//!   typed link kind; the classifier emits the kind but no
//!   relation-exemplars exist on `RelationKindExpectation`.
//! - LLM-assisted entity discovery from fetched documents. That's
//!   the Phase-3 extraction layer (`pipeline::extract` is still a
//!   4-line stub).

use chrono::{DateTime, Utc};
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::Entity;
use situation_room_core::vocab::{Confidence, EntityId, Topic};
use situation_room_storage::{
    entity_tier_from_license, EntityProvenanceTier, Store, StorageError,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::research::ResearchPlan;

/// Summary of one materialisation pass over a plan's
/// `entity_kinds[*].exemplars[*]`. Returned to callers (the
/// `accept_plan` command, today) so they can log a single
/// per-plan summary line instead of one log per exemplar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationReport {
    /// Exemplars that didn't exist in `entities` yet and were
    /// inserted as fresh rows.
    pub materialized: u32,

    /// Exemplars that already had an `entities.entity_id` row
    /// (from a prior accept of this plan, or from a different plan
    /// that named the same actor) at a tier equal to or richer than
    /// SlugHumanised. The upsert is a no-op.
    pub skipped_existing: u32,

    /// Sn-100 #4 — exemplars whose existing row was at a strictly-
    /// lower tier (today: `RecipeIterator`, from Sn-97 Lever B) and
    /// got elevated to `SlugHumanised` by this pass. The refresh fires
    /// an `EntityRefreshEvent` and the row's `kind`/`canonical_name`/
    /// `license` are updated in place; the row id and entity_id stay
    /// stable so existing `record_subjects_*` / `record_derived_from`
    /// joins continue to resolve.
    pub refreshed: u32,

    /// Per-exemplar failures. Each entry is a short human-readable
    /// string `"{entity_id_attempt}: {error}"` so the operator can
    /// see what went wrong without scraping multi-line logs.
    pub errors: Vec<String>,
}

impl MaterializationReport {
    /// Total exemplars considered — sum of the four buckets.
    /// Useful for logs and tests.
    pub fn total(&self) -> u32 {
        self.materialized
            + self.skipped_existing
            + self.refreshed
            + self.errors.len() as u32
    }
}

/// Walk a plan's `entity_kinds[*].exemplars[*]` and persist each as
/// an [`Entity`] row in `store`. Idempotent; safe to call repeatedly.
///
/// The runtime path is [`crate::entity_synth`]'s only public entry
/// point. Callers should treat its `MaterializationReport` as
/// observability — never as a `Result` to propagate; per-exemplar
/// failures are captured inside the report and must not break
/// plan-accept.
pub fn materialize_entity_exemplars(
    plan: &ResearchPlan,
    store: &Store,
    accepted_at: DateTime<Utc>,
) -> MaterializationReport {
    let mut report = MaterializationReport::default();

    for kind_exp in &plan.expectations.entity_kinds {
        let kind = kind_exp.kind.trim();
        if kind.is_empty() {
            // Defensive: a classifier bug that emits empty `kind`
            // would otherwise produce kind="" entities which the
            // dashboard renders as "(unknown)" — useless and
            // confusing. Skip and log, don't fail the loop.
            warn!(
                plan_id = %plan.id,
                "entity_synth: skipping entity_kind with empty kind string"
            );
            continue;
        }

        for exemplar in &kind_exp.exemplars {
            match try_materialize_one(plan, kind, exemplar, store, accepted_at) {
                Ok(MaterializeOutcome::Inserted) => {
                    report.materialized += 1;
                    debug!(
                        plan_id = %plan.id,
                        entity_id = %exemplar.as_str(),
                        kind = %kind,
                        "entity_synth: materialised exemplar"
                    );
                }
                Ok(MaterializeOutcome::Refreshed) => {
                    report.refreshed += 1;
                    debug!(
                        plan_id = %plan.id,
                        entity_id = %exemplar.as_str(),
                        kind = %kind,
                        "entity_synth: refreshed exemplar (elevated to SlugHumanised)"
                    );
                }
                Ok(MaterializeOutcome::AlreadyExists) => {
                    report.skipped_existing += 1;
                }
                Err(e) => {
                    report.errors.push(format!("{}: {e}", exemplar.as_str()));
                    warn!(
                        plan_id = %plan.id,
                        entity_id = %exemplar.as_str(),
                        error = %e,
                        "entity_synth: per-exemplar materialisation failed; \
                         continuing with remaining exemplars"
                    );
                }
            }
        }
    }

    info!(
        plan_id = %plan.id,
        materialized = report.materialized,
        refreshed = report.refreshed,
        skipped_existing = report.skipped_existing,
        errors = report.errors.len(),
        "entity_synth: plan-accept materialisation complete"
    );

    report
}

/// Per-exemplar outcome inside the materialisation loop.
enum MaterializeOutcome {
    /// No prior row for this `entity_id`; the upsert wrote a fresh row.
    Inserted,
    /// Prior row was at a strictly-lower tier (today only
    /// `RecipeIterator`); the upsert refreshed it in place to
    /// `SlugHumanised`.
    Refreshed,
    /// Prior row was at `SlugHumanised` or higher tier; the upsert was
    /// a no-op. Mapped to `skipped_existing` for parity with the
    /// pre-Sn-100 semantic.
    AlreadyExists,
}

/// Single-exemplar attempt: snapshot pre-state, build the candidate,
/// upsert with the explicit `SlugHumanised` tier, derive the outcome
/// from the pre-state.
///
/// ## Sn-100 #4 — migrated to upsert_entity_with_tier
///
/// Pre-Sn-100 this function used existence-check-then-insert: existing
/// rows were never touched, regardless of tier. That left a hole — a
/// recipe-iterator (Lever B) row written at `RecipeIterator` tier
/// stayed at that tier even after the operator accepted a plan whose
/// exemplar names the same entity at a richer (`SlugHumanised`) tier.
///
/// Sn-100 #4 closes the hole. The explicit-tier upsert (Sn-99 #5)
/// already implements the strictly-greater-tier refresh; this call
/// site now opts in. Behavioural changes:
///
///   - **RecipeIterator → SlugHumanised refresh.** A re-accepted plan
///     whose exemplar names an entity already at `RecipeIterator` tier
///     elevates it to `SlugHumanised` (humanised slug replaces the
///     iterator scalar). One `EntityRefreshEvent` fires per refresh.
///   - **DocumentExtracted untouched.** Lever A's prose name never
///     gets clobbered by a humanised slug — the upsert's same-or-lower-
///     tier branch returns a no-op.
///   - **Same-tier no-op preserved.** Two accepts of the same plan
///     compute `SlugHumanised <= SlugHumanised`, no change.
///
/// The `AlreadyExists` outcome the report counts as `skipped_existing`
/// is preserved by inspecting the pre-upsert row's tier; the new
/// `Refreshed` outcome surfaces in the report's `refreshed` bucket so
/// the operator can see how many entities were elevated by a re-accept.
fn try_materialize_one(
    plan: &ResearchPlan,
    kind: &str,
    entity_id: &EntityId,
    store: &Store,
    accepted_at: DateTime<Utc>,
) -> Result<MaterializeOutcome, String> {
    // Snapshot pre-state. The upsert below internally repeats this
    // lookup; we duplicate it here so the outcome (Inserted /
    // Refreshed / AlreadyExists) is observable from this layer
    // without changing the storage API. The cost is one extra
    // indexed lookup on the UNIQUE `entities.entity_id`; cheap.
    let pre_tier = match store.get_entity_by_business_id(entity_id) {
        Ok(existing) => Some(entity_tier_from_license(
            &existing.envelope.provenance.license,
        )),
        Err(StorageError::NotFound(_)) => None,
        Err(other) => return Err(format!("existence check: {other}")),
    };

    let entity = build_exemplar_entity(plan, kind, entity_id, accepted_at);
    store
        .upsert_entity_with_tier(&entity, EntityProvenanceTier::SlugHumanised)
        .map_err(|e| format!("upsert: {e}"))?;

    Ok(match pre_tier {
        None => MaterializeOutcome::Inserted,
        Some(t) if t < EntityProvenanceTier::SlugHumanised => MaterializeOutcome::Refreshed,
        Some(_) => MaterializeOutcome::AlreadyExists,
    })
}

/// Construct one [`Entity`] from `(plan, kind, entity_id, accepted_at)`.
/// Pure function — no I/O, no `Store` — so the unit tests can pin the
/// envelope shape without standing up a DB.
///
/// `source_id` uses the format `"plan:{plan_id}#entity_exemplar"`.
/// `records_for_plan` (Session 76 extension) matches the
/// `plan:{plan_id}#%` prefix in addition to its existing
/// `%#recipe:{recipe_uuid}@v%` patterns, so the row routes to the
/// originating plan's dashboard view even when no recipes have run
/// yet.
pub fn build_exemplar_entity(
    plan: &ResearchPlan,
    kind: &str,
    entity_id: &EntityId,
    accepted_at: DateTime<Utc>,
) -> Entity {
    let provenance = Provenance {
        source_id: format!("plan:{}#entity_exemplar", plan.id),
        source_url: None,
        source_published_at: None,
        license: "classifier-emitted".to_string(),
        derived_from: vec![],
        selector_path: None,
        raw_bytes_excerpt: None,
    };

    let subjects = Subjects {
        entities: vec![entity_id.clone()],
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: None,
        observed_at: accepted_at,
        confidence: Confidence::ONE,
    };

    Entity::new(
        entity_id.clone(),
        kind.to_string(),
        canonical_name_from_exemplar(entity_id),
        envelope,
    )
}

/// Humanise an exemplar EntityId into a display string.
///
/// Convention from the classifier prompt: exemplars are
/// `prefix:slug_with_underscores` (e.g. `company:ibm_quantum`,
/// `mine:greenbushes`, `agency:fema`). For the dashboard's
/// Entities panel, the `canonical_name` slot wants something
/// readable — so we strip the `prefix:` (the kind is already
/// carried separately on `Entity.kind`) and convert underscores
/// to spaces. We deliberately do *not* title-case: classifier
/// slugs are sometimes acronyms (`fema`, `tsmc`, `ofac`) where
/// `Fema` reads worse than `fema`, and the dashboard already
/// renders `kind` alongside `canonical_name` for context.
///
/// Edge cases:
/// - No colon: the whole string is treated as the slug
///   (`"greenbushes"` → `"greenbushes"`).
/// - Empty slug after colon: the original string is returned
///   verbatim — better than rendering an empty cell.
/// - Multiple colons: only the **first** is treated as the
///   prefix separator (`"port:singapore:terminal_a"` →
///   `"singapore:terminal a"`).
pub fn canonical_name_from_exemplar(entity_id: &EntityId) -> String {
    let raw = entity_id.as_str();
    let slug = match raw.split_once(':') {
        Some((_prefix, rest)) if !rest.is_empty() => rest,
        _ => raw,
    };
    slug.replace('_', " ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{EntityKindExpectation, RecordExpectations, ResearchPlan};
    use chrono::{TimeZone, Utc};
    use situation_room_core::vocab::{EntityId, Topic};
    use situation_room_storage::Store;
    use uuid::Uuid;

    fn sample_plan_with_exemplars(
        exemplars_by_kind: &[(&str, &[&str])],
    ) -> ResearchPlan {
        let entity_kinds = exemplars_by_kind
            .iter()
            .map(|(kind, ex)| EntityKindExpectation {
                kind: (*kind).to_string(),
                exemplars: ex
                    .iter()
                    .map(|s| EntityId::new(*s).expect("valid entity id"))
                    .collect(),
                attributes: vec![],
                rationale: "test".into(),
            })
            .collect();

        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "test topic".into(),
            interpretation: "test interpretation".into(),
            topic_tags: vec![Topic::new("test").unwrap()],
            geographic_scope: vec![],
            historical_window_days: 730,
            expectations: RecordExpectations {
                observation_metrics: vec![],
                event_types: vec![],
                entity_kinds,
                relation_kinds: vec![],
                document_sources: vec![],
                assertion_guidance: None,
            },
            created_at: Utc::now(),
        }
    }

    #[test]
    fn canonical_name_strips_prefix_and_humanises() {
        let id = EntityId::new("company:ibm_quantum").unwrap();
        assert_eq!(canonical_name_from_exemplar(&id), "ibm quantum");
    }

    #[test]
    fn canonical_name_handles_no_prefix() {
        let id = EntityId::new("greenbushes").unwrap();
        assert_eq!(canonical_name_from_exemplar(&id), "greenbushes");
    }

    #[test]
    fn canonical_name_handles_empty_slug_after_colon() {
        // Trailing-colon edge case. Better to render the raw
        // string than an empty cell.
        let id = EntityId::new("company:").unwrap();
        assert_eq!(canonical_name_from_exemplar(&id), "company:");
    }

    #[test]
    fn canonical_name_keeps_inner_colons_after_first_split() {
        let id = EntityId::new("port:singapore:terminal_a").unwrap();
        assert_eq!(
            canonical_name_from_exemplar(&id),
            "singapore:terminal a"
        );
    }

    #[test]
    fn build_entity_carries_plan_keyed_provenance() {
        let plan = sample_plan_with_exemplars(&[("company", &["company:tsla"])]);
        let id = EntityId::new("company:tsla").unwrap();
        let accepted = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

        let entity = build_exemplar_entity(&plan, "company", &id, accepted);

        assert_eq!(entity.entity_id.as_str(), "company:tsla");
        assert_eq!(entity.kind, "company");
        assert_eq!(entity.canonical_name, "tsla");
        assert_eq!(
            entity.envelope.provenance.source_id,
            format!("plan:{}#entity_exemplar", plan.id),
            "source_id must carry the plan id so records_for_plan can route it"
        );
        assert!(entity.envelope.provenance.source_url.is_none());
        assert_eq!(entity.envelope.observed_at, accepted);
        // Subject entities seed the cross-record entity join.
        assert_eq!(entity.envelope.subjects.entities.len(), 1);
        assert_eq!(
            entity.envelope.subjects.entities[0].as_str(),
            "company:tsla"
        );
        // Topics propagate from the plan so the entity is
        // recoverable via `Subjects::topics` lookups too.
        assert_eq!(entity.envelope.subjects.topics.len(), 1);
        assert_eq!(entity.envelope.subjects.topics[0].as_str(), "test");
    }

    #[test]
    fn report_total_sums_all_buckets() {
        let mut r = MaterializationReport::default();
        r.materialized = 3;
        r.skipped_existing = 2;
        r.refreshed = 1;
        r.errors.push("x: y".into());
        assert_eq!(r.total(), 7);
    }

    #[test]
    fn materialize_inserts_fresh_exemplars() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[
            ("company", &["company:tsla", "company:ford"]),
            ("agency", &["agency:sec"]),
        ]);

        let report = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.materialized, 3);
        assert_eq!(report.skipped_existing, 0);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        // Confirm round-trip via the storage layer.
        let tsla = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .expect("tsla should be persisted");
        assert_eq!(tsla.kind, "company");
        assert_eq!(tsla.canonical_name, "tsla");
    }

    #[test]
    fn materialize_is_idempotent_on_reaccept() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[("company", &["company:tsla"])]);

        let first = materialize_entity_exemplars(&plan, &store, Utc::now());
        let second = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(first.materialized, 1);
        assert_eq!(first.skipped_existing, 0);
        assert_eq!(second.materialized, 0);
        assert_eq!(second.skipped_existing, 1);
        assert!(second.errors.is_empty());
    }

    #[test]
    fn materialize_skips_empty_kind_strings() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[
            ("", &["company:nope"]),
            ("company", &["company:yes"]),
        ]);

        let report = materialize_entity_exemplars(&plan, &store, Utc::now());

        // The empty-kind bucket is skipped wholesale; the valid
        // bucket goes through.
        assert_eq!(report.materialized, 1);
        assert_eq!(report.skipped_existing, 0);
        assert!(report.errors.is_empty());

        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:nope").unwrap())
            .is_err());
        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:yes").unwrap())
            .is_ok());
    }

    #[test]
    fn report_default_is_empty() {
        // The `errors` Vec must be empty-by-default so callers can
        // treat `report.errors.is_empty()` as the success predicate.
        // The exemplar-by-exemplar failure path is otherwise hard to
        // exercise without a mockable Store — the
        // UNIQUE-by-existence-check branch is covered by
        // `materialize_is_idempotent_on_reaccept`; transient DB
        // errors are an integration concern.
        let report = MaterializationReport::default();
        assert!(report.errors.is_empty());
        assert_eq!(report.materialized, 0);
        assert_eq!(report.skipped_existing, 0);
        assert_eq!(report.refreshed, 0);
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn materialize_handles_empty_expectations() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[]);
        let report = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.total(), 0);
    }

    // -------------------------------------------------------------------
    // Sn-100 #4 — elevate-on-re-accept semantics
    // -------------------------------------------------------------------

    /// Helper: poke a `RecipeIterator`-tier entity row into the store,
    /// matching the shape Sn-97 Lever B (`recipe_apply::build_record`)
    /// emits today. License = `"unknown"` is the closed-vocab signal
    /// for that tier.
    fn seed_recipe_iterator_entity(store: &Store, entity_id: &str, name: &str) {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!("recipe:lever-b:{entity_id}"),
                source_url: None,
                source_published_at: None,
                license: "unknown".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects {
                entities: vec![EntityId::new(entity_id).unwrap()],
                places: vec![],
                time: None,
                topics: vec![],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let ent = Entity::new(
            EntityId::new(entity_id).unwrap(),
            "company",
            name,
            envelope,
        );
        store.insert_entity(&ent).expect("seed RecipeIterator row");
    }

    /// Helper: poke a `DocumentExtracted`-tier entity row into the
    /// store, matching the shape Sn-97 Lever A
    /// (`extract::build_extracted_entity`) emits. License =
    /// `"extracted"` is the closed-vocab signal.
    fn seed_document_extracted_entity(store: &Store, entity_id: &str, name: &str) {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!("recipe:lever-a:{entity_id}"),
                source_url: None,
                source_published_at: None,
                license: "extracted".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects {
                entities: vec![EntityId::new(entity_id).unwrap()],
                places: vec![],
                time: None,
                topics: vec![],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let ent = Entity::new(
            EntityId::new(entity_id).unwrap(),
            "company",
            name,
            envelope,
        );
        store.insert_entity(&ent).expect("seed DocumentExtracted row");
    }

    #[test]
    fn materialize_refreshes_recipe_iterator_to_slug_humanised() {
        // Lever B wrote first at RecipeIterator. Plan-accept's
        // SlugHumanised exemplar must elevate the row in place; the
        // refresh bucket counts it.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        seed_recipe_iterator_entity(&store, "company:tsla", "tsla-noisy");

        let plan = sample_plan_with_exemplars(&[("company", &["company:tsla"])]);
        let report = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.materialized, 0);
        assert_eq!(report.skipped_existing, 0);
        assert_eq!(
            report.refreshed, 1,
            "RecipeIterator row must elevate to SlugHumanised"
        );

        // The row's canonical_name + license reflect the refresh.
        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(
            back.canonical_name, "tsla",
            "SlugHumanised canonical_name wins over the iterator scalar"
        );
        assert_eq!(back.envelope.provenance.license, "classifier-emitted");

        // The refresh-log captured exactly one event.
        let log = store.entity_refresh_log_snapshot();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].entity_id, "company:tsla");
        assert_eq!(log[0].previous_tier, EntityProvenanceTier::RecipeIterator);
        assert_eq!(log[0].new_tier, EntityProvenanceTier::SlugHumanised);
    }

    #[test]
    fn materialize_does_not_refresh_document_extracted_row() {
        // Lever A wrote first at DocumentExtracted. Plan-accept's
        // SlugHumanised exemplar must NOT clobber the prose name. The
        // outcome falls into `skipped_existing` (same-or-lower-tier
        // no-op), not `refreshed`.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        seed_document_extracted_entity(&store, "company:tsla", "Tesla, Inc.");

        let plan = sample_plan_with_exemplars(&[("company", &["company:tsla"])]);
        let report = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.materialized, 0);
        assert_eq!(report.refreshed, 0);
        assert_eq!(
            report.skipped_existing, 1,
            "DocumentExtracted row must stay untouched; counted as skipped_existing"
        );

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(
            back.canonical_name, "Tesla, Inc.",
            "Lever A prose name must survive a SlugHumanised re-accept"
        );
        assert_eq!(back.envelope.provenance.license, "extracted");
        assert!(store.entity_refresh_log_snapshot().is_empty());
    }

    #[test]
    fn materialize_reaccept_at_same_slug_humanised_tier_is_no_op() {
        // First accept: fresh insert at SlugHumanised. Second accept:
        // same-tier upsert → no-op, no refresh event.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[("company", &["company:tsla"])]);
        let first = materialize_entity_exemplars(&plan, &store, Utc::now());
        let second = materialize_entity_exemplars(&plan, &store, Utc::now());

        assert_eq!(first.materialized, 1);
        assert_eq!(first.refreshed, 0);
        assert_eq!(first.skipped_existing, 0);

        assert_eq!(second.materialized, 0);
        assert_eq!(second.refreshed, 0);
        assert_eq!(second.skipped_existing, 1);
        assert!(
            store.entity_refresh_log_snapshot().is_empty(),
            "same-tier re-accept must not push a refresh event"
        );
    }

    #[test]
    fn materialize_inserted_path_does_not_push_refresh_event() {
        // Fresh insert is the common cold-cache path; the refresh log
        // must stay empty.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_exemplars(&[("company", &["company:newco"])]);
        let report = materialize_entity_exemplars(&plan, &store, Utc::now());
        assert_eq!(report.materialized, 1);
        assert!(store.entity_refresh_log_snapshot().is_empty());
    }
}
