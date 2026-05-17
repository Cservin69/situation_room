//! Operator-triggered re-extraction of relation Assertions from
//! Documents already on disk (Session 92, ADR 0023 Option 2).
//!
//! ## Why this module exists
//!
//! Session 91 / ADR 0023 landed `document_assertions.md` v1.2 with
//! multi-claimant attribution. The new prompt fires only on net-new
//! fetches under the executor's per-Document hook; Documents already
//! on disk keep the singleton-claimant shape they were extracted
//! under (v1.1 or earlier). For the pre-Sn-91 article-kind Document
//! pile, the only way to materialise the v1.2 attribution shape is to
//! re-run the extractor against the stored body.
//!
//! ## What this module does
//!
//! [`reextract_relations_for_plan`] iterates a plan's article-kind
//! Documents (matching the same `should_extract_from` gate the live
//! executor uses) and re-runs
//! [`crate::extract::extract_and_persist_assertions`] against each.
//! Each call uses the current production prompt (the v1.2 string
//! threaded through from `AppState::document_assertions_prompt`).
//!
//! ## Closed-vocab discipline
//!
//! No host strings. Documents route back to their owning recipe via
//! the canonical `{source}#recipe:{uuid}@v{ver}` `source_id` shape
//! both `extract::build_assertion` writes and
//! `records_for_plan`'s LIKE join reads (Session 22). Documents
//! whose `source_id` doesn't parse to a known recipe are silently
//! skipped — they may have been ingested before the recipe-routed
//! provenance shape existed, or via a synth path that bypasses
//! recipes entirely.
//!
//! ## Cost discipline
//!
//! Bounded by **article-kind Document count per plan**. One
//! workhorse-tier LLM call per Document. The summary returned by the
//! Tauri caller surfaces document/assertion counts so the operator
//! can see the spend before reaching for a second plan — matches
//! `feedback_eval_cost_discipline`. No retry on call failure; the
//! per-Document orchestrator absorbs LLM errors into its report.
//!
//! ## Idempotency
//!
//! Re-running this command produces fresh Assertion rows — there is
//! no per-Document dedup in v1, identical to Session 77's posture
//! when the executor first lit up the per-Document hook. The
//! downstream `promote_consensus_for_plan` pass dedups at the
//! cross-source consensus layer (ADR 0021). Operators who want to
//! re-extract repeatedly without piling on duplicates should run
//! promote between re-extracts.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use situation_room_llm::LlmProvider;
use situation_room_storage::Store;
use tracing::{info, warn};
use uuid::Uuid;

use crate::extract;
use crate::recipes::FetchRecipe;
use crate::recipes_store::load_recipes_for_plan;
use crate::research::ResearchPlan;

/// Per-plan re-extraction summary, surfaced to the dashboard.
///
/// All counts are non-negative `u32`s, never `Option`s — a zero
/// result is a meaningful answer ("the plan has no article-kind
/// Documents on disk yet") and the dashboard's renderer treats
/// 0/N/M counters uniformly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReextractReport {
    /// Documents the gate (article-kind + non-empty body) accepted.
    pub documents_considered: u32,
    /// Documents whose `source_id` failed to parse to a recipe in
    /// this plan — silently skipped. Almost always pre-Sn-22 rows
    /// or synth-path rows that don't carry the canonical shape.
    pub documents_unrouted: u32,
    /// Sum of `ExtractionReport.extracted` across all per-Document
    /// passes (raw count of LLM-emitted Assertion drafts that
    /// survived the validator).
    pub assertions_extracted: u32,
    /// Sum of `ExtractionReport.persisted` — how many of the
    /// extracted drafts actually landed in storage.
    pub assertions_persisted: u32,
    /// Sum of `ExtractionReport.insert_failures` — per-Assertion
    /// `insert_assertion` errors. Should be zero in steady state.
    pub assertion_insert_failures: u32,
    /// Documents whose LLM call returned `Err(_)`. The orchestrator
    /// absorbed the error and continued; this counter surfaces the
    /// rate to the operator.
    pub llm_call_errors: u32,
}

/// Iterate a plan's article-kind Documents and re-run the relation
/// extractor on each. Returns a per-plan summary.
///
/// **Cost.** One workhorse-tier LLM call per Document that passes
/// the gate. The caller should expose a per-plan button (Session 92
/// Option 2 chose per-plan over per-Document granularity), not an
/// auto-trigger.
pub async fn reextract_relations_for_plan(
    store: &Store,
    provider: &dyn LlmProvider,
    document_assertions_prompt: &str,
    plan: &ResearchPlan,
) -> ReextractReport {
    let mut report = ReextractReport::default();

    // Build the recipe lookup map up-front. Documents fan in to
    // recipes via the parsed `source_id` shape; the linear scan over
    // recipes per Document would be O(D·R), the map is O(D + R).
    let recipes = match load_recipes_for_plan(store, plan.id) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                plan_id = %plan.id,
                error = %e,
                "reextract: failed to load recipes for plan; nothing to route documents to"
            );
            return report;
        }
    };
    let recipe_by_id: std::collections::HashMap<Uuid, FetchRecipe> =
        recipes.into_iter().map(|r| (r.id, r)).collect();

    // The records-for-plan join surfaces the same Documents the
    // dashboard's Documents panel renders; we re-use it rather than
    // adding a parallel storage query so a future Document-routing
    // tweak moves both surfaces in lockstep.
    let records = match store.records_for_plan(plan.id) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                plan_id = %plan.id,
                error = %e,
                "reextract: records_for_plan failed; no Documents to iterate"
            );
            return report;
        }
    };

    for doc in &records.documents {
        // Gate: article-kind MIME + non-empty body. Same predicate
        // `extract::extract_and_persist_assertions` re-checks, but
        // we early-out here so the documents_considered counter
        // only ticks for Documents the extractor will actually
        // touch.
        if !extract::should_extract_from(&doc.mime, doc.body.len()) {
            continue;
        }
        report.documents_considered += 1;

        // Parse the `source_id` to find the owning recipe. Format
        // `{source}#recipe:{uuid}@v{version}` — same shape
        // build_assertion writes, recipe_apply::build_record
        // writes, and records_for_plan's LIKE join reads.
        let source_id = &doc.envelope.provenance.source_id;
        let recipe_id = match parse_recipe_id_from_source_id(source_id) {
            Some(id) => id,
            None => {
                report.documents_unrouted += 1;
                continue;
            }
        };
        let recipe = match recipe_by_id.get(&recipe_id) {
            Some(r) => r,
            None => {
                report.documents_unrouted += 1;
                continue;
            }
        };

        // observed_at on the Document envelope carries the original
        // fetched_at — we re-use it so the new Assertions inherit
        // that timestamp on their own envelope. Today's clock is
        // wrong (the Document body wasn't fetched today).
        let fetched_at = doc.envelope.observed_at;

        // Pass the stored body bytes through. `extract_and_persist_assertions`
        // calls `document_synth::body_preview_for_mime` internally —
        // for HTML MIME with already-stripped input that's an
        // idempotent no-op (no tags to strip, no entities to decode,
        // whitespace already collapsed). The double-pass cost is
        // microseconds per Document.
        let mime_str = doc.mime.as_str();
        let inner = extract::extract_and_persist_assertions(
            store,
            provider,
            document_assertions_prompt,
            plan,
            recipe,
            doc.body.as_bytes(),
            Some(mime_str),
            fetched_at,
        )
        .await;

        report.assertions_extracted += inner.extracted;
        report.assertions_persisted += inner.persisted;
        report.assertion_insert_failures += inner.insert_failures;
        if inner.call_error.is_some() {
            report.llm_call_errors += 1;
        }
    }

    info!(
        plan_id = %plan.id,
        documents_considered = report.documents_considered,
        documents_unrouted = report.documents_unrouted,
        assertions_extracted = report.assertions_extracted,
        assertions_persisted = report.assertions_persisted,
        assertion_insert_failures = report.assertion_insert_failures,
        llm_call_errors = report.llm_call_errors,
        at = %Utc::now(),
        "reextract: relation extraction pass complete for plan"
    );

    report
}

/// Parse the `recipe:` UUID out of a canonical
/// `{source}#recipe:{uuid}@v{version}` source_id. Returns `None` on
/// any deviation from the shape — pre-Sn-22 rows, plan-keyed
/// rows (`plan:{id}#…`), synth-path rows, anything malformed.
///
/// Lenient on the `@v{N}` suffix's digit count; strict on the
/// `#recipe:` marker so non-canonical shapes (`#recipe-`,
/// `recipe:` without `#`, etc.) don't accidentally match.
fn parse_recipe_id_from_source_id(source_id: &str) -> Option<Uuid> {
    let after_marker = source_id.split("#recipe:").nth(1)?;
    let uuid_str = after_marker.split('@').next()?;
    Uuid::parse_str(uuid_str).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recipe_id_canonical_shape() {
        let uuid = Uuid::now_v7();
        let s = format!("gdelt#recipe:{uuid}@v3");
        assert_eq!(parse_recipe_id_from_source_id(&s), Some(uuid));
    }

    #[test]
    fn parse_recipe_id_higher_version_count() {
        let uuid = Uuid::now_v7();
        let s = format!("usgs_mcs#recipe:{uuid}@v42");
        assert_eq!(parse_recipe_id_from_source_id(&s), Some(uuid));
    }

    #[test]
    fn parse_recipe_id_rejects_plan_keyed_shape() {
        // Session 76's plan-keyed shape: no `#recipe:` marker.
        let plan_id = Uuid::now_v7();
        let s = format!("plan:{plan_id}#entity:foo");
        assert_eq!(parse_recipe_id_from_source_id(&s), None);
    }

    #[test]
    fn parse_recipe_id_rejects_unmarked_uuid() {
        // A bare UUID without the `#recipe:` framing — common in
        // pre-Sn-22 rows.
        let uuid = Uuid::now_v7();
        let s = uuid.to_string();
        assert_eq!(parse_recipe_id_from_source_id(&s), None);
    }

    #[test]
    fn parse_recipe_id_rejects_malformed_uuid() {
        // Marker present, UUID isn't.
        let s = "gdelt#recipe:not-a-uuid@v1";
        assert_eq!(parse_recipe_id_from_source_id(s), None);
    }

    #[test]
    fn parse_recipe_id_rejects_almost_marker() {
        // `#recipe-` (dash) shouldn't accidentally match `#recipe:`.
        let uuid = Uuid::now_v7();
        let s = format!("gdelt#recipe-{uuid}@v1");
        assert_eq!(parse_recipe_id_from_source_id(&s), None);
    }

    #[test]
    fn reextract_report_default_is_zeroed() {
        let r = ReextractReport::default();
        assert_eq!(r.documents_considered, 0);
        assert_eq!(r.documents_unrouted, 0);
        assert_eq!(r.assertions_extracted, 0);
        assert_eq!(r.assertions_persisted, 0);
        assert_eq!(r.assertion_insert_failures, 0);
        assert_eq!(r.llm_call_errors, 0);
    }

    #[test]
    fn reextract_report_serde_roundtrip() {
        // The Tauri DTO derives Deserialize for JSON IPC; pin the
        // round-trip so a future shape edit doesn't silently break
        // the frontend.
        let r = ReextractReport {
            documents_considered: 5,
            documents_unrouted: 1,
            assertions_extracted: 12,
            assertions_persisted: 11,
            assertion_insert_failures: 1,
            llm_call_errors: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ReextractReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
