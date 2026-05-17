//! Per-plan relation-predicate vocabulary enforcement (Session 93,
//! Sn-90 carryover).
//!
//! ## Why this module exists
//!
//! The classifier emits a closed `relation_kinds: Vec<RelationKindExpectation>`
//! list per plan. Pre-Sn-93 the per-Document relation extractor
//! ([`crate::extract::extract_and_persist_assertions`]) honoured that
//! list only loosely — the LLM's draft `kind` field could carry any
//! string, and the orchestrator passed it through to `Assertion.kind`
//! without checking. The visible failure shape: a federal-reserve
//! plan accumulated relation Assertions with predicates like
//! `produces` (Tesla-shape), `acquired` (Meta-shape), etc., spilling
//! cross-plan training-data noise into the wrong topic's records.
//!
//! ## What this module does
//!
//! [`filter_drafts_against_plan`] takes a slice of
//! `extraction::AssertionDraft`s and the plan's
//! `relation_kinds[].kind` list (the closed vocabulary for the
//! plan's relations), and returns the subset whose `kind` matches
//! one of the declared predicates. Drops are warn-logged so the
//! operator-visible signal is "drift detected" rather than "silent
//! shrink".
//!
//! ## Closed-vocab discipline
//!
//! No host strings; no source strings; no per-claimant routing. The
//! vocabulary is read from the classifier's plan output and applied
//! uniformly. Drift caught here is the LLM hallucinating predicates
//! outside the plan's declared vocabulary — the same shape ADR 0019
//! and ADR 0017 are organised against, just at a different layer.
//!
//! ## What this module does NOT do
//!
//! - **It does not normalise predicates.** A draft whose `kind` is
//!   `Operates` when the plan declared `operates` is dropped, not
//!   case-folded; the prompt is responsible for emitting the exact
//!   predicate, and silent normalisation would mask drift.
//! - **It does not allow empty-vocab plans through.** When the
//!   plan declared no `relation_kinds` at all, every draft passes —
//!   the closed-vocab gate degenerates to "no gate" because the
//!   plan never named what shapes it cares about. That matches the
//!   classifier's posture: empty `relation_kinds` means "no
//!   relations expected", which the extractor honours by not
//!   calling the LLM (gated in [`crate::extract`]); a non-empty
//!   draft pile arriving here with no declared vocab is a
//!   pre-Sn-93 plan whose classifier output predates the closed-
//!   vocab discipline — we let it through with a debug-log so the
//!   change is observable.
//! - **It does not consult the predicate's `(from, to)` types.**
//!   Type-level gating is a separate question (and a separate
//!   ADR's territory); this module's contract is the predicate
//!   vocabulary alone.

use std::collections::HashSet;

use situation_room_llm::extraction::AssertionDraft;
use tracing::{debug, warn};

use crate::research::ResearchPlan;

/// Number of distinct dropped predicates to surface in a single
/// warn-log call. Higher cardinality just truncates with `, …` so
/// log lines stay bounded.
const MAX_PREDICATE_LIST: usize = 16;

/// Per-call summary surfaced to the orchestrator so the per-Document
/// `ExtractionReport` can grow a `drift_dropped` counter in a future
/// session. For now the summary is debug-logged at the call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PredicateFilterReport {
    /// Drafts the gate accepted.
    pub kept: usize,
    /// Drafts the gate dropped because their `kind` wasn't in the
    /// plan's declared `relation_kinds[].kind` list.
    pub dropped_unknown_kind: usize,
    /// Distinct predicate strings the gate dropped, capped at
    /// [`MAX_PREDICATE_LIST`] for log-line bounds.
    pub dropped_predicates_sample: Vec<String>,
}

/// Filter a slice of relation drafts against the plan's declared
/// `relation_kinds` vocabulary. Returns the kept drafts in input
/// order + a summary.
///
/// When the plan declared no `relation_kinds` at all, every draft
/// passes (the gate degenerates to no-op; see module docstring for
/// the rationale).
pub fn filter_drafts_against_plan(
    drafts: &[AssertionDraft],
    plan: &ResearchPlan,
) -> (Vec<AssertionDraft>, PredicateFilterReport) {
    if plan.expectations.relation_kinds.is_empty() {
        debug!(
            plan_id = %plan.id,
            n_drafts = drafts.len(),
            "relation_vocab: plan declared no relation_kinds; gate is no-op"
        );
        let mut report = PredicateFilterReport::default();
        report.kept = drafts.len();
        return (drafts.to_vec(), report);
    }

    let allowed: HashSet<&str> = plan
        .expectations
        .relation_kinds
        .iter()
        .map(|k| k.kind.as_str())
        .collect();

    let mut kept = Vec::with_capacity(drafts.len());
    let mut dropped_set: HashSet<String> = HashSet::new();
    let mut dropped_count: usize = 0;

    for draft in drafts {
        if allowed.contains(draft.kind.as_str()) {
            kept.push(draft.clone());
        } else {
            dropped_count += 1;
            if dropped_set.len() < MAX_PREDICATE_LIST {
                dropped_set.insert(draft.kind.clone());
            }
        }
    }

    if dropped_count > 0 {
        let mut sample: Vec<String> = dropped_set.into_iter().collect();
        sample.sort();
        warn!(
            plan_id = %plan.id,
            kept = kept.len(),
            dropped = dropped_count,
            allowed_count = allowed.len(),
            dropped_predicates = ?sample,
            "relation_vocab: drift dropped — LLM emitted predicates outside the plan's declared vocabulary"
        );
        let report = PredicateFilterReport {
            kept: kept.len(),
            dropped_unknown_kind: dropped_count,
            dropped_predicates_sample: sample,
        };
        (kept, report)
    } else {
        let report = PredicateFilterReport {
            kept: kept.len(),
            dropped_unknown_kind: 0,
            dropped_predicates_sample: Vec::new(),
        };
        (kept, report)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{RecordExpectations, RelationKindExpectation};
    use chrono::Utc;
    use situation_room_core::vocab::{Confidence, EntityId, Stance, Topic};
    use uuid::Uuid;

    fn mk_draft(kind: &str) -> AssertionDraft {
        AssertionDraft {
            claimant: EntityId::new("publisher:test").unwrap(),
            stance: Stance::Asserted,
            kind: kind.to_string(),
            from: EntityId::new("entity:a").unwrap(),
            to: EntityId::new("entity:b").unwrap(),
            confidence: Confidence::new(0.7).unwrap(),
        }
    }

    fn mk_plan_with(kinds: &[&str]) -> ResearchPlan {
        let relation_kinds = kinds
            .iter()
            .map(|k| RelationKindExpectation {
                kind: (*k).into(),
                exemplar_triples: vec![],
                rationale: "test".into(),
            })
            .collect();
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "test".into(),
            interpretation: "test".into(),
            topic_tags: vec![Topic::new("test").unwrap()],
            geographic_scope: vec![],
            historical_window_days: 365,
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
    fn allowed_predicate_is_kept() {
        let plan = mk_plan_with(&["holds_seat_on", "votes_with"]);
        let drafts = vec![mk_draft("holds_seat_on")];
        let (kept, report) = filter_drafts_against_plan(&drafts, &plan);
        assert_eq!(kept.len(), 1);
        assert_eq!(report.kept, 1);
        assert_eq!(report.dropped_unknown_kind, 0);
        assert!(report.dropped_predicates_sample.is_empty());
    }

    #[test]
    fn unknown_predicate_is_dropped() {
        let plan = mk_plan_with(&["holds_seat_on", "votes_with"]);
        let drafts = vec![mk_draft("produces"), mk_draft("holds_seat_on")];
        let (kept, report) = filter_drafts_against_plan(&drafts, &plan);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kind, "holds_seat_on");
        assert_eq!(report.dropped_unknown_kind, 1);
        assert_eq!(report.dropped_predicates_sample, vec!["produces".to_string()]);
    }

    #[test]
    fn case_mismatch_is_drift_not_a_silent_normalisation() {
        // The plan declared lowercase; the LLM emitted Title-case.
        // The gate drops it — silent normalisation would mask drift.
        let plan = mk_plan_with(&["operates"]);
        let drafts = vec![mk_draft("Operates")];
        let (kept, report) = filter_drafts_against_plan(&drafts, &plan);
        assert_eq!(kept.len(), 0);
        assert_eq!(report.dropped_unknown_kind, 1);
    }

    #[test]
    fn empty_relation_kinds_is_no_op_passthrough() {
        // Pre-Sn-93 plan (or genuinely no relations expected): the
        // gate degenerates to no-op so we don't accidentally drop
        // every draft on plans the classifier authored before this
        // contract existed.
        let plan = mk_plan_with(&[]);
        let drafts = vec![mk_draft("anything")];
        let (kept, report) = filter_drafts_against_plan(&drafts, &plan);
        assert_eq!(kept.len(), 1);
        assert_eq!(report.kept, 1);
        assert_eq!(report.dropped_unknown_kind, 0);
    }

    #[test]
    fn many_drops_truncate_sample_at_cap() {
        // Cardinality > MAX_PREDICATE_LIST: report.dropped_predicates_sample
        // is capped, but dropped_unknown_kind reflects the true count.
        let plan = mk_plan_with(&["allowed_only"]);
        let mut drafts = Vec::new();
        for i in 0..(MAX_PREDICATE_LIST + 5) {
            drafts.push(mk_draft(&format!("predicate_{i}")));
        }
        let (kept, report) = filter_drafts_against_plan(&drafts, &plan);
        assert_eq!(kept.len(), 0);
        assert_eq!(report.dropped_unknown_kind, MAX_PREDICATE_LIST + 5);
        assert_eq!(report.dropped_predicates_sample.len(), MAX_PREDICATE_LIST);
    }

    #[test]
    fn predicate_filter_report_default_is_zeroed() {
        let r = PredicateFilterReport::default();
        assert_eq!(r.kept, 0);
        assert_eq!(r.dropped_unknown_kind, 0);
        assert!(r.dropped_predicates_sample.is_empty());
    }
}
