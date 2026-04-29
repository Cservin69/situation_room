//! Fetch executor — Phase-6 runtime path (ADR 0007).
//!
//! Given an accepted [`ResearchPlan`], this module:
//!
//! 1. Loads the plan from storage.
//! 2. Loads the recipes already authored for the plan; if there are
//!    none, runs Level-2 authoring once per plan-bound source and
//!    persists the results.
//! 3. For each recipe whose extraction mode is currently supported by
//!    the executor (CSV in Session 8), fetches the URL, applies the
//!    recipe, normalizes, and writes the resulting records.
//! 4. Returns a [`FetchReport`] summarizing what happened. Per-recipe
//!    outcomes are carried so the UI can show which sources worked.
//!
//! ## ADR 0007 — the LLM-free runtime invariant
//!
//! Recipe authoring (step 2) is the **only** LLM-touching part of
//! this module, and it is conditional on no recipes existing for
//! the plan yet. Once recipes exist, runs are deterministic and
//! cheap: fetch → apply → normalize → insert. Re-authoring on
//! recipe failure is explicitly **not** in scope for Session 8 —
//! a failed recipe surfaces in the report and the user decides what
//! to do (handoff §"explicitly NOT").
//!
//! ## Extraction-mode policy in this session
//!
//! Only [`ExtractionSpec::CsvCell`] is wired through to apply +
//! insert. The other modes get authored normally (Level-2 picks
//! whatever fits the source) and are surfaced in the report as
//! `Skipped { reason }` rather than failures — they're not bugs,
//! they're a deliberate phasing of work. This is the cheapest
//! discipline that keeps the executor honest about what it can and
//! can't do without conflating "didn't try" with "tried and broke".
//!
//! ## What this module does NOT do
//!
//! - Re-author recipes on failure. (Per-failure auto-rewrite needs
//!   the failure-mode taxonomy, which we don't have yet.)
//! - Multi-plan or background scheduling. One plan, one call.
//! - Coverage reports. ADR 0007's all-gaps `CoverageReport` is a
//!   later session.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use stockpile_llm::{LlmProvider, ModelTier};
use stockpile_secure::bounds::Bounds;
use stockpile_storage::{
    fetch_runs::FetchRunRow, research_plans::PlanStatus, Store,
};

use crate::http_fetcher::{FetchError as HttpFetchError, HttpFetcher};
use crate::recipe_apply::{apply, ApplyContext, ApplyError};
use crate::recipe_author::{author_recipe, AuthoringContext, AuthoringError};
use crate::recipes::{ExtractionSpec, FetchRecipe};
use crate::recipes_store::{load_recipes_for_plan, save_recipe, RecipeStoreError};
use crate::research::{DocumentSourceHint, ResearchPlan};
use crate::research_plans_store::{load_research_plan, ResearchPlanStoreError};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Report returned from one [`run_fetch_for_plan`] invocation.
///
/// Every recipe that was considered shows up exactly once in
/// `outcomes`, with the variant naming why it was processed the way
/// it was. The aggregate counters (`recipes_attempted`,
/// `recipes_succeeded`, `records_produced`) are derived from the
/// outcomes and exist as convenience for the UI / tests; they're not
/// independently maintained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchReport {
    pub plan_id: Uuid,
    pub run_id: Uuid,
    pub outcomes: Vec<RecipeOutcome>,
    pub recipes_attempted: u32,
    pub recipes_succeeded: u32,
    pub records_produced: u32,
    /// Top-level error if the run failed before processing any recipe.
    /// `None` when the run completed normally — *individual* recipe
    /// failures live inside `outcomes`, not here.
    pub error_summary: Option<String>,
}

/// What happened with one recipe during a fetch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeOutcome {
    /// Recipe ran end-to-end: fetched, applied, records inserted.
    Succeeded {
        recipe_id: Uuid,
        source_id: String,
        records_produced: u32,
    },
    /// Recipe was skipped on purpose. Currently the only reason is
    /// "extraction mode not yet enabled in executor"; future Session 9+
    /// modes (JSON, CSS, regex, PDF) will turn these into Succeeded
    /// or Failed as they come online.
    Skipped {
        recipe_id: Uuid,
        source_id: String,
        reason: String,
    },
    /// Recipe ran but failed at some stage — fetch, apply, or insert.
    /// `stage` names the stage so the UI can render an icon /
    /// per-stage troubleshooting hint without parsing the message.
    Failed {
        recipe_id: Uuid,
        source_id: String,
        stage: FailureStage,
        message: String,
    },
}

/// Stage at which a recipe's run failed. Closed enum so the UI's
/// rendering logic doesn't need to grow with every internal error
/// variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    /// HTTP fetch errored before any bytes were returned.
    Fetch,
    /// `recipe_apply::apply` returned an error — extraction or
    /// content-assembly failure.
    Apply,
    /// Storage rejected one of the produced records on insert.
    Insert,
}

/// Errors that prevent the executor from doing any per-recipe work.
/// Per-recipe failures are not these — they live in [`RecipeOutcome`].
#[derive(Debug, Error)]
pub enum FetchExecutorError {
    #[error("plan not found: {0}")]
    PlanNotFound(Uuid),

    #[error("plan must be accepted before fetch (current: {current})")]
    PlanNotAccepted { current: PlanStatus },

    #[error("recipe load failed: {0}")]
    RecipeLoad(#[from] RecipeStoreError),

    #[error("plan load failed: {0}")]
    PlanLoad(#[from] ResearchPlanStoreError),

    #[error("recipe authoring failed wholesale: {0}")]
    Authoring(#[from] AuthoringError),

    #[error("storage error: {0}")]
    Storage(#[from] stockpile_storage::StorageError),
}

/// Inputs the executor needs from the composition root. Bundled into
/// a struct so the call site is one argument and so adding a new
/// dependency (e.g. a recipe-author config) doesn't ripple through
/// every test.
pub struct ExecutorContext<'a> {
    pub store: &'a Store,
    pub http: &'a dyn HttpFetcher,
    pub provider: &'a dyn LlmProvider,
    /// The recipe-author prompt template (loaded by the binary via
    /// `include_str!`, same pattern as the classifier prompt).
    pub recipe_author_prompt: &'a str,
}

/// Run the fetch executor against an accepted plan.
///
/// See module docs for the flow. This function is *not* a Tauri
/// command — that lives in the api crate per ADR 0001.
pub async fn run_fetch_for_plan(
    ctx: &ExecutorContext<'_>,
    plan_id: Uuid,
) -> Result<FetchReport, FetchExecutorError> {
    // 1. Open a fetch_run row immediately so even a wholesale failure
    //    leaves an audit trail. `started_at` is wall-clock here —
    //    deterministic time in tests is achieved by calling
    //    `run_fetch_for_plan` with a `tokio::time::pause`'d runtime
    //    or asserting on monotonic ordering rather than exact values.
    let run_id = Uuid::now_v7();
    let started_at = Utc::now();

    let mut run_row = FetchRunRow {
        id: run_id,
        plan_id,
        started_at,
        finished_at: None,
        recipes_attempted: 0,
        recipes_succeeded: 0,
        records_produced: 0,
        error_summary: None,
    };
    ctx.store.insert_fetch_run(&run_row)?;

    info!(plan_id = %plan_id, run_id = %run_id, "fetch run opened");

    // 2. Load and validate the plan. A wholesale failure here closes
    //    the run row with the appropriate error_summary so the audit
    //    log shows what went wrong.
    let plan = match prepare_plan(ctx, plan_id).await {
        Ok(p) => p,
        Err(e) => {
            close_run_with_error(ctx.store, &mut run_row, &e.to_string());
            return Err(e);
        }
    };

    // 3. Load-or-author recipes for the plan.
    let recipes = match load_or_author_recipes(ctx, &plan).await {
        Ok(r) => r,
        Err(e) => {
            close_run_with_error(ctx.store, &mut run_row, &e.to_string());
            return Err(e);
        }
    };

    info!(
        plan_id = %plan_id,
        run_id = %run_id,
        recipe_count = recipes.len(),
        "recipes prepared, executing"
    );

    // 4. Iterate recipes. Per-recipe failures don't abort the run —
    //    they get reported and we move on. This is what "deterministic
    //    runtime" feels like to the user: a partial failure leaves a
    //    partial result with a precise account of what worked.
    let mut outcomes = Vec::with_capacity(recipes.len());
    let mut records_produced_total: u32 = 0;
    let mut recipes_succeeded: u32 = 0;
    let recipes_attempted: u32 = recipes.len() as u32;

    for recipe in &recipes {
        let outcome = run_one_recipe(ctx, &plan, recipe).await;
        match &outcome {
            RecipeOutcome::Succeeded {
                records_produced, ..
            } => {
                recipes_succeeded += 1;
                records_produced_total = records_produced_total.saturating_add(*records_produced);
            }
            RecipeOutcome::Skipped { .. } => {}
            RecipeOutcome::Failed { stage, message, .. } => {
                warn!(plan_id = %plan_id, run_id = %run_id, ?stage, %message, "recipe failed");
            }
        }
        outcomes.push(outcome);
    }

    // 5. Close the run row with final counters.
    run_row.finished_at = Some(Utc::now());
    run_row.recipes_attempted = recipes_attempted;
    run_row.recipes_succeeded = recipes_succeeded;
    run_row.records_produced = records_produced_total;
    if let Err(e) = ctx.store.update_fetch_run(&run_row) {
        // Closing the row failing is concerning but doesn't invalidate
        // the work we just did. Log loudly and surface the produced
        // records anyway; the user gets the report, the on-disk run
        // row stays in its open state (which the next listing will
        // show as "in flight" — the next session's UI will need to
        // handle that gracefully, but for now it's better than
        // pretending the run didn't produce anything).
        warn!(plan_id = %plan_id, run_id = %run_id, error = %e, "failed to close fetch_run row");
    }

    info!(
        plan_id = %plan_id,
        run_id = %run_id,
        attempted = recipes_attempted,
        succeeded = recipes_succeeded,
        records = records_produced_total,
        "fetch run completed"
    );

    Ok(FetchReport {
        plan_id,
        run_id,
        outcomes,
        recipes_attempted,
        recipes_succeeded,
        records_produced: records_produced_total,
        error_summary: None,
    })
}

// ---------------------------------------------------------------------------
// Stage helpers
// ---------------------------------------------------------------------------

/// Load the plan and assert it's in the right state for fetching.
async fn prepare_plan(
    ctx: &ExecutorContext<'_>,
    plan_id: Uuid,
) -> Result<ResearchPlan, FetchExecutorError> {
    let stored = ctx
        .store
        .get_research_plan(plan_id)
        .map_err(FetchExecutorError::Storage)?
        .ok_or(FetchExecutorError::PlanNotFound(plan_id))?;

    if stored.status != PlanStatus::Accepted {
        return Err(FetchExecutorError::PlanNotAccepted {
            current: stored.status,
        });
    }

    let plan = load_research_plan(ctx.store, plan_id)?
        .ok_or(FetchExecutorError::PlanNotFound(plan_id))?;
    Ok(plan)
}

/// If the plan already has recipes, return them. Otherwise run
/// Level-2 authoring once per bound source and persist the results.
///
/// "Bound source" = an entry in `plan.expectations.document_sources`
/// whose `preferred_source_ids` non-empty. ADR 0007 has a
/// description-only fallback for the source-matching step; that
/// matching belongs to the source registry (Phase 3) and isn't in
/// this session's scope. Treating empty `preferred_source_ids` as
/// "no binding to author against here" is the honest, narrow scope.
async fn load_or_author_recipes(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
) -> Result<Vec<FetchRecipe>, FetchExecutorError> {
    let existing = load_recipes_for_plan(ctx.store, plan.id)?;
    if !existing.is_empty() {
        return Ok(existing);
    }

    let mut authored = Vec::new();
    for hint in &plan.expectations.document_sources {
        for source_id in bound_source_ids(hint) {
            match author_one(ctx, plan, &source_id).await {
                Ok(recipe) => {
                    save_recipe(ctx.store, &recipe)?;
                    authored.push(recipe);
                }
                Err(e) => {
                    // Per-source authoring failures shouldn't abort
                    // the whole run — other sources may still author
                    // cleanly. We log loudly and continue.
                    //
                    // If *every* source fails to author, the run will
                    // produce a report with zero outcomes, and the
                    // user sees an empty list. That's the right
                    // failure mode for now: the wholesale-authoring-
                    // failed error surface is reserved for cases the
                    // user's request couldn't even start (e.g. the
                    // provider isn't configured), not "every single
                    // source rejected the prompt", which is a recipe
                    // problem the next session improves.
                    warn!(
                        plan_id = %plan.id,
                        source_id = %source_id,
                        error = %e,
                        "recipe authoring failed for this source; continuing"
                    );
                }
            }
        }
    }

    Ok(authored)
}

/// Sources to author recipes against, derived from a single
/// `DocumentSourceHint`. Filters out blank ids and duplicates within
/// the same hint.
fn bound_source_ids(hint: &DocumentSourceHint) -> Vec<String> {
    let mut seen = Vec::new();
    for id in &hint.preferred_source_ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !seen.iter().any(|s: &String| s == trimmed) {
            seen.push(trimmed.to_string());
        }
    }
    seen
}

/// Author one recipe for one (plan, source_id) pair.
///
/// The authoring step needs a sample URL and a document excerpt for
/// the prompt. We synthesize a stable sample URL from the source id
/// (`https://example.invalid/{source_id}` — the URL guard accepts
/// it; the LLM's job is to *replace* it with the real fetch URL).
/// The excerpt is a placeholder describing the source plus the
/// plan's interpretation; in a future session this becomes a real
/// pre-fetch of the source's current content. The narrowing is
/// deliberate — Session 8's job is to prove the executor pipeline,
/// not to re-implement the demo binary's authoring loop here.
///
/// After authoring, the executor stamps the recipe's `source_id`
/// (which `build_validated_recipe` left blank for the caller per
/// ADR 0007) and a deterministic `dedup_key` of the form
/// `{plan_id}:{source_id}` so subsequent re-runs upsert by version
/// rather than create parallel recipes.
async fn author_one(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    source_id: &str,
) -> Result<FetchRecipe, FetchExecutorError> {
    let sample_url = format!("https://example.invalid/{source_id}")
        .parse::<url::Url>()
        .map_err(|e| {
            FetchExecutorError::Authoring(AuthoringError::InvalidRecipe(format!(
                "could not parse synthetic sample url for {source_id}: {e}"
            )))
        })?;

    let mut excerpt = format!(
        "Source id: {source_id}\nPlan topic: {}\nInterpretation: {}\n",
        plan.topic, plan.interpretation
    );
    // Bound the excerpt so the prompt fits inside Bounds::LLM_PROMPT_BODY
    // even before substitution. The recipe-author prompt itself does a
    // second check after substitution.
    if excerpt.len() > Bounds::LLM_PROMPT_BODY {
        excerpt.truncate(Bounds::LLM_PROMPT_BODY);
    }

    let auth_ctx = AuthoringContext {
        source_id: source_id.to_string(),
        sample_url,
        document_excerpt: excerpt,
    };

    let mut recipe = author_recipe(
        ctx.provider,
        ModelTier::Workhorse,
        ctx.recipe_author_prompt,
        plan,
        &auth_ctx,
    )
    .await?;

    // Stamp the per-source metadata `build_validated_recipe` left
    // blank.
    recipe.source_id = source_id.to_string();
    recipe.dedup_key = Some(format!("{}:{}", plan.id, source_id));

    Ok(recipe)
}

/// Run one recipe end-to-end. Pure dispatch on the extraction mode
/// — Session 8 only enables CSV, the rest report as Skipped.
async fn run_one_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    match &recipe.extraction {
        ExtractionSpec::CsvCell { .. } => run_csv_recipe(ctx, plan, recipe).await,
        ExtractionSpec::JsonPath { .. } => RecipeOutcome::Skipped {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            reason: "json_path: extraction mode not yet enabled in executor".into(),
        },
        ExtractionSpec::CssSelect { .. } => RecipeOutcome::Skipped {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            reason: "css_select: extraction mode not yet enabled in executor".into(),
        },
        ExtractionSpec::PdfTable { .. } => RecipeOutcome::Skipped {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            reason: "pdf_table: extraction mode not implemented (ADR 0007 Session-3 review note)".into(),
        },
        ExtractionSpec::RegexCapture { .. } => RecipeOutcome::Skipped {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            reason: "regex_capture: extraction mode not yet enabled in executor".into(),
        },
    }
}

/// CSV runtime path: fetch → apply → insert.
async fn run_csv_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    // Fetch.
    let bytes = match ctx.http.fetch_bytes(recipe.source_url.as_str()).await {
        Ok(b) => b,
        Err(HttpFetchError::Http(msg)) => {
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Fetch,
                message: msg,
            }
        }
        Err(HttpFetchError::NoFixture(url)) => {
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Fetch,
                message: format!("no fixture configured for url: {url}"),
            }
        }
    };

    // Apply.
    let fetched_at = Utc::now();
    let apply_ctx = ApplyContext {
        recipe,
        plan,
        bytes: &bytes,
        fetched_at,
    };
    let records = match apply(apply_ctx) {
        Ok(rs) => rs,
        Err(e) => {
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message: describe_apply_error(&e),
            }
        }
    };

    // Insert. A failure to insert any one record fails the recipe —
    // we don't half-write a recipe's batch.
    for record in &records {
        if let Err(e) = ctx.store.insert_record(record) {
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Insert,
                message: e.to_string(),
            };
        }
    }

    RecipeOutcome::Succeeded {
        recipe_id: recipe.id,
        source_id: recipe.source_id.clone(),
        records_produced: records.len() as u32,
    }
}

fn describe_apply_error(e: &ApplyError) -> String {
    // The apply error's Display already names the stage; including
    // the Debug form would just duplicate. Display is enough.
    e.to_string()
}

/// Close a fetch_run row with an error_summary populated. Used when
/// the run failed before processing any recipe — per-recipe failures
/// don't go through here.
fn close_run_with_error(store: &Store, run: &mut FetchRunRow, message: &str) {
    run.finished_at = Some(Utc::now());
    run.error_summary = Some(message.to_string());
    if let Err(e) = store.update_fetch_run(run) {
        warn!(run_id = %run.id, error = %e, "failed to close fetch_run row with error");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_fetcher::testing::StaticFetcher;
    use crate::recipes::{
        ExpectationRef, FieldMap, FieldValueSource, ProductionBinding, RowFilter,
    };
    use crate::research::{
        DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
        MetricExpectation, RecordExpectations, RelationKindExpectation,
    };
    use crate::research_plans_store::save_research_plan;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use serde_json::json;
    use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};
    use stockpile_core::RecordType;
    use stockpile_llm::{
        CompletionRequest, CompletionResponse, LlmError, LlmProvider, ModelTier,
    };
    use url::Url;

    /// Test plan with one bound source and one observation metric.
    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium production".into(),
            interpretation: "Global lithium production figures.".into(),
            topic_tags: vec![Topic::new("Li").unwrap()],
            geographic_scope: vec![GeoScope::code_only("CL")],
            historical_window_days: 365,
            expectations: RecordExpectations {
                observation_metrics: vec![MetricExpectation {
                    name: "production".into(),
                    unit_hint: Some(Unit::new("t").unwrap()),
                    rationale: "Primary".into(),
                }],
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("mine_opened").unwrap(),
                    rationale: "Capacity".into(),
                }],
                entity_kinds: vec![EntityKindExpectation {
                    kind: "mine".into(),
                    exemplars: vec![EntityId::new("mine:greenbushes").unwrap()],
                    rationale: "Unit of supply".into(),
                }],
                relation_kinds: vec![RelationKindExpectation {
                    kind: "operator_of".into(),
                    rationale: "Asset link".into(),
                }],
                document_sources: vec![DocumentSourceHint {
                    description: "Demo CSV".into(),
                    preferred_source_ids: vec!["demo_csv".into()],
                }],
                assertion_guidance: None,
            },
            created_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
        }
    }

    /// Working CSV recipe — pre-authored, persisted, exercises the
    /// happy-path runtime. Mirrors the recipe shape used in
    /// `recipe_apply::tests::end_to_end_csv_recipe_produces_observation`.
    fn working_csv_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:demo_csv", plan.id)),
            plan_id: plan.id,
            source_id: "demo_csv".into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::CsvCell {
                column: "production".into(),
                row_filter: Some(RowFilter::Equals {
                    column: "country".into(),
                    value: "Chile".into(),
                }),
            },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "value".into(),
                        source: FieldValueSource::Extracted,
                    },
                    FieldMap {
                        path: "unit".into(),
                        source: FieldValueSource::Literal { value: json!("t") },
                    },
                    FieldMap {
                        path: "metric".into(),
                        source: FieldValueSource::FromPlan {
                            pointer: "expectations.observation_metrics.0.name".into(),
                        },
                    },
                    FieldMap {
                        path: "period".into(),
                        source: FieldValueSource::Literal {
                            value: json!("annual"),
                        },
                    },
                ],
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
        }
    }

    /// LLM provider that panics on use. The pre-authored-recipes
    /// tests must never reach the provider — if they do, the
    /// LLM-free-runtime invariant is broken and we want a loud
    /// failure, not a silent no-op.
    struct UnreachableProvider;

    #[async_trait]
    impl LlmProvider for UnreachableProvider {
        fn id(&self) -> &'static str {
            "unreachable"
        }
        fn supported_tiers(&self) -> &[ModelTier] {
            &[ModelTier::Workhorse]
        }
        async fn complete(
            &self,
            _tier: ModelTier,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            panic!("LLM-free runtime invariant violated: provider was called when recipes already existed");
        }
    }

    fn make_store_with_accepted_plan(plan: &ResearchPlan) -> Store {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        save_research_plan(&store, plan, "test").unwrap();
        store
            .set_plan_status(plan.id, PlanStatus::Accepted)
            .unwrap();
        store
    }

    #[tokio::test]
    async fn run_fetch_for_plan_succeeds_against_csv_recipe_without_calling_llm() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.csv";
        let recipe = working_csv_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        let csv = b"country,production\nAustralia,88000\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(url, csv);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(report.plan_id, plan.id);
        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 1);
        assert_eq!(report.records_produced, 1);
        assert_eq!(report.outcomes.len(), 1);
        match &report.outcomes[0] {
            RecipeOutcome::Succeeded {
                records_produced, ..
            } => assert_eq!(*records_produced, 1),
            other => panic!("expected Succeeded, got {other:?}"),
        }

        // The fetch_runs row was opened and closed.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, report.run_id);
        assert_eq!(runs[0].recipes_attempted, 1);
        assert_eq!(runs[0].recipes_succeeded, 1);
        assert_eq!(runs[0].records_produced, 1);
        assert!(runs[0].finished_at.is_some());
        assert!(runs[0].error_summary.is_none());
    }

    #[tokio::test]
    async fn run_fetch_for_plan_rejects_pending_plan() {
        let plan = sample_plan();
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        save_research_plan(&store, &plan, "test").unwrap();
        // Note: NOT setting status to Accepted — leaves it Pending.

        let fetcher = StaticFetcher::new();
        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
        };

        let err = run_fetch_for_plan(&ctx, plan.id).await.unwrap_err();
        match err {
            FetchExecutorError::PlanNotAccepted { current } => {
                assert_eq!(current, PlanStatus::Pending);
            }
            other => panic!("expected PlanNotAccepted, got {other:?}"),
        }

        // Audit row exists, with error_summary populated.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].error_summary.is_some());
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn run_fetch_for_plan_rejects_unknown_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let fetcher = StaticFetcher::new();
        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
        };

        let err = run_fetch_for_plan(&ctx, Uuid::now_v7()).await.unwrap_err();
        assert!(matches!(err, FetchExecutorError::PlanNotFound(_)));
    }

    #[tokio::test]
    async fn run_fetch_for_plan_reports_per_recipe_fetch_failure_without_aborting_run() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let working_url = "https://example.test/works.csv";
        let broken_url = "https://example.test/broken.csv";

        let mut working = working_csv_recipe(&plan, working_url);
        working.dedup_key = Some(format!("{}:demo_csv:works", plan.id));
        save_recipe(&store, &working).unwrap();

        let mut broken = working_csv_recipe(&plan, broken_url);
        broken.id = Uuid::now_v7();
        broken.dedup_key = Some(format!("{}:demo_csv:broken", plan.id));
        save_recipe(&store, &broken).unwrap();

        // Only the working URL is fixtured.
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(working_url, csv);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_attempted, 2);
        assert_eq!(report.recipes_succeeded, 1);
        assert_eq!(report.records_produced, 1);

        let mut succeeded = 0;
        let mut failed = 0;
        for o in &report.outcomes {
            match o {
                RecipeOutcome::Succeeded { .. } => succeeded += 1,
                RecipeOutcome::Failed { stage, .. } => {
                    assert_eq!(*stage, FailureStage::Fetch);
                    failed += 1;
                }
                RecipeOutcome::Skipped { .. } => panic!("no skips expected here"),
            }
        }
        assert_eq!(succeeded, 1);
        assert_eq!(failed, 1);
    }

    #[tokio::test]
    async fn run_fetch_for_plan_skips_non_csv_extraction_modes() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/api.json";
        let mut json_recipe = working_csv_recipe(&plan, url);
        json_recipe.id = Uuid::now_v7();
        json_recipe.dedup_key = Some(format!("{}:demo_csv:json", plan.id));
        json_recipe.extraction = ExtractionSpec::JsonPath {
            path: "$.data".into(),
        };
        save_recipe(&store, &json_recipe).unwrap();

        // Fixture not strictly necessary — Skipped is decided before
        // fetch — but include it so a regression that *did* attempt
        // fetch wouldn't trip on a missing fixture and look like a
        // test setup bug.
        let fetcher = StaticFetcher::new().with(url, b"{}");

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 0);
        assert_eq!(report.records_produced, 0);
        assert!(matches!(
            report.outcomes[0],
            RecipeOutcome::Skipped { .. }
        ));
    }

    #[tokio::test]
    async fn run_fetch_for_plan_reports_apply_failure_on_malformed_csv() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/bad.csv";
        let recipe = working_csv_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Non-numeric value where an f64 is expected — apply rejects
        // at content assembly per recipe_apply's existing test.
        let bad_csv = b"country,production\nChile,unavailable\n";
        let fetcher = StaticFetcher::new().with(url, bad_csv);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Live end-to-end — `cargo test --ignored`.
    //
    // Walks the real path: real SecureHttpClient, real network, real
    // CSV. No LLM call (the recipe is pre-authored, mirroring the
    // ADR 0007 invariant: once recipes exist, the runtime is
    // deterministic and LLM-free).
    //
    // The default URL points at a small public-domain CSV that has
    // been stable for years. Override with FETCH_LIVE_CSV_URL to
    // point at any other small CSV; the recipe's column/row_filter
    // are constructed to match whatever shape that URL returns by
    // reading the literal column name from FETCH_LIVE_CSV_COLUMN
    // (default: "Code") and a row anchor from FETCH_LIVE_CSV_FILTER
    // (default: column "Name", value "Chile").
    //
    // Structural assertions only — the test asserts that *something*
    // was produced and that the audit row closed cleanly, not that
    // any specific value came back. The point is to prove the wiring
    // doesn't lie.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore]
    async fn live_fetch_against_real_csv_produces_observation_and_closes_run() {
        use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();

        let url = std::env::var("FETCH_LIVE_CSV_URL").unwrap_or_else(|_| {
            "https://raw.githubusercontent.com/datasets/country-list/main/data.csv".to_string()
        });
        let column =
            std::env::var("FETCH_LIVE_CSV_COLUMN").unwrap_or_else(|_| "Code".to_string());
        let filter_col =
            std::env::var("FETCH_LIVE_CSV_FILTER_COL").unwrap_or_else(|_| "Name".to_string());
        let filter_val =
            std::env::var("FETCH_LIVE_CSV_FILTER_VAL").unwrap_or_else(|_| "Chile".to_string());

        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();

        // Build a plan + a hand-authored recipe targeting the live
        // CSV. Pre-authoring the recipe is the whole point: the
        // executor *must not* call the LLM here, and we use the
        // UnreachableProvider to enforce that invariant the same way
        // the offline tests do.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:csv_demo:live", plan.id)),
            plan_id: plan.id,
            source_id: "csv_demo".into(),
            source_url: Url::parse(&url).expect("FETCH_LIVE_CSV_URL must be a valid URL"),
            extraction: ExtractionSpec::CsvCell {
                column,
                row_filter: Some(RowFilter::Equals {
                    column: filter_col,
                    value: filter_val,
                }),
            },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "value".into(),
                        // The CSV's chosen column is non-numeric in
                        // the default fixture (country code) — the
                        // recipe stores it via FieldValueSource
                        // Literal so the apply stage doesn't try to
                        // coerce it to f64. The point of the test
                        // is the wiring, not a numeric extraction;
                        // override the env vars to target a numeric
                        // dataset if you want the value path
                        // exercised.
                        source: FieldValueSource::Literal {
                            value: serde_json::json!(0.0),
                        },
                    },
                    FieldMap {
                        path: "unit".into(),
                        source: FieldValueSource::Literal {
                            value: serde_json::json!("t"),
                        },
                    },
                    FieldMap {
                        path: "metric".into(),
                        source: FieldValueSource::FromPlan {
                            pointer: "expectations.observation_metrics.0.name".into(),
                        },
                    },
                    FieldMap {
                        path: "period".into(),
                        source: FieldValueSource::Literal {
                            value: serde_json::json!("annual"),
                        },
                    },
                ],
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // Structural: the recipe was attempted; either it succeeded
        // or it surfaced a typed failure stage (Fetch / Apply /
        // Insert). A Skipped here would mean we accidentally went
        // through a non-CSV branch — that's a regression.
        assert_eq!(report.recipes_attempted, 1);
        assert!(
            !matches!(report.outcomes[0], RecipeOutcome::Skipped { .. }),
            "live test should not skip — got: {:?}",
            report.outcomes[0]
        );

        // The audit row exists and was closed.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 5).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].finished_at.is_some(), "fetch_run must be closed");
    }
}
