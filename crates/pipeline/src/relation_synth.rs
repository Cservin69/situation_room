//! Plan-accept-time Relation triple materialisation (Session 77).
//!
//! Sibling of [`crate::entity_synth`] (Session 76). The Level-1
//! classifier emits `(from, kind, to)` prototype triples on each
//! `RelationKindExpectation::exemplar_triples` — concrete edges the
//! topic revolves around (`relation_kinds[i].exemplar_triples[j]`).
//! Pre-Session-77 the classifier emitted only the relation `kind` and
//! no triples, so the dashboard's Relations panel stayed at 0
//! system-wide regardless of how relation-rich the topic was — the
//! same shape Session 76 closed for the Entities panel.
//!
//! This module promotes each triple to a [`Relation`] row at
//! plan-accept time, before any fetching runs. The kind comes from
//! the expectation; `from` / `to` come from the triple; the envelope
//! carries a plan-keyed `source_id` so `records_for_plan` finds the
//! row under the originating plan (Session 76 extended the SQL pattern
//! to match `plan:{plan_id}#%`).
//!
//! ## Scope (Session 77)
//!
//! - **Closed-vocabulary only.** Triples come from the classifier's
//!   structured output — no host-specific code, no source routing.
//! - **No LLM calls.** Free path. The cost is one `INSERT` per triple
//!   that isn't already in the `relations` table.
//! - **Idempotent.** Re-accepting a plan, re-classifying the same
//!   topic, or seeing the same `(kind, from, to)` in two different
//!   plans is safe — `dedup_key` is deterministically derived as
//!   `"plan:{plan_id}#relation_exemplar:{kind}:{from}:{to}"` and the
//!   existence check uses it. The `relations` table does not (yet)
//!   declare a UNIQUE constraint on `dedup_key`, so the check is
//!   explicit rather than constraint-driven.
//! - **Non-fatal on per-triple failure.** A malformed exemplar
//!   string or a transient DB error during one insert must not
//!   break `accept_plan`. Failures land in
//!   [`MaterializationReport::errors`] for operator visibility; the
//!   loop keeps going.
//!
//! ## What this module does NOT do
//!
//! - Relation discovery from fetched documents. That's the Phase-3
//!   LLM extraction layer (`pipeline::extract`).
//! - Multi-hop relation derivation. The classifier may name "A owns
//!   B" and "B operates C"; this module does not synthesise "A
//!   controls C." Reasoning over the relation graph is downstream
//!   analysis, not classification-time materialisation.

use chrono::{DateTime, Utc};
use situation_room_core::schema::content::RelationContent;
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::Relation;
use situation_room_core::vocab::Confidence;
use situation_room_storage::Store;
use tracing::{debug, info, warn};

use crate::research::{RelationTripleExemplar, ResearchPlan};

/// Summary of one materialisation pass over a plan's
/// `relation_kinds[*].exemplar_triples[*]`. Returned to callers so
/// they can log a single per-plan summary line instead of one log
/// per triple.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationReport {
    /// Triples that didn't exist in `relations` yet (by `dedup_key`)
    /// and were inserted as fresh rows.
    pub materialized: u32,

    /// Triples whose `dedup_key` already pointed to a `relations`
    /// row (from a prior accept of this plan, or from a different
    /// plan that emitted the same `(kind, from, to)`). No INSERT
    /// issued.
    pub skipped_existing: u32,

    /// Per-triple failures. Each entry is a short human-readable
    /// string `"{kind}:{from}->{to}: {error}"` so the operator can
    /// see what went wrong without scraping multi-line logs.
    pub errors: Vec<String>,
}

impl MaterializationReport {
    /// Total triples considered — sum of the three buckets.
    pub fn total(&self) -> u32 {
        self.materialized + self.skipped_existing + self.errors.len() as u32
    }
}

/// Walk a plan's `relation_kinds[*].exemplar_triples[*]` and persist
/// each as a [`Relation`] row in `store`. Idempotent; safe to call
/// repeatedly.
///
/// The runtime path is [`crate::relation_synth`]'s only public entry
/// point. Callers should treat its `MaterializationReport` as
/// observability — never as a `Result` to propagate; per-triple
/// failures are captured inside the report and must not break
/// plan-accept (the entire purpose of this hook is to populate the
/// dashboard; surfacing as a hard error here would mask the more
/// important successful inserts).
pub fn materialize_relation_exemplars(
    plan: &ResearchPlan,
    store: &Store,
    accepted_at: DateTime<Utc>,
) -> MaterializationReport {
    let mut report = MaterializationReport::default();

    for kind_exp in &plan.expectations.relation_kinds {
        let kind = kind_exp.kind.trim();
        if kind.is_empty() {
            // Defensive: a classifier bug that emits empty `kind`
            // would produce kind="" relations the dashboard renders
            // as "(unknown)". Skip and warn, don't fail the loop.
            warn!(
                plan_id = %plan.id,
                "relation_synth: skipping relation_kind with empty kind string"
            );
            continue;
        }

        for triple in &kind_exp.exemplar_triples {
            match try_materialize_one(plan, kind, triple, store, accepted_at) {
                Ok(MaterializeOutcome::Inserted) => {
                    report.materialized += 1;
                    debug!(
                        plan_id = %plan.id,
                        kind = %kind,
                        from = %triple.from.as_str(),
                        to = %triple.to.as_str(),
                        "relation_synth: materialised triple"
                    );
                }
                Ok(MaterializeOutcome::AlreadyExists) => {
                    report.skipped_existing += 1;
                }
                Err(e) => {
                    report.errors.push(format!(
                        "{}:{}->{}: {e}",
                        kind,
                        triple.from.as_str(),
                        triple.to.as_str()
                    ));
                    warn!(
                        plan_id = %plan.id,
                        kind = %kind,
                        from = %triple.from.as_str(),
                        to = %triple.to.as_str(),
                        error = %e,
                        "relation_synth: per-triple materialisation failed; \
                         continuing with remaining triples"
                    );
                }
            }
        }
    }

    info!(
        plan_id = %plan.id,
        materialized = report.materialized,
        skipped_existing = report.skipped_existing,
        errors = report.errors.len(),
        "relation_synth: plan-accept materialisation complete"
    );

    report
}

/// Per-triple outcome inside the materialisation loop.
enum MaterializeOutcome {
    Inserted,
    AlreadyExists,
}

/// Single-triple attempt: existence check via `dedup_key`, then build
/// + insert. Split out so the loop body stays scannable and the unit
/// tests can drive specific outcomes against an in-memory store.
fn try_materialize_one(
    plan: &ResearchPlan,
    kind: &str,
    triple: &RelationTripleExemplar,
    store: &Store,
    accepted_at: DateTime<Utc>,
) -> Result<MaterializeOutcome, String> {
    let dedup = dedup_key_for_triple(plan, kind, triple);

    match store.relation_exists_by_dedup_key(&dedup) {
        Ok(true) => return Ok(MaterializeOutcome::AlreadyExists),
        Ok(false) => { /* fall through to insert */ }
        Err(e) => return Err(format!("existence check: {e}")),
    }

    let relation = build_exemplar_relation(plan, kind, triple, accepted_at, dedup);
    store
        .insert_relation(&relation)
        .map_err(|e| format!("insert: {e}"))?;
    Ok(MaterializeOutcome::Inserted)
}

/// Construct one [`Relation`] from `(plan, kind, triple, accepted_at,
/// dedup_key)`. Pure function — no I/O, no `Store` — so the unit
/// tests can pin the envelope shape without standing up a DB.
///
/// `source_id` uses the format `"plan:{plan_id}#relation_exemplar"`.
/// `records_for_plan` (Session 76 extension) matches the
/// `plan:{plan_id}#%` prefix in addition to its existing
/// `%#recipe:{recipe_uuid}@v%` patterns, so the row routes to the
/// originating plan's dashboard view even when no recipes have run
/// yet.
///
/// Subjects carry both endpoints under `entities` so the
/// cross-record entity join surfaces the relation alongside the two
/// Entity rows the entity-synth materialiser writes; topics
/// propagate from the plan's `topic_tags` so the cross-plan
/// topic-filter view sees the relation under the same tag the
/// originating plan declared.
pub fn build_exemplar_relation(
    plan: &ResearchPlan,
    kind: &str,
    triple: &RelationTripleExemplar,
    accepted_at: DateTime<Utc>,
    dedup_key: String,
) -> Relation {
    let provenance = Provenance {
        source_id: format!("plan:{}#relation_exemplar", plan.id),
        source_url: None,
        source_published_at: None,
        license: "classifier-emitted".to_string(),
        derived_from: vec![],
    };

    let subjects = Subjects {
        entities: vec![triple.from.clone(), triple.to.clone()],
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

    let content = RelationContent {
        kind: kind.to_string(),
        from: triple.from.clone(),
        to: triple.to.clone(),
        magnitude: None,
        valid_until: None,
    };

    let mut rel = Relation::new(envelope, content);
    rel.dedup_key = Some(dedup_key);
    rel
}

/// Deterministic `dedup_key` for a `(plan, kind, from, to)` tuple.
/// Re-running the same plan-accept against the same triple always
/// produces the same key, which is what powers the idempotency
/// check.
///
/// Format: `"plan:{plan_id}#relation_exemplar:{kind}:{from}:{to}"`.
/// The `plan_id` prefix scopes the dedup namespace per-plan: two
/// different plans that both emit `(company:tsla, supplier_of,
/// company:panasonic)` get distinct dedup_keys and therefore each
/// get their own Relation row — which is correct under the per-plan
/// dashboard model (Session 63's cross-plan view aggregates from
/// the `relations` table directly, so duplication is fine; what
/// matters is that re-accepting *the same* plan is a no-op).
pub fn dedup_key_for_triple(
    plan: &ResearchPlan,
    kind: &str,
    triple: &RelationTripleExemplar,
) -> String {
    format!(
        "plan:{}#relation_exemplar:{}:{}:{}",
        plan.id,
        kind,
        triple.from.as_str(),
        triple.to.as_str()
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{
        EntityKindExpectation, RecordExpectations, RelationKindExpectation,
        RelationTripleExemplar, ResearchPlan,
    };
    use chrono::TimeZone;
    use situation_room_core::vocab::{EntityId, Topic};
    use uuid::Uuid;

    fn sample_plan_with_triples(
        triples_by_kind: &[(&str, &[(&str, &str)])],
    ) -> ResearchPlan {
        let relation_kinds = triples_by_kind
            .iter()
            .map(|(kind, triples)| RelationKindExpectation {
                kind: (*kind).to_string(),
                exemplar_triples: triples
                    .iter()
                    .map(|(from, to)| RelationTripleExemplar {
                        from: EntityId::new(*from).expect("valid entity id"),
                        to: EntityId::new(*to).expect("valid entity id"),
                        rationale: None,
                    })
                    .collect(),
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
                entity_kinds: vec![],
                relation_kinds,
                document_sources: vec![],
                assertion_guidance: None,
            },
            created_at: Utc::now(),
        }
    }

    #[test]
    fn dedup_key_is_deterministic() {
        let plan = sample_plan_with_triples(&[(
            "supplier_of",
            &[("company:panasonic", "company:tsla")],
        )]);
        let triple = &plan.expectations.relation_kinds[0].exemplar_triples[0];
        let k1 = dedup_key_for_triple(&plan, "supplier_of", triple);
        let k2 = dedup_key_for_triple(&plan, "supplier_of", triple);
        assert_eq!(k1, k2);
        assert!(k1.starts_with(&format!("plan:{}#relation_exemplar:", plan.id)));
        assert!(k1.contains(":supplier_of:"));
        assert!(k1.ends_with(":company:panasonic:company:tsla"));
    }

    #[test]
    fn dedup_key_distinguishes_endpoints_and_kind() {
        let plan = sample_plan_with_triples(&[]);
        let t1 = RelationTripleExemplar {
            from: EntityId::new("company:a").unwrap(),
            to: EntityId::new("company:b").unwrap(),
            rationale: None,
        };
        let t2 = RelationTripleExemplar {
            from: EntityId::new("company:b").unwrap(),
            to: EntityId::new("company:a").unwrap(),
            rationale: None,
        };
        assert_ne!(
            dedup_key_for_triple(&plan, "supplier_of", &t1),
            dedup_key_for_triple(&plan, "supplier_of", &t2),
            "direction must matter for dedup"
        );
        assert_ne!(
            dedup_key_for_triple(&plan, "supplier_of", &t1),
            dedup_key_for_triple(&plan, "buyer_of", &t1),
            "kind must matter for dedup"
        );
    }

    #[test]
    fn build_relation_carries_plan_keyed_provenance() {
        let plan = sample_plan_with_triples(&[(
            "supplier_of",
            &[("company:panasonic", "company:tsla")],
        )]);
        let triple = &plan.expectations.relation_kinds[0].exemplar_triples[0];
        let accepted = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();
        let dedup = dedup_key_for_triple(&plan, "supplier_of", triple);

        let rel = build_exemplar_relation(&plan, "supplier_of", triple, accepted, dedup.clone());

        assert_eq!(rel.content.kind, "supplier_of");
        assert_eq!(rel.content.from.as_str(), "company:panasonic");
        assert_eq!(rel.content.to.as_str(), "company:tsla");
        assert_eq!(
            rel.envelope.provenance.source_id,
            format!("plan:{}#relation_exemplar", plan.id)
        );
        assert!(rel.envelope.provenance.source_url.is_none());
        assert_eq!(rel.envelope.observed_at, accepted);
        assert_eq!(rel.dedup_key.as_deref(), Some(dedup.as_str()));
        // Subjects: both endpoints listed so the cross-record entity
        // join surfaces the relation alongside the two Entity rows
        // entity-synth materialised.
        assert_eq!(rel.envelope.subjects.entities.len(), 2);
        assert_eq!(rel.envelope.subjects.entities[0].as_str(), "company:panasonic");
        assert_eq!(rel.envelope.subjects.entities[1].as_str(), "company:tsla");
        // Topics propagate from the plan.
        assert_eq!(rel.envelope.subjects.topics.len(), 1);
        assert_eq!(rel.envelope.subjects.topics[0].as_str(), "test");
    }

    #[test]
    fn materialize_inserts_fresh_triples() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_triples(&[
            (
                "supplier_of",
                &[
                    ("company:panasonic", "company:tsla"),
                    ("company:catl", "company:tsla"),
                ],
            ),
            ("subsidiary_of", &[("company:spacex_subsidiary", "company:spacex")]),
        ]);

        let report = materialize_relation_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.materialized, 3);
        assert_eq!(report.skipped_existing, 0);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    }

    #[test]
    fn materialize_is_idempotent_on_reaccept() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_triples(&[(
            "supplier_of",
            &[("company:panasonic", "company:tsla")],
        )]);

        let first = materialize_relation_exemplars(&plan, &store, Utc::now());
        let second = materialize_relation_exemplars(&plan, &store, Utc::now());

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

        let plan = sample_plan_with_triples(&[
            ("", &[("company:a", "company:b")]),
            ("supplier_of", &[("company:c", "company:d")]),
        ]);

        let report = materialize_relation_exemplars(&plan, &store, Utc::now());

        // The empty-kind bucket is skipped wholesale; the valid
        // bucket goes through.
        assert_eq!(report.materialized, 1);
        assert_eq!(report.skipped_existing, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn materialize_handles_empty_relation_kinds() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_triples(&[]);
        let report = materialize_relation_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.total(), 0);
    }

    #[test]
    fn materialize_handles_kind_with_no_triples() {
        // A relation_kind with no exemplar_triples is the
        // pre-Session-77 shape and the common case for unknown
        // topics. Must not produce any Relations.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan_with_triples(&[("supplier_of", &[])]);
        let report = materialize_relation_exemplars(&plan, &store, Utc::now());

        assert_eq!(report.total(), 0);
    }

    #[test]
    fn report_total_sums_all_buckets() {
        let mut r = MaterializationReport::default();
        r.materialized = 3;
        r.skipped_existing = 2;
        r.errors.push("x: y".into());
        assert_eq!(r.total(), 6);
    }
}
