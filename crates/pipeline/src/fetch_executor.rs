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
//! ## Session 10, Option F — pre-fetch for authoring
//!
//! Before Session 10 the executor passed
//! `https://example.invalid/{source_id}` as both the sample URL and
//! a stub document excerpt to the Level-2 author. The Session 9
//! production run on "bulgaria elections 2026" revealed that the
//! LLM would echo the placeholder back into the recipe, producing
//! recipes that fetched `example.invalid` at runtime and failed at
//! the Fetch stage. (See `situation_room_HANDOFF_SESSION10.md` §"gdelt
//! → Failed @ Fetch" for the diagnosis.)
//!
//! Session 10 fixes this by:
//!
//! - Looking up the source's `SourceDescriptor::endpoint_hint` in
//!   the registered-sources slice (loaded from `config/sources.toml`).
//! - Pre-fetching the hint via the same `HttpFetcher` the runtime
//!   uses for recipe execution — one client, ADR 0009 §"The rule".
//! - Passing the real URL as `AuthoringContext::sample_url` and the
//!   pre-fetched bytes (UTF-8 lossy, truncated) as
//!   `AuthoringContext::document_excerpt`.
//!
//! The fallback discipline is conservative: a missing descriptor, a
//! missing `endpoint_hint`, or a failed pre-fetch all degrade
//! gracefully to the pre-Session-10 behaviour (placeholder URL +
//! stub excerpt) with a logged warning. The intent is "make
//! authoring better when we can", not "block authoring when we
//! can't".
//!
//! ## Extraction-mode policy in this session
//!
//! [`ExtractionSpec::CsvCell`], [`ExtractionSpec::JsonPath`],
//! [`ExtractionSpec::CssSelect`], and [`ExtractionSpec::RegexCapture`]
//! are wired through to apply + insert. The remaining mode
//! ([`ExtractionSpec::PdfTable`]) gets authored normally (Level-2
//! picks whatever fits the source) and is surfaced in the report as
//! `Skipped { reason }` rather than a failure — not a bug, a
//! deliberate phasing of work. This is the cheapest discipline that
//! keeps the executor honest about what it can and can't do without
//! conflating "didn't try" with "tried and broke".
//!
//! CssSelect was promoted in Session 12; RegexCapture in Session 13.
//! The recipe_apply runtime has supported every mode since Session 3
//! (via `csv`, `jsonpath_lib`, `scraper`, and `regex` respectively);
//! what was missing each time was the executor-level dispatch + the
//! apply-and-insert plumbing. The wiring is structurally identical
//! to the CSV and JSON paths because all of them go through the same
//! `apply()` boundary, which dispatches internally on the recipe's
//! `ExtractionSpec`.
//!
//! RegexCapture's promotion was prompted by a real Session-13
//! production run: a "EU AI Act enforcement" plan authored a
//! sensible regex against EUR-Lex's RSS feed XML, and the prior
//! `Skipped` outcome cost the only authored-and-runnable recipe of
//! the run. The handoff predicted RegexCapture would see "less
//! production use than CssSelect"; that was wrong — RSS+regex is a
//! legitimate first-class pattern for news/announcement feeds and
//! the LLM nominates it correctly.
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

use situation_room_llm::{LlmProvider, ModelTier};
use situation_room_secure::bounds::Bounds;
use situation_room_storage::{
    fetch_runs::FetchRunRow, research_plans::PlanStatus, Store,
};

use crate::http_fetcher::{FetchError as HttpFetchError, HttpFetcher};
use crate::recipe_apply::{apply, ApplyContext, ApplyError};
use crate::recipe_author::{author_recipe, AuthoringContext, AuthoringError};
use crate::recipes::{ExtractionSpec, FetchRecipe};
use crate::recipes_store::{load_recipes_for_plan, save_recipe, RecipeStoreError};
use crate::research::{DocumentSourceHint, ResearchPlan};
use crate::research_classifier::SourceDescriptor;
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
    Storage(#[from] situation_room_storage::StorageError),
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
    /// Registered source descriptors. The executor uses these at
    /// Level-2 authoring time to look up `endpoint_hint`s for the
    /// pre-fetch step (Session 10, Option F). An empty slice is
    /// legal: every author call falls back to the placeholder URL
    /// path, mirroring the pre-Session-10 behaviour.
    ///
    /// We take a slice (not a Vec) because the executor only needs
    /// to read; the binary owns the canonical `Vec<SourceDescriptor>`
    /// in `AppState`.
    pub sources: &'a [SourceDescriptor],
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

    // Flatten the (hint, source_id) pairs into a single Vec so we
    // know the total up-front. This is cosmetic — the resulting
    // authoring loop is identical to the prior nested form — but it
    // lets us emit a "N total sources to author" log at the top and
    // an "authoring N of M" log per iteration, which the Session 13
    // run identified as a real ergonomic gap (a 1m25s silent stretch
    // during multi-source authoring made the GUI look frozen).
    //
    // Order is preserved: hints in the order Level-1 emitted them,
    // and within each hint, sources in `preferred_source_ids` order.
    // This matters because Level-1's ordering reflects the source-
    // priority hierarchy the classifier was prompted to apply.
    let mut sources: Vec<String> = Vec::new();
    for hint in &plan.expectations.document_sources {
        for source_id in bound_source_ids(hint) {
            // bound_source_ids already deduplicates within a hint;
            // dedup across hints too so a source nominated by two
            // hints is authored once, not twice.
            if !sources.iter().any(|s| s == &source_id) {
                sources.push(source_id);
            }
        }
    }

    let total = sources.len();
    info!(
        plan_id = %plan.id,
        total_sources = total,
        "authoring recipes for plan: starting"
    );

    let mut authored = Vec::new();
    for (idx, source_id) in sources.iter().enumerate() {
        let position = idx + 1;
        info!(
            plan_id = %plan.id,
            source_id = %source_id,
            position,
            total,
            "authoring source"
        );
        match author_one(ctx, plan, source_id).await {
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
                    position,
                    total,
                    error = %e,
                    "recipe authoring failed for this source; continuing"
                );
            }
        }
    }

    info!(
        plan_id = %plan.id,
        total_sources = total,
        succeeded = authored.len(),
        "authoring recipes for plan: complete"
    );

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

/// Maximum number of bytes from a pre-fetched source document that we
/// shove into the recipe-author prompt. The recipe-author prompt is
/// ultimately bounded by `Bounds::LLM_PROMPT_BODY` (256 KiB), which
/// also has to fit the prompt template, the plan JSON, the source
/// metadata, and any future additions. 32 KiB leaves comfortable
/// headroom while being more than enough excerpt for the LLM to
/// recognize the source's shape.
///
/// Bumping this is fine, but check `build_prompt`'s post-substitution
/// bound check first; the prompt + plan + excerpt together must stay
/// under `Bounds::LLM_PROMPT_BODY`.
const PREFETCH_EXCERPT_BUDGET: usize = 32 * 1024;

/// Author one recipe for one (plan, source_id) pair.
///
/// This is the only function in the executor that calls the LLM. It
/// runs at most once per (plan, bound source) pair — see the
/// `load_or_author_recipes` callers — and the result is persisted so
/// subsequent runs of the same plan don't re-author.
///
/// ## What the LLM sees (Session 10, Option F)
///
/// The author needs three things to do its job well: (a) what the
/// research is about, (b) where the data lives, and (c) what shape
/// it has. (a) comes from `plan`. (b) and (c) are the ones Session
/// 10 fixes:
///
/// - **(b) The URL.** If the source's `SourceDescriptor` has an
///   `endpoint_hint`, we use that as `AuthoringContext::sample_url`.
///   Otherwise we synthesize `https://example.invalid/{source_id}`
///   as a placeholder — same as pre-Session-10. The placeholder
///   path is *not* removed because some sources are well-known
///   enough that the LLM can author against the description alone
///   (the sources.toml prose, e.g. SEC EDGAR), and we don't want a
///   missing hint to be a hard error.
///
/// - **(c) The excerpt.** When `endpoint_hint` exists *and* the
///   pre-fetch succeeds, the excerpt is the source's actual current
///   content (UTF-8 lossy, truncated to `PREFETCH_EXCERPT_BUDGET`).
///   When the pre-fetch fails (network error, DNS failure, response
///   too large, server returned an error status), we log the failure
///   and fall back to a stub excerpt — but we still pass the real
///   `endpoint_hint` URL as the sample, so the LLM at least has a
///   real target to author against.
///
/// ## Why fall back rather than error
///
/// A pre-fetch failure is an external condition (network down,
/// rate-limited, geo-blocked); aborting authoring would mean the
/// user can never recover without restarting the run. Falling back
/// preserves the pre-Session-10 behaviour exactly: the LLM authors
/// from the description alone. If the LLM produces a usable recipe
/// anyway, the user gets a working pipeline; if not, the
/// `RecipeOutcome::Failed { stage: Fetch | Apply }` surfaces in the
/// report and the user can re-run.
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
    // Look up the descriptor. A missing one isn't an error — we
    // just lose the chance to pre-fetch.
    let descriptor = ctx.sources.iter().find(|s| s.id == source_id);
    if descriptor.is_none() {
        warn!(
            source_id = %source_id,
            "no SourceDescriptor registered for source_id; falling back to placeholder url + stub excerpt"
        );
    }

    // Resolve the sample URL. Priority:
    //   1. descriptor's endpoint_hint (if parseable),
    //   2. synthetic placeholder.
    //
    // Parsing happens up-front so a malformed hint surfaces as a
    // warning and falls back, rather than crashing authoring later.
    let (sample_url, hint_for_prefetch) = match descriptor.and_then(|d| d.endpoint_hint.as_deref()) {
        Some(hint) => match hint.parse::<url::Url>() {
            Ok(u) => (u.clone(), Some(u)),
            Err(e) => {
                warn!(
                    source_id = %source_id,
                    hint = %hint,
                    error = %e,
                    "endpoint_hint failed to parse; falling back to placeholder url"
                );
                (placeholder_url(source_id)?, None)
            }
        },
        None => (placeholder_url(source_id)?, None),
    };

    // Build the document excerpt. Prefer real bytes from the
    // endpoint_hint; fall back to a stub describing the source.
    //
    // ADR 0014: which branch we took here is the load-bearing
    // signal for `authored_from`. We track it as a boolean
    // alongside the excerpt and stamp the recipe after authoring.
    // Three sub-cases all collapse to the same StubExcerpt outcome:
    //   * no descriptor / no endpoint_hint at all (`hint_for_prefetch
    //     == None`),
    //   * endpoint_hint present but pre-fetch returned None (network
    //     error, 4xx/5xx, body too large),
    //   * endpoint_hint unparseable (already replaced with placeholder
    //     above; arrives here as `hint_for_prefetch == None`).
    // The FetchedBytes case is the single happy path: prefetch_excerpt
    // returned Some.
    let (excerpt, used_real_bytes) = match &hint_for_prefetch {
        Some(url) => match prefetch_excerpt(ctx, url, source_id).await {
            Some(real) => (real, true),
            None => (stub_excerpt(plan, source_id, Some(url.as_str())), false),
        },
        None => (stub_excerpt(plan, source_id, None), false),
    };

    // Look up any operator feedback the user attached to this
    // (plan, source) pair via the recipe-inspection panel. ADR 0013:
    // the feedback persists across re-authoring (keyed by plan_id +
    // source_id, not recipe_id), so even after a `dedup_key`-bumped
    // version rotation the next authoring call still sees the
    // operator's correction. A storage error here is non-fatal —
    // we log and continue with no feedback rather than aborting
    // authoring, because feedback is a hint, not a precondition.
    let recipe_feedback = match ctx
        .store
        .recipe_feedback_for_source(plan.id, source_id)
    {
        Ok(Some(stored)) => Some(stored.note),
        Ok(None) => None,
        Err(e) => {
            warn!(
                plan_id = %plan.id,
                source_id = %source_id,
                error = %e,
                "recipe_feedback lookup failed; authoring will proceed without operator feedback"
            );
            None
        }
    };

    let auth_ctx = AuthoringContext {
        source_id: source_id.to_string(),
        sample_url,
        document_excerpt: excerpt,
        recipe_feedback,
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
    // ADR 0014: stamp the authoring provenance signal. The
    // validator left it `Unknown`; here is the only place that
    // knows the truth, derived from the same branch the excerpt
    // came from a few lines up. A visible `info` log makes the
    // signal observable in the executor's tracing output without
    // requiring the operator to look at the recipes panel.
    recipe.authored_from = if used_real_bytes {
        situation_room_storage::AuthoredFrom::FetchedBytes
    } else {
        situation_room_storage::AuthoredFrom::StubExcerpt
    };
    info!(
        plan_id = %plan.id,
        source_id = %source_id,
        recipe_id = %recipe.id,
        authored_from = recipe.authored_from.as_str(),
        "recipe authored; provenance stamped"
    );

    Ok(recipe)
}

/// Synthesize a placeholder URL for sources without an
/// `endpoint_hint`. The URL guard accepts `example.invalid` (it's a
/// reserved-for-testing TLD), but the recipe author is told via the
/// prompt that it must replace the placeholder with the source's
/// real URL — the LLM, given a strong enough description, can
/// usually do that for well-known sources.
///
/// This was the only authoring URL strategy before Session 10. It
/// remains as the fallback for un-hinted sources because removing it
/// would force every entry in `config/sources.toml` to declare an
/// `endpoint_hint` even when the LLM doesn't need it, and that's a
/// step we want to take when forced to, not preemptively.
fn placeholder_url(source_id: &str) -> Result<url::Url, FetchExecutorError> {
    format!("https://example.invalid/{source_id}")
        .parse::<url::Url>()
        .map_err(|e| {
            FetchExecutorError::Authoring(AuthoringError::InvalidRecipe(format!(
                "could not parse synthetic sample url for {source_id}: {e}"
            )))
        })
}

/// Fetch the endpoint hint and return a bounded UTF-8 excerpt, or
/// `None` if the fetch failed. Failure is logged at warn level; the
/// caller decides what to do with the absence.
///
/// We read up to `PREFETCH_EXCERPT_BUDGET` bytes. The HTTP layer
/// already enforces a much larger ceiling (`max_response_bytes`); the
/// budget here is about prompt size, not about defending the network
/// layer.
async fn prefetch_excerpt(
    ctx: &ExecutorContext<'_>,
    url: &url::Url,
    source_id: &str,
) -> Option<String> {
    // Operator-visible "we're now fetching X" log. The Session 13
    // run had a 1m25s silent stretch that included the time spent
    // pre-fetching; this turns it into a visible step rather than a
    // mystery wait.
    info!(
        source_id = %source_id,
        url = %url,
        "pre-fetching endpoint hint"
    );
    let bytes = match ctx.http.fetch_bytes(url.as_str()).await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                source_id = %source_id,
                url = %url,
                error = %e,
                "endpoint_hint pre-fetch failed; authoring will fall back to stub excerpt"
            );
            return None;
        }
    };

    // Truncate at `PREFETCH_EXCERPT_BUDGET` *bytes*, not chars. The
    // LLM tokenizer doesn't care about UTF-8 boundaries; we use
    // `from_utf8_lossy` to handle the cut cleanly.
    let byte_count = bytes.len();
    let trimmed = if byte_count > PREFETCH_EXCERPT_BUDGET {
        &bytes[..PREFETCH_EXCERPT_BUDGET]
    } else {
        &bytes[..]
    };
    let body = String::from_utf8_lossy(trimmed).into_owned();

    let truncated_marker = if byte_count > PREFETCH_EXCERPT_BUDGET {
        format!(
            "\n\n[... excerpt truncated at {PREFETCH_EXCERPT_BUDGET} bytes; original was {byte_count} bytes ...]"
        )
    } else {
        String::new()
    };

    Some(format!(
        "Source id: {source_id}\nFetched URL: {url}\nFetched bytes: {byte_count}\n\n--- begin excerpt ---\n{body}{truncated_marker}\n--- end excerpt ---\n"
    ))
}

/// Build a stub excerpt for cases where pre-fetch is impossible
/// (no descriptor, no endpoint_hint, fetch failed). When we have a
/// real URL but no body, we surface the URL so the LLM still has a
/// concrete target — that alone often produces a usable recipe for
/// well-known sources.
fn stub_excerpt(plan: &ResearchPlan, source_id: &str, real_url: Option<&str>) -> String {
    let topic = &plan.topic;
    let interp = &plan.interpretation;
    let mut out = format!(
        "Source id: {source_id}\nPlan topic: {topic}\nInterpretation: {interp}\n"
    );
    if let Some(u) = real_url {
        out.push_str(&format!(
            "Documented endpoint (pre-fetch failed; author against this URL pattern): {u}\n"
        ));
    } else {
        out.push_str(
            "(no documented endpoint registered for this source; author from the description alone)\n",
        );
    }
    // Bound the stub the same way the original code did, even though
    // it's already much smaller than LLM_PROMPT_BODY — defense in
    // depth.
    if out.len() > Bounds::LLM_PROMPT_BODY {
        out.truncate(Bounds::LLM_PROMPT_BODY);
    }
    out
}

/// Run one recipe end-to-end. Pure dispatch on the extraction mode
/// — Session 8 wired CSV; Session 9 added JSON; Session 12 added
/// CssSelect; Session 13 added RegexCapture. The only remaining
/// unwired mode (PdfTable) is reported as `Skipped` until it lands
/// in its own session — it carries enough complexity (PDF table
/// detection libraries, page rasterization, positional addressing)
/// to deserve careful design.
async fn run_one_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    match &recipe.extraction {
        ExtractionSpec::CsvCell { .. } => run_csv_recipe(ctx, plan, recipe).await,
        ExtractionSpec::JsonPath { .. } => run_json_recipe(ctx, plan, recipe).await,
        ExtractionSpec::CssSelect { .. } => run_css_recipe(ctx, plan, recipe).await,
        ExtractionSpec::RegexCapture { .. } => run_regex_recipe(ctx, plan, recipe).await,
        ExtractionSpec::PdfTable { .. } => RecipeOutcome::Skipped {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            reason: "pdf_table: extraction mode not implemented (ADR 0007 Session-3 review note)".into(),
        },
    }
}

/// CSV runtime path: fetch → apply → insert.
async fn run_csv_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    // Fetch — or, if the recipe carries a baked `static_payload`,
    // skip the HTTP fetch and feed the baked bytes to apply().
    // ADR 0007 Amendment 3: the bytes' provenance is orthogonal to
    // the extraction mode. This short-circuit is duplicated at all
    // four run_X_recipe sites rather than extracted, preserving the
    // dispatch-contract readability per Session 9's
    // "duplication-with-comments over premature unification" rule.
    let bytes = if let Some(payload) = recipe.static_payload.as_ref() {
        payload.as_bytes().to_vec()
    } else {
        match ctx.http.fetch_bytes(recipe.source_url.as_str()).await {
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

/// JSON runtime path: fetch → apply → insert.
///
/// Structurally identical to [`run_csv_recipe`] — both go through the
/// same `apply()` boundary, which dispatches internally on the recipe's
/// `ExtractionSpec`. The two functions exist as separate dispatch
/// targets because (a) it keeps `run_one_recipe` honest about which
/// modes are wired, and (b) when the modes start to diverge in
/// behaviour (e.g. JSON gaining streamed parsing, CSV gaining row-set
/// extraction), the split lets each path evolve without a
/// flag-soup-inside-one-function. If you find yourself collapsing
/// these into one helper, first ask whether the dispatch contract
/// from `run_one_recipe` would still be readable — Session 9 chose
/// duplication-with-comments over premature unification.
async fn run_json_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    // Fetch — or short-circuit on baked `static_payload`. See the
    // comment in `run_csv_recipe` for the full ADR 0007 Amendment 3
    // reasoning. Inlined here rather than extracted per Session 9's
    // duplication-with-comments rule.
    let bytes = if let Some(payload) = recipe.static_payload.as_ref() {
        payload.as_bytes().to_vec()
    } else {
        match ctx.http.fetch_bytes(recipe.source_url.as_str()).await {
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
    // we don't half-write a recipe's batch. Same discipline as the
    // CSV path.
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

/// CSS runtime path: fetch → apply → insert.
///
/// Structurally identical to [`run_csv_recipe`] and [`run_json_recipe`]
/// — all three go through the same `apply()` boundary, which
/// dispatches internally on the recipe's `ExtractionSpec`. Promoted
/// from `Skipped` in Session 12. The duplication-with-comments
/// discipline that Session 9 chose for the CSV/JSON split applies
/// here too: keeping the dispatch in `run_one_recipe` honest about
/// which modes are wired is worth more than the line-saving of a
/// generic helper, especially while modes may still diverge in
/// behaviour (CssSelect could grow attribute-vs-text rendering
/// concerns at the executor level later).
async fn run_css_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    // Fetch — or short-circuit on baked `static_payload`. See the
    // comment in `run_csv_recipe` for the full ADR 0007 Amendment 3
    // reasoning. Inlined here rather than extracted per Session 9's
    // duplication-with-comments rule.
    let bytes = if let Some(payload) = recipe.static_payload.as_ref() {
        payload.as_bytes().to_vec()
    } else {
        match ctx.http.fetch_bytes(recipe.source_url.as_str()).await {
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
    // we don't half-write a recipe's batch. Same discipline as the
    // CSV and JSON paths.
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

/// RegexCapture runtime path: fetch → apply → insert.
///
/// Structurally identical to [`run_csv_recipe`], [`run_json_recipe`],
/// and [`run_css_recipe`] — the dispatch on `ExtractionSpec` happens
/// inside `apply()`, not here. The reason this still lives as a
/// standalone helper rather than being collapsed into a shared
/// "fetch-apply-insert" function is preserved across modes for
/// failure-mode legibility: each mode has its own call site so a
/// future "Class X failure shows up in mode Y but not Z" diagnosis
/// has an obvious place to add per-mode hooks (timing, mode-specific
/// fixture paths, mode-specific retry policies). When that
/// diagnosis never materialises across multiple sessions the right
/// move is consolidation; today the duplication earns its keep.
///
/// The mode is well-suited to RSS / news feeds and other XML-ish
/// content where extraction is a literal regex against the bytes
/// rather than structural navigation. The Session 13 production run
/// against EUR-Lex's `/news/rss.xml` was the prompt.
async fn run_regex_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
) -> RecipeOutcome {
    // Fetch — or short-circuit on baked `static_payload`. See the
    // comment in `run_csv_recipe` for the full ADR 0007 Amendment 3
    // reasoning. Inlined here rather than extracted per Session 9's
    // duplication-with-comments rule.
    let bytes = if let Some(payload) = recipe.static_payload.as_ref() {
        payload.as_bytes().to_vec()
    } else {
        match ctx.http.fetch_bytes(recipe.source_url.as_str()).await {
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
    // we don't half-write a recipe's batch. Same discipline as the
    // CSV, JSON, and CSS paths.
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
    use situation_room_core::vocab::{EntityId, EventType, Topic, Unit};
    use situation_room_core::RecordType;
    use situation_room_llm::{
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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
        }
    }

    /// Working JSON recipe — pre-authored, persisted, exercises the
    /// JSON happy-path runtime. Mirrors `working_csv_recipe` in
    /// shape; only `extraction` differs. The `produces` binding is
    /// identical because both extractors produce a single scalar
    /// string that flows through the same field-mapping discipline
    /// in `apply()`.
    fn working_json_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:demo_json", plan.id)),
            plan_id: plan.id,
            source_id: "demo_json".into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::JsonPath {
                path: "$.data.production.chile".into(),
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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
        }
    }

    /// Working CSS recipe — pre-authored, persisted, exercises the
    /// CssSelect happy-path runtime promoted in Session 12. Mirrors
    /// `working_csv_recipe` and `working_json_recipe` in shape; only
    /// `extraction` differs. The `produces` binding is identical
    /// because the CssSelect extractor produces a single scalar
    /// string (the matched element's text or attribute) that flows
    /// through the same field-mapping discipline in `apply()`.
    fn working_css_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:demo_css", plan.id)),
            plan_id: plan.id,
            source_id: "demo_css".into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: "td.prod".into(),
                attribute: None,
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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
        }
    }

    /// Build a working RegexCapture recipe — extracts the production
    /// figure from a one-line plain-text body via a single capture
    /// group. Mirrors the CSV/JSON/CSS helpers in shape; only the
    /// `extraction` field varies.
    ///
    /// The chosen pattern is deliberately simple — `recipe_apply`
    /// already has rich tests for the regex extractor; what these
    /// fetch_executor tests need is a recipe that flows cleanly
    /// through fetch → apply → insert.
    fn working_regex_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:demo_regex", plan.id)),
            plan_id: plan.id,
            source_id: "demo_regex".into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::RegexCapture {
                pattern: r"production:\s*(\d+)".into(),
                group: 1,
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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
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
            sources: &[],
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

    /// ADR 0007 Amendment 3 (Session 18): when a recipe carries
    /// `static_payload`, the runtime serves the baked bytes to
    /// extraction in place of an HTTP fetch.
    ///
    /// This test configures a `StaticFetcher` with **zero** fixtures.
    /// If the executor were to call `fetch_bytes()` for any URL, the
    /// fetcher would return `NoFixture` and the recipe would land as
    /// `Failed { stage: Fetch }`. The fact that this test asserts a
    /// `Succeeded` outcome with a record produced means the
    /// short-circuit at the byte-acquisition site engaged correctly
    /// — the HTTP fetcher was never asked for the URL.
    ///
    /// The recipe's `extraction` is `csv_cell` so `apply()` runs
    /// against the baked CSV bytes exactly as it would against
    /// network-fetched bytes. ADR 0007 A3 §"bytes' provenance is
    /// orthogonal to extraction mode" — proven here.
    #[tokio::test]
    async fn run_fetch_for_plan_uses_static_payload_without_calling_http() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Recipe shape mirrors `working_csv_recipe` but the URL is a
        // sentinel that *no fixture answers* — and the recipe carries
        // baked CSV bytes that apply() can extract from.
        let url = "https://example.test/baked-only.csv";
        let mut recipe = working_csv_recipe(&plan, url);
        recipe.static_payload = Some(
            "country,production\nAustralia,88000\nChile,49000\n".into(),
        );
        save_recipe(&store, &recipe).unwrap();

        // Zero fixtures: any HTTP fetch attempt surfaces as Failed.
        let fetcher = StaticFetcher::new();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipe already authored",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 1,
            "static_payload short-circuit must engage; if recipes_succeeded is 0 \
             the executor likely called fetch_bytes() and got NoFixture: {:?}",
             report.outcomes);
        assert_eq!(report.records_produced, 1);
        match &report.outcomes[0] {
            RecipeOutcome::Succeeded { records_produced, .. } => {
                assert_eq!(*records_produced, 1);
            }
            other => panic!(
                "expected Succeeded (short-circuit engaged), got: {other:?}"
            ),
        }
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
            sources: &[],
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
            sources: &[],
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
            sources: &[],
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
    async fn run_fetch_for_plan_succeeds_against_json_recipe_without_calling_llm() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.json";
        let recipe = working_json_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Mirrors the shape from the recipe_apply JSON path tests:
        // the path `$.data.production.chile` extracts the scalar
        // 49000, which flows into the Observation's `value` field.
        let body = br#"{"data": {"production": {"chile": 49000, "australia": 88000}}}"#;
        let fetcher = StaticFetcher::new().with(url, body);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
            sources: &[],
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

        // The fetch_runs row was opened and closed cleanly — same
        // discipline as the CSV path.
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
    async fn run_fetch_for_plan_reports_apply_failure_on_malformed_json() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/bad.json";
        let recipe = working_json_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Path matches nothing in this body — the JSON extractor
        // surfaces an `ApplyError::Extraction { mode: "json_path" }`,
        // which the executor maps to `FailureStage::Apply`.
        let bad_body = br#"{"unrelated": 1}"#;
        let fetcher = StaticFetcher::new().with(url, bad_body);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_fetch_for_plan_succeeds_against_css_recipe_without_calling_llm() {
        // Session 12 happy-path: CssSelect promoted from Skipped to a
        // first-class wired mode. Mirrors the CSV and JSON success
        // tests structurally; the only meaningful differences are the
        // recipe's `extraction` variant and the body bytes (HTML
        // instead of CSV/JSON).
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.html";
        let recipe = working_css_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // The selector `td.prod` matches the cell whose text is
        // `49,000`. `parse_extracted_scalar` strips the comma and
        // produces `49000.0`, which flows into the Observation's
        // `value` field — same end-state as the CSV / JSON paths.
        let html =
            b"<html><body><table><tr><td class='prod'>49,000</td></tr></table></body></html>";
        let fetcher = StaticFetcher::new().with(url, html);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
            sources: &[],
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

        // The fetch_runs row was opened and closed cleanly — same
        // discipline as the CSV and JSON paths.
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
    async fn run_fetch_for_plan_reports_apply_failure_on_unmatched_css_selector() {
        // Failure-shape coverage for the new CssSelect arm: when the
        // selector matches nothing in the fetched HTML, `apply()`
        // surfaces `ApplyError::Extraction { mode: "css_select" }`,
        // which the executor maps to `FailureStage::Apply`. Mirrors
        // the malformed-CSV and malformed-JSON apply-failure tests.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/empty.html";
        let recipe = working_css_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Body parses as HTML but the recipe's `td.prod` selector
        // matches no elements — extraction errors at the apply stage.
        let bad_html = b"<html><body><p>nothing here</p></body></html>";
        let fetcher = StaticFetcher::new().with(url, bad_html);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_fetch_for_plan_succeeds_against_regex_recipe_without_calling_llm() {
        // Session 13 happy-path: RegexCapture promoted from Skipped
        // to a first-class wired mode. Mirrors the CSV / JSON / CSS
        // success tests structurally; the only meaningful difference
        // is the recipe's `extraction` variant and the body bytes.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/feed.txt";
        let recipe = working_regex_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // The pattern `production:\s*(\d+)` captures `49000` from the
        // body. `parse_extracted_scalar` parses it as an f64 which
        // flows into the Observation's `value` field — same end-state
        // as the CSV / JSON / CSS paths.
        let body = b"daily report -- production: 49000 metric tons";
        let fetcher = StaticFetcher::new().with(url, body);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
            sources: &[],
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

        // The fetch_runs row was opened and closed cleanly — same
        // discipline as the CSV / JSON / CSS paths.
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
    async fn run_fetch_for_plan_reports_apply_failure_on_unmatched_regex_pattern() {
        // Failure-shape coverage for the new RegexCapture arm: when
        // the pattern matches nothing in the fetched body, `apply()`
        // surfaces `ApplyError::Extraction { mode: "regex_capture" }`,
        // which the executor maps to `FailureStage::Apply`. Mirrors
        // the malformed-CSV, malformed-JSON, and unmatched-CSS apply-
        // failure tests.
        //
        // This is the failure mode a real-world regex recipe most
        // often hits — the LLM authors a sensible-looking pattern
        // against the description of the source's content but the
        // actual fetched bytes have a slightly different format. The
        // user diagnoses via the fetch report's failure detail.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/empty.txt";
        let recipe = working_regex_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Body has no occurrence of `production:`, so the recipe's
        // `production:\s*(\d+)` pattern matches nothing — apply
        // errors at the extraction stage.
        let bad_body = b"daily report -- nothing relevant here";
        let fetcher = StaticFetcher::new().with(url, bad_body);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_fetch_for_plan_skips_unwired_extraction_modes() {
        // Coverage regression: confirms the dispatch arm for the only
        // remaining unwired mode (PdfTable) still emits Skipped rather
        // than silently going through a wired path. The canary's
        // history walks the project's extraction-mode promotion
        // sequence:
        //
        //   - Sessions 8–11: CssSelect was the canary (CSV, JSON wired).
        //   - Session 12: CssSelect promoted; RegexCapture took over.
        //   - Session 13: RegexCapture promoted; PdfTable is now it.
        //
        // PdfTable is the last unwired mode and will probably stay
        // that way for several sessions — it carries enough complexity
        // (PDF table-detection libraries, page rasterization,
        // positional addressing) to deserve careful design. When
        // PdfTable lands, this test goes away or becomes a happy-path
        // test for PdfTable with the canary role retiring entirely
        // (the closed extraction-mode enum has only the five we have
        // today; ADR 0007).
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/page.pdf";
        let mut pdf_recipe = working_csv_recipe(&plan, url);
        pdf_recipe.id = Uuid::now_v7();
        pdf_recipe.dedup_key = Some(format!("{}:demo_pdf", plan.id));
        pdf_recipe.extraction = ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 3,
            col: 1,
        };
        save_recipe(&store, &pdf_recipe).unwrap();

        // Fixture not strictly necessary — Skipped is decided before
        // fetch — but include it so a regression that *did* attempt
        // fetch wouldn't trip on a missing fixture and look like a
        // test setup bug.
        let fetcher = StaticFetcher::new().with(url, b"%PDF-1.4 stub");

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            sources: &[],
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
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Session 10, Option F — pre-fetch for Level-2 authoring.
    //
    // These tests exercise `author_one` indirectly through
    // `run_fetch_for_plan` by constructing a plan that has a bound
    // source but no pre-authored recipes — so `load_or_author_recipes`
    // falls into the authoring branch and calls the provider once
    // per bound source.
    //
    // The provider is a `RecordingProvider` that captures the
    // user-message prompt it receives and returns a fixed valid
    // `RecipeAuthoringOutput`. We assert on what the provider saw
    // (excerpt content, sample URL) rather than on what the runtime
    // produced — the runtime's behaviour with the resulting recipe is
    // covered by the existing CSV/JSON happy-path tests.
    // -----------------------------------------------------------------------

    /// Test provider that records the prompts it receives and returns
    /// a hardcoded recipe-authoring output. Unlike `UnreachableProvider`,
    /// this one is *meant* to be called — the tests below assert that
    /// `author_one` reaches it with the expected prompt content.
    ///
    /// We use a `Mutex<Vec<_>>` rather than `tokio::sync::Mutex` because
    /// the recording happens inside the synchronous `complete` body
    /// before any await; the std lock never spans an await point.
    struct RecordingProvider {
        recorded_prompts: std::sync::Mutex<Vec<String>>,
        canned_output: serde_json::Value,
    }

    impl RecordingProvider {
        fn new() -> Self {
            // A minimal valid `RecipeAuthoringOutput` JSON. The URL
            // points at a real-looking host so URL-guard validation
            // passes; the extraction is `csv_cell` because that's the
            // simplest mode whose runtime path is fully wired.
            let canned = serde_json::json!({
                "source_url": "https://api.example.com/data.csv",
                "extraction": {
                    "mode": "csv_cell",
                    "column": "production",
                    "row_filter": null
                },
                "produces": [{
                    "record_type": "observation",
                    "expectation": { "list": "observation_metric", "index": 0 },
                    "field_mappings": [
                        { "path": "value", "source": { "kind": "extracted" } },
                        { "path": "unit", "source": { "kind": "literal", "value": "t" } },
                        { "path": "metric", "source": { "kind": "from_plan",
                            "pointer": "expectations.observation_metrics.0.name" } },
                        { "path": "period", "source": { "kind": "literal", "value": "annual" } }
                    ]
                }]
            });
            Self {
                recorded_prompts: std::sync::Mutex::new(Vec::new()),
                canned_output: canned,
            }
        }

        fn last_prompt(&self) -> String {
            self.recorded_prompts
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }

        fn call_count(&self) -> usize {
            self.recorded_prompts.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        fn id(&self) -> &'static str {
            "recording"
        }
        fn supported_tiers(&self) -> &[ModelTier] {
            &[ModelTier::Workhorse]
        }
        async fn complete(
            &self,
            _tier: ModelTier,
            req: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            self.recorded_prompts.lock().unwrap().push(req.user.clone());
            Ok(CompletionResponse {
                text: serde_json::to_string(&self.canned_output).unwrap(),
                structured: Some(self.canned_output.clone()),
                provider: "recording".into(),
                model: "recording-test".into(),
                // Token usage is "best effort" per the trait docs;
                // None is the honest value for a test double.
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    /// A minimal recipe-author prompt template for offline tests. The
    /// real prompt at `config/prompts/recipe_author.md` is far longer;
    /// we only need the placeholders to be substituted so we can
    /// assert what the LLM saw.
    const TEST_AUTHOR_PROMPT: &str = "PLAN={{PLAN_JSON}}\nID={{SOURCE_ID}}\nURL={{SOURCE_URL}}\nEXCERPT={{DOCUMENT_EXCERPT}}\n";

    #[tokio::test]
    async fn author_one_uses_endpoint_hint_url_and_prefetched_excerpt() {
        // Session 10, Option F happy path: the source has an
        // endpoint_hint, the pre-fetch returns real bytes, the prompt
        // the LLM sees contains those bytes verbatim and references
        // the real URL — not `example.invalid`.
        let plan = sample_plan(); // has document_sources -> "demo_csv"
        let store = make_store_with_accepted_plan(&plan);

        let hint_url = "https://api.example.com/csv-demo.csv";
        // The pre-fetch body and the recipe-execution body don't
        // need to be the same; the assertions only require that the
        // pre-fetch body lands in the prompt. We use distinct
        // bodies so they're easy to reason about:
        //   - `hint_body` contains "Chile,49000" so the test asserts
        //     the prefetched bytes appear in the prompt.
        //   - `recipe_body` is a *single-row* CSV (no header twice,
        //     just one data row) so the canned `csv_cell` recipe —
        //     which has no `row_filter` — extracts unambiguously.
        //     `recipe_apply::csv_cell_errors_on_ambiguous_multi_row_without_filter`
        //     covers the other branch; this test wants the success
        //     path so we can assert `recipes_succeeded == 1`.
        let hint_body = b"country,production\nChile,49000\nAustralia,88000\n";
        let recipe_body = b"country,production\nChile,49000\n";

        let canned_recipe_url = "https://api.example.com/data.csv";
        let fetcher = StaticFetcher::new()
            .with(hint_url, hint_body)
            .with(canned_recipe_url, recipe_body);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some(hint_url.into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // Authoring happened exactly once (one bound source).
        assert_eq!(provider.call_count(), 1);

        let prompt = provider.last_prompt();
        // The prompt's URL line refers to the endpoint_hint, not the
        // synthetic placeholder.
        assert!(
            prompt.contains(hint_url),
            "prompt should reference endpoint_hint URL; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("example.invalid"),
            "prompt should not contain example.invalid placeholder; got:\n{prompt}"
        );
        // The pre-fetched body is in the excerpt.
        assert!(
            prompt.contains("Chile,49000"),
            "prompt should contain pre-fetched body; got:\n{prompt}"
        );

        // The run completed; one recipe authored, one record produced.
        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 1);
        assert_eq!(report.records_produced, 1);
    }

    #[tokio::test]
    async fn author_one_falls_back_to_placeholder_when_no_endpoint_hint() {
        // Session 10, Option F fallback: descriptor exists but has no
        // endpoint_hint. The pre-fetch is skipped entirely; the
        // synthesized placeholder URL goes through (matches
        // pre-Session-10 behaviour). The LLM is still called.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: None,
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(provider.call_count(), 1);
        let prompt = provider.last_prompt();
        // No endpoint hint → placeholder URL appears in the prompt.
        assert!(
            prompt.contains("example.invalid"),
            "prompt should fall back to placeholder URL when endpoint_hint is absent; got:\n{prompt}"
        );
        // The stub-excerpt path was taken: the prompt explicitly
        // notes the lack of a documented endpoint.
        assert!(
            prompt.contains("no documented endpoint registered")
                || prompt.contains("author from the description"),
            "prompt should carry the stub-excerpt marker; got:\n{prompt}"
        );

        assert_eq!(report.recipes_attempted, 1);
    }

    #[tokio::test]
    async fn author_one_falls_back_when_descriptor_absent() {
        // No descriptor at all for the bound source_id. Same fallback
        // as missing endpoint_hint. Guards against a
        // misconfiguration: the plan references "demo_csv" but no
        // such descriptor was loaded into AppState.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        // sources slice is empty — no descriptor for "demo_csv".
        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(provider.call_count(), 1);
        let prompt = provider.last_prompt();
        assert!(prompt.contains("example.invalid"));
        assert_eq!(report.recipes_attempted, 1);
    }

    #[tokio::test]
    async fn author_one_falls_back_when_prefetch_fails() {
        // Pre-fetch failure (URL not in fixture map → NoFixture
        // error) must not abort authoring. The executor should log a
        // warning and use the stub excerpt — but should still pass
        // the real endpoint_hint URL as the sample URL, so the LLM
        // has a real target.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Endpoint hint URL is *not* in the fixture map → pre-fetch
        // returns NoFixture. The recipe-execution URL *is* fixtured
        // so the rest of the run completes.
        let hint_url = "https://api.example.com/missing-fixture.csv";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some(hint_url.into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(provider.call_count(), 1);
        let prompt = provider.last_prompt();
        // The sample URL is the real endpoint_hint, even though the
        // pre-fetch failed.
        assert!(
            prompt.contains(hint_url),
            "prompt should still carry the real endpoint_hint URL on pre-fetch failure; got:\n{prompt}"
        );
        // The stub-excerpt path was taken: it surfaces the URL as
        // the documented endpoint.
        assert!(
            prompt.contains("Documented endpoint")
                || prompt.contains("pre-fetch failed"),
            "prompt should mark pre-fetch failure with the documented-endpoint hint; got:\n{prompt}"
        );

        assert_eq!(report.recipes_attempted, 1);
    }

    #[tokio::test]
    async fn author_one_falls_back_when_endpoint_hint_unparseable() {
        // A malformed `endpoint_hint` (non-URL string) must not crash
        // authoring. The executor logs a warning and falls back to
        // the placeholder path. Guards a misconfiguration in
        // sources.toml from breaking the run.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some("not a url at all".into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(provider.call_count(), 1);
        let prompt = provider.last_prompt();
        assert!(
            prompt.contains("example.invalid"),
            "prompt should fall back to placeholder URL when endpoint_hint is unparseable; got:\n{prompt}"
        );
        assert_eq!(report.recipes_attempted, 1);
    }

    #[tokio::test]
    async fn author_one_truncates_oversized_prefetch_excerpt() {
        // Pre-fetch a body bigger than `PREFETCH_EXCERPT_BUDGET`. The
        // excerpt that lands in the prompt must be truncated and
        // include the truncation marker, so the LLM doesn't think
        // the document just stops mid-row.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // A body larger than the 32 KiB budget. We use a
        // distinctive prefix so we can assert it appears in the
        // prompt, and a distinctive suffix that should NOT appear.
        let mut body = Vec::with_capacity(PREFETCH_EXCERPT_BUDGET * 2);
        body.extend_from_slice(b"PREFIX-MARKER\n");
        body.extend(std::iter::repeat_n(b'x', PREFETCH_EXCERPT_BUDGET * 2));
        body.extend_from_slice(b"SUFFIX-MARKER\n");

        let hint_url = "https://api.example.com/large.csv";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let small_csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new()
            .with(hint_url, body.as_slice())
            .with(canned_recipe_url, small_csv);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some(hint_url.into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let _ = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        let prompt = provider.last_prompt();
        assert!(
            prompt.contains("PREFIX-MARKER"),
            "prompt should include the start of the body"
        );
        assert!(
            !prompt.contains("SUFFIX-MARKER"),
            "prompt should not include content past the truncation budget"
        );
        assert!(
            prompt.contains("excerpt truncated"),
            "prompt should carry an explicit truncation marker"
        );
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
        use situation_room_secure::http::{SecureHttpClient, SecureHttpConfig};

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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            sources: &[],
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

    // Live JSON variant. Same structural-only discipline as the CSV
    // live test: prove the wiring works end-to-end against a real
    // network endpoint, without asserting on extracted values. The
    // default URL points at a small, stable public JSON document;
    // override with FETCH_LIVE_JSON_URL / FETCH_LIVE_JSON_PATH to
    // target something else.
    //
    // The recipe is pre-authored — UnreachableProvider enforces that
    // the executor must not call the LLM here (ADR 0011 §"LLM-free
    // runtime invariant").
    #[tokio::test]
    #[ignore]
    async fn live_fetch_against_real_json_produces_observation_and_closes_run() {
        use situation_room_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();

        // Default: a stable JSON file in the same datasets/country-list
        // repo the CSV live test uses. The path `$[0].Code` extracts
        // the first country code as a single scalar — matches the
        // shape of working_json_recipe (one extracted scalar per
        // recipe). Override the env vars if you want to target a
        // numeric dataset.
        let url = std::env::var("FETCH_LIVE_JSON_URL").unwrap_or_else(|_| {
            "https://raw.githubusercontent.com/datasets/country-list/main/data.json".to_string()
        });
        let path =
            std::env::var("FETCH_LIVE_JSON_PATH").unwrap_or_else(|_| "$[0].Code".to_string());

        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:json_demo:live", plan.id)),
            plan_id: plan.id,
            source_id: "json_demo".into(),
            source_url: Url::parse(&url).expect("FETCH_LIVE_JSON_URL must be a valid URL"),
            extraction: ExtractionSpec::JsonPath { path },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "value".into(),
                        // Same reasoning as the CSV live test: the
                        // default extracts a non-numeric scalar
                        // (country code), so we side-step the f64
                        // coercion by literal-binding `value`. The
                        // test is about wiring, not extraction
                        // values; override the env vars to exercise
                        // the numeric path.
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
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // Structural: recipe was attempted; either it succeeded or
        // surfaced a typed failure stage (Fetch / Apply / Insert).
        // A Skipped here would mean we accidentally went through a
        // non-JSON branch — that's a regression.
        assert_eq!(report.recipes_attempted, 1);
        assert!(
            !matches!(report.outcomes[0], RecipeOutcome::Skipped { .. }),
            "live test should not skip — got: {:?}",
            report.outcomes[0]
        );

        // Audit row exists and was closed.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 5).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].finished_at.is_some(), "fetch_run must be closed");
    }

    // -----------------------------------------------------------------
    // Session 21 — authored_from stamping (ADR 0014)
    // -----------------------------------------------------------------

    /// Happy path: when `prefetch_excerpt` returns real bytes, the
    /// recipe lands with `authored_from = FetchedBytes`. This is the
    /// optimistic case — most production recipes hit it.
    #[tokio::test]
    async fn author_one_stamps_fetched_bytes_when_prefetch_succeeds() {
        use situation_room_storage::AuthoredFrom;

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Both URLs in the fixture: pre-fetch sees real bytes and
        // recipe execution finds its CSV body.
        let hint_url = "https://api.example.com/csv-demo.csv";
        let hint_body = b"country,production\nChile,49000\n";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let recipe_body = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new()
            .with(hint_url, hint_body)
            .with(canned_recipe_url, recipe_body);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some(hint_url.into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let _report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // The recipe is persisted; load it back and assert the
        // stamped value. Using `recipes_for_plan` matches the load
        // path the UI uses (RecipesPanel reads recipes via the same
        // store method); the field must survive the same path.
        let recipes = store.recipes_for_plan(plan.id).unwrap();
        assert_eq!(recipes.len(), 1, "exactly one recipe was authored");
        assert_eq!(
            recipes[0].authored_from,
            AuthoredFrom::FetchedBytes,
            "happy-path authoring must stamp FetchedBytes"
        );
    }

    /// Stub-excerpt path: when pre-fetch fails (here: hint URL not
    /// in the fixture map → NoFixture error), the recipe lands with
    /// `authored_from = StubExcerpt`. This is the motivating case
    /// for ADR 0014 — exactly what happened to GDELT in the Session
    /// 20 live run.
    #[tokio::test]
    async fn author_one_stamps_stub_excerpt_when_prefetch_fails() {
        use situation_room_storage::AuthoredFrom;

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Hint URL is *not* in the fixture map → pre-fetch returns
        // None. Recipe-execution URL *is* fixtured so the fetch run
        // completes (the stub-authored recipe still runs against
        // the canned URL).
        let hint_url = "https://api.example.com/missing-fixture.csv";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        let sources = vec![SourceDescriptor {
            id: "demo_csv".into(),
            display_name: "CSV Demo".into(),
            description: "Used by tests.".into(),
            authoritative_for: vec![],
            endpoint_hint: Some(hint_url.into()),
        }];

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &sources,
        };

        let _report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        let recipes = store.recipes_for_plan(plan.id).unwrap();
        assert_eq!(recipes.len(), 1);
        assert_eq!(
            recipes[0].authored_from,
            AuthoredFrom::StubExcerpt,
            "pre-fetch failure must stamp StubExcerpt"
        );
    }

    /// No descriptor → no endpoint_hint → stub excerpt. Same outcome
    /// as the prefetch-failed path; pinned separately because the
    /// code path is distinct (the `hint_for_prefetch` is None from
    /// the start, vs. Some-then-None from a failed fetch).
    #[tokio::test]
    async fn author_one_stamps_stub_excerpt_when_descriptor_absent() {
        use situation_room_storage::AuthoredFrom;

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        // sources slice empty: no descriptor for "demo_csv".
        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &[],
        };

        let _report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        let recipes = store.recipes_for_plan(plan.id).unwrap();
        assert_eq!(recipes.len(), 1);
        assert_eq!(
            recipes[0].authored_from,
            AuthoredFrom::StubExcerpt,
            "missing descriptor must stamp StubExcerpt"
        );
    }
}
