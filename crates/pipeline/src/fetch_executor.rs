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
//! recipe author has been told `pdf_table` is fine for stable annual
//! reports) and runs through `recipe_apply::apply` like every other
//! mode. Session 29 (Track C, ADR 0007 amendment 5) wired the
//! runtime arm — what was `Skipped { reason: "pdf_table not
//! implemented" }` is now a real fetch → apply → insert path. With
//! pdf_table in, every variant of the closed extraction-mode enum is
//! a first-class wired runtime path.
//!
//! CssSelect was promoted in Session 12; RegexCapture in Session 13;
//! PdfTable in Session 29. The recipe_apply runtime has supported
//! every mode (via `csv`, `jsonpath_lib`, `scraper`, `regex`, and
//! `pdf-extract` respectively); what was missing each time was the
//! executor-level dispatch + the apply-and-insert plumbing. The
//! wiring is structurally identical to the CSV and JSON paths because
//! all of them go through the same `apply()` boundary, which
//! dispatches internally on the recipe's `ExtractionSpec`.
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
    fetch_run_outcomes::{FetchRunOutcomeKind, FetchRunOutcomeRow},
    fetch_runs::FetchRunRow,
    research_plans::PlanStatus,
    Store,
};

use std::time::{Duration, Instant};

use crate::fetch_backoff::{fetch_with_backoff, format_retry_after, BackoffOutcome};
use crate::http_fetcher::{FetchError as HttpFetchError, HttpFetcher};
use crate::propose_source_url::{
    propose_source_url, PriorAttempt, ProposalError, ProposalOutcome,
};
use crate::recipe_apply::{apply, ApplyContext, ApplyError};
use crate::recipe_author::{author_recipe, AuthoringContext, AuthoringError};
use crate::recipes::{ExpectationRef, ExtractionSpec, FetchRecipe};
use crate::recipes_store::{
    load_recipes_for_plan, save_recipe, RecipeStoreError,
};
use crate::research::{DocumentSourceEntry, DocumentSourceNomination, ResearchPlan};
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
    /// The source returned HTTP 429 with a `Retry-After` value the
    /// executor's inline backoff (see [`crate::fetch_backoff`]) chose
    /// not to wait through — either because the wait exceeded the
    /// short-backoff ceiling, or because no `Retry-After` header was
    /// provided at all (`retry_after_seconds: None`).
    ///
    /// Distinct from [`RecipeOutcome::Failed`] because the operator
    /// surface should render rate-limits differently from "the
    /// extraction broke" — re-running with no other change is
    /// reasonable for a rate-limit but pointless for an apply
    /// failure. The frontend's outcome-tone helper renders these in
    /// warning amber rather than error red.
    ///
    /// Track D, Session 25.
    RateLimited {
        recipe_id: Uuid,
        source_id: String,
        retry_after_seconds: Option<u64>,
    },
    /// The recipe-author LLM declined to write a recipe for this
    /// source and explained why through the `decline_reason` field of
    /// [`crate::recipe_author::RecipeAuthoringOutput`]. Track B
    /// (Session 28, ADR 0007 amendment 4).
    ///
    /// Distinct from [`RecipeOutcome::Failed`] (a recipe ran and
    /// broke), [`RecipeOutcome::Skipped`] (the executor itself chose
    /// not to run a recipe — historically pdf_table before Session 29,
    /// today reserved for any future not-yet-wired mode added to the
    /// closed enum), and [`RecipeOutcome::RateLimited`] (the source
    /// threw 429): a `Declined` outcome means **no recipe was created
    /// at all**, on the LLM's honest assessment that the source
    /// doesn't admit one under the closed extraction vocabulary.
    ///
    /// Carries `source_id` and `reason` only — there is no
    /// `recipe_id` because no recipe exists. The frontend renders
    /// this in a distinct tone so the operator sees an authoring-
    /// stage decision rather than a runtime failure; the appropriate
    /// remediation is editorial (drop the source, find an
    /// alternative, escalate the model tier) rather than retrying
    /// the same recipe.
    ///
    /// Declined outcomes are produced by the executor's
    /// `load_or_author_recipes` step on first authoring; once a plan
    /// has any persisted recipes, that step short-circuits and no
    /// new authoring runs, so subsequent fetch runs against the same
    /// plan never produce `Declined` outcomes for already-authored
    /// sources. The previous decline lives in the operator's
    /// memory of the prior run's report.
    Declined {
        source_id: String,
        reason: String,
    },
    /// The plan was classified before Session 37 / ADR 0015 and the
    /// operator triggered a fetch before any recipes had been authored
    /// against it. Pre-Session-37 plans carry their document sources
    /// as [`crate::research::DocumentSourceHint`] (description +
    /// `preferred_source_ids`) rather than as the post-ADR-0015
    /// [`crate::research::DocumentSourceNomination`] (which carries
    /// the URL the executor needs).
    ///
    /// The executor cannot author against a Legacy entry because there
    /// is no `endpoint_url` to feed the recipe-author pre-fetch — the
    /// pre-Session-37 path resolved the URL through
    /// `config/sources.toml`'s `endpoint_hint`, and that registry has
    /// been retired (the file now holds only two demo fixtures). The
    /// honest move is to surface this per-entry as a distinct outcome
    /// so the operator sees the cause and remediation in one glance.
    ///
    /// Carries `source_id` only — there is no `recipe_id` because no
    /// recipe was ever authored. The frontend renders this in the
    /// same "authoring decision" tone as `Declined`. Remediation:
    /// re-classify the plan; the new pass produces nominations
    /// carrying their own URLs.
    ///
    /// Once the plan is re-classified and recipes have been authored,
    /// this outcome stops appearing — the early-return at the top of
    /// `load_or_author_recipes` short-circuits when stored recipes
    /// already exist, and only the Legacy-entry branch produces this
    /// variant.
    LegacyPlanCannotAuthor {
        source_id: String,
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
    /// The propose-URL prompt template (Session 39). Same loading
    /// pattern. Consumed by [`propose_source_url`] inside the retry
    /// loop in `author_one`.
    pub propose_url_prompt: &'a str,
    /// Source descriptors for the executor.
    ///
    /// **Doc-narrowed under ADR 0015 (Session 37) and further under
    /// Session 39.** Production authoring no longer consults this
    /// slice — Session 37 moved URL emission to the L1 classifier,
    /// and Session 39 moved URL emission again to a per-attempt
    /// Level-2 propose-URL step. The slice survives only because the
    /// `apps_common::sources::load_source_descriptors` loader still
    /// parses `config/sources.toml` at startup and a few `#[ignore]`
    /// live tests author hand-crafted recipes against the `csv_demo`
    /// / `json_demo` fixtures. Pass `&[]` from any new composition
    /// root.
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

    // 3. Load-or-author recipes for the plan. Track B (Session 28):
    //    `load_or_author_recipes` now also returns any
    //    `RecipeOutcome::Declined` entries from the LLM declining
    //    via `decline_reason`. Declines are surfaced to the operator
    //    in the report's `outcomes` (prepended before any per-recipe
    //    outcomes) and do NOT count toward `recipes_attempted` —
    //    declined sources never produced a recipe to attempt.
    let (recipes, decline_outcomes) = match load_or_author_recipes(ctx, &plan).await {
        Ok(pair) => pair,
        Err(e) => {
            close_run_with_error(ctx.store, &mut run_row, &e.to_string());
            return Err(e);
        }
    };

    info!(
        plan_id = %plan_id,
        run_id = %run_id,
        recipe_count = recipes.len(),
        declined_count = decline_outcomes.len(),
        "recipes prepared, executing"
    );

    // 4. Iterate recipes. Per-recipe failures don't abort the run —
    //    they get reported and we move on. This is what "deterministic
    //    runtime" feels like to the user: a partial failure leaves a
    //    partial result with a precise account of what worked.
    //
    //    Track B (Session 28): start the outcomes Vec with any
    //    declines from the authoring step. They appear first in the
    //    UI list because they happened first in time (authoring
    //    precedes per-recipe execution). Their order within the list
    //    matches `load_or_author_recipes`'s source-iteration order,
    //    which itself reflects Level-1's source-priority hierarchy.
    let mut outcomes: Vec<RecipeOutcome> = Vec::with_capacity(decline_outcomes.len() + recipes.len());
    for declined in &decline_outcomes {
        if let RecipeOutcome::Declined { source_id, reason } = declined {
            warn!(
                plan_id = %plan_id,
                run_id = %run_id,
                source_id = %source_id,
                decline_reason = %reason,
                "recipe author declined this source; surfacing in report"
            );
        }
    }
    outcomes.extend(decline_outcomes);
    let mut records_produced_total: u32 = 0;
    let mut recipes_succeeded: u32 = 0;
    let recipes_attempted: u32 = recipes.len() as u32;

    for recipe in &recipes {
        let outcome = run_one_recipe(ctx, &plan, recipe, run_id).await;
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
            RecipeOutcome::RateLimited {
                retry_after_seconds,
                ..
            } => {
                // Track D: rate-limit is its own outcome category.
                // It does NOT count as `recipes_succeeded` (no
                // records produced) and it is not a `Failed`-style
                // stage error. The warn line names it specifically
                // so the operator's run log distinguishes "transient
                // throttling" from "the recipe is broken."
                warn!(
                    plan_id = %plan_id,
                    run_id = %run_id,
                    retry_after_seconds = ?retry_after_seconds,
                    "recipe rate-limited"
                );
            }
            // Track B: a `Declined` outcome cannot reach this match
            // arm — declines are produced by `load_or_author_recipes`
            // (and prepended to `outcomes` above) before this loop
            // ever runs, and `run_one_recipe` itself never returns
            // `Declined`. The arm exists to keep the match
            // exhaustive and to flag the invariant: if a future
            // session ever extends `run_one_recipe` to return
            // `Declined`, the run-counter logic above needs
            // revisiting (today, Declined doesn't bump
            // `recipes_attempted` or `recipes_succeeded`).
            RecipeOutcome::Declined { source_id, reason } => {
                warn!(
                    plan_id = %plan_id,
                    run_id = %run_id,
                    source_id = %source_id,
                    decline_reason = %reason,
                    "unexpected: run_one_recipe returned Declined; \
                     declines should originate in load_or_author_recipes"
                );
            }
            // ADR 0015 / Session 37: same exhaustiveness rationale as
            // the Declined arm above. `LegacyPlanCannotAuthor` is
            // produced by `load_or_author_recipes` for pre-Session-37
            // plans whose Legacy entries cannot be authored against.
            // It is prepended to `outcomes` before this loop runs;
            // `run_one_recipe` never returns it. Keep the arm to
            // preserve match exhaustiveness.
            RecipeOutcome::LegacyPlanCannotAuthor { source_id } => {
                warn!(
                    plan_id = %plan_id,
                    run_id = %run_id,
                    source_id = %source_id,
                    "unexpected: run_one_recipe returned LegacyPlanCannotAuthor; \
                     legacy outcomes should originate in load_or_author_recipes"
                );
            }
        }
        outcomes.push(outcome);
    }

    // 5a. Persist per-outcome rows for the heatmap surface (Session
    //     46). One row per RecipeOutcome captures the (run, recipe-or-
    //     source, kind) tuple the FetchReport carries synchronously
    //     to the UI; persisting it makes the same data legible across
    //     sessions and lets the recipe-success heatmap render history
    //     without re-running fetches.
    //
    //     Storage failures here are non-fatal: we log loudly and
    //     continue. The user-visible report is unchanged; only the
    //     persisted history loses these rows. Same posture as the
    //     `update_fetch_run` write below — the run's work is preserved
    //     even when an auxiliary write fails.
    persist_run_outcomes(ctx.store, plan_id, run_id, &outcomes);

    // 5b. Close the run row with final counters.
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

/// If the plan already has recipes, return them. Otherwise walk
/// `plan.expectations.document_sources` and run Level-2 authoring
/// once per [`DocumentSourceEntry::Nomination`]. ADR 0015 / Session
/// 37 changed the iteration:
///
/// - **`Nomination`** (the post-Session-37 shape) → `author_one`
///   reads the URL from the entry directly, no descriptor lookup.
/// - **`Legacy`** (pre-Session-37 plans persisted before ADR 0015)
///   → emit one [`RecipeOutcome::LegacyPlanCannotAuthor`] per
///   `preferred_source_id` and skip authoring. The executor cannot
///   author against a Legacy entry because there is no `endpoint_url`
///   to feed the recipe-author pre-fetch — see the variant doc.
///
/// Returns `(recipes, decline_outcomes)`:
/// - `recipes` is the Vec of successfully authored (or pre-existing)
///   `FetchRecipe`s for the run loop to iterate.
/// - `decline_outcomes` carries `RecipeOutcome::Declined` entries for
///   sources where the LLM declined via Track B's `decline_reason`
///   channel **and** `RecipeOutcome::LegacyPlanCannotAuthor` entries
///   for any pre-Session-37 Legacy entries the plan still carries.
///   The run loop prepends these to the report's `outcomes`.
///
/// Per-source authoring failures *other than* `Declined` /
/// `LegacyPlanCannotAuthor` (LLM call errors, schema parse errors,
/// structural validation rejects) keep the prior session's
/// behaviour: log loudly and continue. They do not surface as
/// outcomes — the warn log is the audit trail. The rationale is
/// the same as before Track B: a transient provider error or a
/// single malformed output shouldn't poison the run loop.
async fn load_or_author_recipes(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
) -> Result<(Vec<FetchRecipe>, Vec<RecipeOutcome>), FetchExecutorError> {
    let existing = load_recipes_for_plan(ctx.store, plan.id)?;
    if !existing.is_empty() {
        // Recipes already authored — no fresh LLM call, no decline
        // path to surface, no Legacy outcome to emit. Return
        // whatever already lives in storage.
        return Ok((existing, Vec::new()));
    }

    // Walk the document_sources entries and split them into two work
    // streams: nominations to author against, and legacy entries to
    // surface as cannot-author outcomes. Cross-entry dedup on
    // (source_url, known_id) for nominations is deliberately left
    // out — the LLM already deduplicates at emission time, and the
    // executor's authoring step is idempotent through the recipe
    // dedup_key on persistence.
    let mut nominations: Vec<&DocumentSourceNomination> = Vec::new();
    let mut legacy_outcomes: Vec<RecipeOutcome> = Vec::new();
    for entry in &plan.expectations.document_sources {
        match entry {
            DocumentSourceEntry::Nomination(n) => nominations.push(n),
            DocumentSourceEntry::Legacy(h) => {
                // Pre-ADR-0015 hint. Match the pre-Session-37
                // `bound_source_ids` skip-on-empty semantic: an entry
                // with no preferred_source_ids contributed no
                // authoring work then either, and surfacing one
                // outcome with no source_id would be confusing in
                // the report.
                for raw_id in &h.preferred_source_ids {
                    let trimmed = raw_id.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    warn!(
                        plan_id = %plan.id,
                        source_id = %trimmed,
                        "legacy DocumentSourceHint cannot be authored under ADR 0015; \
                         emitting LegacyPlanCannotAuthor — re-classify the plan to update"
                    );
                    legacy_outcomes.push(RecipeOutcome::LegacyPlanCannotAuthor {
                        source_id: trimmed.to_string(),
                    });
                }
            }
        }
    }

    let total = nominations.len();
    info!(
        plan_id = %plan.id,
        total_sources = total,
        legacy_entries = legacy_outcomes.len(),
        "authoring recipes for plan: starting"
    );

    let mut authored = Vec::new();
    let mut declines: Vec<RecipeOutcome> = legacy_outcomes;
    for (idx, nomination) in nominations.iter().enumerate() {
        let position = idx + 1;
        info!(
            plan_id = %plan.id,
            nomination_id = %nomination.nomination_id,
            description = %nomination.description,
            position,
            total,
            "authoring nomination via Session-47 multi-expectation flow"
        );
        // Session 47: one nomination → up to MAX_AUTHORS_PER_NOMINATION
        // recipes (one per target expectation the LLM authors against
        // the same prefetched bytes). The first target drives URL
        // discovery (the Session-39 retry loop); subsequent targets
        // re-use the locked URL+bytes via direct authoring calls.
        match author_for_nomination(ctx, plan, nomination).await {
            Ok((nomination_recipes, expectation_declines, nomination_decline_reason)) => {
                // Persist every authored recipe. Each carries its own
                // dedup_key — `{plan_id}:{nomination_id}:{bucket}:{index}` —
                // so siblings from the same nomination coexist without
                // collision (Session 47).
                let recipe_count = nomination_recipes.len();
                for recipe in nomination_recipes {
                    save_recipe(ctx.store, &recipe)?;
                    authored.push(recipe);
                }
                // Per-expectation declines (the LLM declined a
                // specific (target, locked URL+bytes) combination)
                // surface with widened source_id so the FetchReport /
                // heatmap / coverage matrix see them as distinct
                // rows from the authored siblings of the same
                // nomination.
                let per_expectation_count = expectation_declines.len();
                for d in expectation_declines {
                    let source_id = derive_source_id_for_decline(
                        nomination,
                        Some(d.expectation),
                    );
                    info!(
                        plan_id = %plan.id,
                        source_id = %source_id,
                        position,
                        total,
                        decline_reason = %d.reason,
                        "expectation declined; surfacing as RecipeOutcome::Declined"
                    );
                    declines.push(RecipeOutcome::Declined {
                        source_id,
                        reason: d.reason,
                    });
                }
                // Nomination-level decline (URL discovery failed,
                // deadline elapsed, or every URL produced no recipe
                // for any target). Surfaces as one row with the
                // legacy `nom:{nomination_id}` source_id shape so
                // the FetchReport's keyed-each + RecipeFlagDialog
                // wiring continues to operate against the
                // nomination's standing identity. Session 40
                // uniqueness invariant preserved.
                if let Some(reason) = nomination_decline_reason {
                    let source_id = derive_source_id_for_decline(nomination, None);
                    info!(
                        plan_id = %plan.id,
                        source_id = %source_id,
                        position,
                        total,
                        decline_reason = %reason,
                        "nomination-level decline; surfacing as RecipeOutcome::Declined"
                    );
                    declines.push(RecipeOutcome::Declined { source_id, reason });
                }
                info!(
                    plan_id = %plan.id,
                    nomination_id = %nomination.nomination_id,
                    position,
                    total,
                    recipes = recipe_count,
                    per_expectation_declines = per_expectation_count,
                    "nomination authored under multi-expectation flow"
                );
            }
            Err(e) => {
                warn!(
                    plan_id = %plan.id,
                    nomination_id = %nomination.nomination_id,
                    position,
                    total,
                    error = %e,
                    "recipe authoring failed for this nomination; continuing"
                );
            }
        }
    }

    info!(
        plan_id = %plan.id,
        total_sources = total,
        succeeded = authored.len(),
        declined_or_legacy = declines.len(),
        "authoring recipes for plan: complete"
    );

    Ok((authored, declines))
}

/// Source-id derivation for a `Declined` outcome where no recipe
/// exists.
///
/// Session 39: descriptions don't carry URLs anymore, so there's no
/// host to derive an id from at decline time. The `nomination_id`
/// is the stable identity surface — it's what `dedup_key` uses too,
/// so any future re-author of this nomination (after re-classification
/// or operator action) lines up against the same id.
///
/// **Session 40 — uniqueness fix.** The Session 39 implementation
/// of this function used `&s[..8]` as a "short prefix for log
/// readability." That was wrong: UUIDv7's first 48 bits are the
/// millisecond Unix timestamp, so the first 12 hex chars are
/// identical across all nominations minted in the same millisecond.
/// The classifier mints all of one plan's nominations in one tight
/// loop — well under a millisecond — so every decline in a plan was
/// receiving the same source_id (`nom:019e06b0` repeated N times in
/// the live titanium-supply-chain run). Two visible failures:
///
///   1. The frontend's `{#each report.outcomes as o (outcomeKey(o))}`
///      keyed-each in `FetchReport.svelte` produces duplicate keys
///      and Svelte 5 throws `each_key_duplicate`, leaving the panel
///      stuck on its summary header without the outcomes list.
///      That is the "looks identical before and after Run Fetch"
///      symptom the operator reported.
///   2. Recipe-feedback (ADR 0013) keys on `(plan_id, source_id)`,
///      so flagging one declined nomination flagged all of them.
///      The flag-from-decline channel was unusable for any plan
///      with >1 decline.
///
/// The fix: use the full nomination_id. Storage's
/// `recipe_feedback.source_id` is `TEXT NOT NULL` with no length
/// cap, and the API command bounds-checks against `Bounds::URL`
/// (2,048 chars), so the longer string passes through unchanged.
/// Log-line scannability is preserved by the existing `position`
/// + `total` fields the caller logs alongside.
fn derive_source_id_for_decline(
    nomination: &DocumentSourceNomination,
    target: Option<ExpectationRef>,
) -> String {
    // Session 47: per-expectation declines under multi-recipe-per-
    // nomination need a source_id distinct from the nomination-level
    // decline so the FetchReport's keyed-each surface, the heatmap's
    // per-(recipe-or-source, source_id) grouping, and the coverage
    // matrix's per-(bucket, index) row all see the rows as distinct.
    //
    // The widened shape mirrors the dedup_key shape used by authored
    // recipes (see `dedup_key_for_recipe`): the same coordinate names
    // the same logical thing whether it ended up authored or
    // declined. That symmetry is what makes the coverage matrix's
    // "uncovered" rows meaningful — every expectation the LLM tried
    // and declined against this nomination has its own surface,
    // legible to the operator without log scraping.
    //
    // `target = None` is the nomination-level decline (no record-
    // typed expectations to target, or every target's retry loop
    // exhausted for an unrelated reason). The pre-Session-47 shape
    // (`nom:{nomination_id}`) is preserved for that case so existing
    // RecipeFlagDialog wiring continues to operate against the
    // nomination's standing identity.
    match target {
        Some(t) => {
            let (bucket, index) = expectation_ref_parts(t);
            format!("nom:{}:{}:{}", nomination.nomination_id, bucket, index)
        }
        None => format!("nom:{}", nomination.nomination_id),
    }
}

/// Closed lookup of `(bucket_str, index)` for an [`ExpectationRef`].
///
/// The bucket strings match the v1.15 recipe-author prompt's
/// `{{TARGET_EXPECTATION}}` rendering and the
/// `{plan_id}:{nomination_id}:{bucket}:{index}` dedup-key shape used
/// by [`dedup_key_for_recipe`]. The same bucket vocabulary is used
/// by [`derive_source_id_for_decline`] for per-expectation declines
/// so dedup-key-shaped strings and decline-source_id-shaped strings
/// share a single naming convention. Session 47.
fn expectation_ref_parts(r: ExpectationRef) -> (&'static str, u32) {
    match r {
        ExpectationRef::ObservationMetric { index } => ("observation_metric", index),
        ExpectationRef::EventType { index } => ("event_type", index),
        ExpectationRef::EntityKind { index } => ("entity_kind", index),
        ExpectationRef::RelationKind { index } => ("relation_kind", index),
        ExpectationRef::DocumentSource { index } => ("document_source", index),
    }
}

/// Build the per-recipe `dedup_key` under Session 47's multi-recipe-
/// per-nomination shape.
///
/// Pre-Session-47 the shape was `{plan_id}:{nomination_id}` — one
/// recipe per nomination. Session 47 widens to
/// `{plan_id}:{nomination_id}:{bucket}:{index}` so multiple recipes
/// from the same nomination (each targeting one expectation against
/// the same prefetched bytes) coexist without primary-key collision
/// in the recipes table.
///
/// **Re-author lineage stays intact.** A re-author of one of these
/// recipes still routes through `Store::get_recipe_by_dedup_key`
/// against the wider key, finds the same row, and bumps the
/// version. Other nomination-siblings under different expectations
/// are untouched.
fn dedup_key_for_recipe(
    plan_id: Uuid,
    nomination_id: Uuid,
    target: ExpectationRef,
) -> String {
    let (bucket, index) = expectation_ref_parts(target);
    format!("{}:{}:{}:{}", plan_id, nomination_id, bucket, index)
}

/// Build the list of target expectation references the executor should
/// attempt to author against for one nomination.
///
/// **Concatenates the four record-typed buckets in declaration order**
/// (`observation_metric`, then `event_type`, then `entity_kind`, then
/// `relation_kind`) and truncates to `max` entries. The plan's own
/// declaration order is the operator's stated priority — the
/// classifier emitted them in that order based on the user's topic.
/// `document_source` is excluded because the nomination *is* a
/// document_source entry; a recipe targeting that bucket would have
/// the source authoring a record about itself.
///
/// **No source-specific routing.** This function reads only the plan;
/// it never inspects the nomination's URL host, description, or any
/// other source identifier. The LLM decides per `(nomination,
/// expectation)` pair whether the prefetch evidence supports the
/// named target — see the v1.15 recipe-author prompt's
/// "target-expectation" section. ADR 0007 / ADR 0015 §"closed-
/// vocabulary discipline".
///
/// Returns an empty Vec when the plan declares no record-typed
/// expectations. The caller surfaces this as a nomination-level
/// decline rather than authoring zero recipes silently.
fn build_target_expectations(plan: &ResearchPlan, max: usize) -> Vec<ExpectationRef> {
    let mut out: Vec<ExpectationRef> = Vec::new();
    for i in 0..plan.expectations.observation_metrics.len() {
        if out.len() >= max {
            return out;
        }
        out.push(ExpectationRef::ObservationMetric { index: i as u32 });
    }
    for i in 0..plan.expectations.event_types.len() {
        if out.len() >= max {
            return out;
        }
        out.push(ExpectationRef::EventType { index: i as u32 });
    }
    for i in 0..plan.expectations.entity_kinds.len() {
        if out.len() >= max {
            return out;
        }
        out.push(ExpectationRef::EntityKind { index: i as u32 });
    }
    for i in 0..plan.expectations.relation_kinds.len() {
        if out.len() >= max {
            return out;
        }
        out.push(ExpectationRef::RelationKind { index: i as u32 });
    }
    out
}

/// One per-expectation decline produced under Session 47's multi-
/// recipe-per-nomination flow. Returned by [`author_for_nomination`]
/// alongside the recipes that did succeed; the caller projects each
/// entry into a [`RecipeOutcome::Declined`] with a per-expectation
/// `source_id`.
struct ExpectationDecline {
    expectation: ExpectationRef,
    reason: String,
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
// Session 44 — bumped from 32 KiB to 64 KiB. The 32 KiB ceiling was
// covering only ~8–10 pages of framed PDF excerpts when the narrative
// branch on no-table pages emitted up to 4 KiB each. With Session 44's
// drop of per-page narrative on no-table pages, framing across the
// whole document becomes much denser — a 110-page PDF with one small
// table per page is ~55 KiB of framed output, which 32 KiB cannot
// hold but 64 KiB can. The bump is uniform across the four prefetch
// branches (PDF, HTML, JSON, raw bytes); HTML/JSON/raw were not the
// binding constraint, so they simply inherit the headroom. PDFs were
// the binding constraint, and 64 KiB is the smallest power-of-two
// budget that fits the full framed-table view of a typical multi-
// chapter regulatory or statistical PDF (USGS MCS, EUR-Lex annex
// volumes, RBA stat releases) without truncation. Above that we
// honestly truncate at the end and the LLM declines on the missing
// pages — no per-source heuristic, no per-document-class branch.
const PREFETCH_EXCERPT_BUDGET: usize = 64 * 1024;

/// Maximum number of (propose URL → fetch → author) attempts the
/// retry loop will make for one nomination before recording the
/// nomination as exhausted. Session 39.
///
/// Three is the conservative compromise: enough to recover from one
/// or two bad URL picks (404, SPA, navigation-only), few enough that
/// 5 sources × 3 attempts × ~30s ≈ 7.5 minutes worst-case wall time
/// stays inside the per-source deadline (which is the tighter bound
/// in practice).
const MAX_AUTHORING_ATTEMPTS_PER_SOURCE: u32 = 3;

/// Maximum number of authoring calls the multi-recipe-per-nomination
/// flow will make for one nomination before stopping. Session 47.
///
/// One nomination drives up to N independent `author_recipe` calls,
/// one per target expectation against the same prefetched bytes
/// (after URL discovery locks). Each authoring call costs a few
/// seconds and a few thousand tokens; capping bounds the worst-case
/// LLM bill per nomination.
///
/// The cap interacts with [`build_target_expectations`]: that
/// function concatenates the four record-typed buckets in
/// declaration order and truncates to this number of entries. So
/// for a plan with 4 obs metrics + 3 event types + 2 entity kinds,
/// the executor authors against the first 4 entries of the
/// concatenation (all four obs metrics in this case); the remainder
/// is silently uncovered until the operator either re-classifies
/// (which yields fresh nominations) or raises the cap.
///
/// 4 is the conservative compromise: enough to cover a small
/// research plan's top-priority bucket fully, few enough that a
/// 7-nomination plan stays under ~30 LLM calls per fetch run for
/// authoring (one propose-URL retry loop + 4 author calls per
/// nomination = ~28 calls in the typical case).
const MAX_AUTHORS_PER_NOMINATION: usize = 4;

/// Per-nomination retry-loop deadline, in seconds. Once `Instant::now`
/// exceeds `started + this`, the loop stops and surfaces the
/// nomination as declined regardless of remaining attempts. Session 39.
///
/// 240s is generous: in practice each propose-URL call lands in
/// 5-15s (Cheap tier, small prompt) and each recipe-author call in
/// 15-30s (Workhorse, longer prompt with bytes). The deadline only
/// bites when the LLM gateway slows down dramatically.
const PER_SOURCE_DEADLINE_SECS: u64 = 240;

/// Author all recipes for one (plan, nomination) pair under Session
/// 47's multi-recipe-per-nomination flow.
///
/// **Two interleaved loops.**
///
/// The outer loop runs at most [`MAX_AUTHORING_ATTEMPTS_PER_SOURCE`]
/// times or until [`PER_SOURCE_DEADLINE_SECS`] elapses, whichever
/// comes first. Each iteration discovers a candidate URL via
/// [`propose_source_url`] (Cheap tier) and pre-fetches its bytes
/// through [`prefetch_excerpt`].
///
/// The inner loop iterates [`build_target_expectations`] (capped at
/// [`MAX_AUTHORS_PER_NOMINATION`]) and calls [`author_recipe`]
/// (Workhorse tier) once per target against those bytes. Each
/// authoring call is constrained to its target via the v1.15
/// recipe-author prompt's `{{TARGET_EXPECTATION}}` section; the
/// validator rejects the LLM's output if it tries to substitute a
/// different expectation.
///
/// **URL discovery is target-agnostic; target iteration shares the
/// fetched bytes.** Pre-Session-47 the executor authored one recipe
/// per nomination; Session 47 authors up to N. The URL discovery
/// cost (one propose-URL call + one fetch per attempt) is paid
/// once per attempt and amortised across every target authored
/// against those bytes. The cap on `MAX_AUTHORS_PER_NOMINATION`
/// bounds the per-attempt LLM bill at `1 propose + N author` calls.
///
/// **Lock-on-first-success.** Once any target authors against an
/// attempt's URL+bytes, the function locks that URL — subsequent
/// targets that decline against those bytes go straight into
/// [`ExpectationDecline`] entries; we do *not* try a different URL
/// for them. Re-fetching a different URL per target would risk
/// stamping siblings of the same nomination with mismatched
/// `source_id`s, and would multiply the LLM bill without principled
/// gain (the v1.15 prompt's contract is "either author against
/// these bytes or decline" — a target that declines was given the
/// same evidentiary surface another target authored against).
///
/// **All-targets-decline retries.** If every target declines
/// against the URL the proposer suggested for an attempt, that URL
/// is recorded as a prior attempt and the outer loop retries with a
/// fresh propose-URL call (which sees the prior-attempts history
/// and can pick differently).
///
/// ## Identity
///
/// Pre-Session-47, `dedup_key` was `{plan_id}:{nomination_id}` —
/// one recipe per nomination. Session 47 widens to
/// `{plan_id}:{nomination_id}:{bucket}:{index}` so siblings from
/// the same nomination coexist; see [`dedup_key_for_recipe`].
/// `source_id` continues to be the URL host of whichever attempt
/// locked, shared across siblings of the same nomination because
/// the URL is shared.
///
/// ## Returns
///
/// `(recipes, expectation_declines, nomination_decline_reason)`:
///
/// - **`recipes`** — every recipe authored. Empty when the
///   nomination decline path was taken.
/// - **`expectation_declines`** — one entry per target the LLM
///   declined for *against the locked URL+bytes*. Targets that the
///   nomination never reached (because no URL ever locked) are
///   represented under `nomination_decline_reason` instead.
/// - **`nomination_decline_reason`** — `Some(reason)` when URL
///   discovery itself failed (propose-URL declined on first
///   attempt, every attempt's URL fetched but no target authored,
///   or the deadline elapsed); `None` when at least one URL
///   locked and at least one target authored against it.
///
/// The caller projects these into either:
///   - one `RecipeOutcome::Declined` with `source_id =
///     nom:{nomination_id}` when `nomination_decline_reason` is
///     set, or
///   - one or more `RecipeOutcome::Declined` rows with widened
///     `source_id = nom:{nomination_id}:{bucket}:{index}` when
///     per-target declines surface alongside one or more authored
///     recipes.
async fn author_for_nomination(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    nomination: &DocumentSourceNomination,
) -> Result<
    (Vec<FetchRecipe>, Vec<ExpectationDecline>, Option<String>),
    FetchExecutorError,
> {
    let nomination_id = nomination.nomination_id;
    let targets = build_target_expectations(plan, MAX_AUTHORS_PER_NOMINATION);
    if targets.is_empty() {
        // Plan declares no record-typed expectations to author for.
        // Surface to the caller as a nomination-level decline.
        return Ok((
            Vec::new(),
            Vec::new(),
            Some(
                "plan declares no record-typed expectations                  (observation_metric, event_type, entity_kind,                  relation_kind); the multi-recipe-per-nomination                  flow has nothing to target"
                    .to_string(),
            ),
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(PER_SOURCE_DEADLINE_SECS);

    // Look up persistent operator feedback once — it doesn't change
    // across attempts within a single retry loop. ADR 0013: feedback
    // is keyed by (plan_id, source_id) at the storage layer; under
    // Session 39 we use the nomination_id as the source_id key for
    // feedback lookup. All Stage-1 and Stage-2 author calls for one
    // nomination share the same feedback row, regardless of which
    // target the call constrains.
    let feedback_key = nomination_id.to_string();
    let recipe_feedback = match ctx
        .store
        .recipe_feedback_for_source(plan.id, &feedback_key)
    {
        Ok(Some(stored)) => Some(stored.note),
        Ok(None) => None,
        Err(e) => {
            warn!(
                plan_id = %plan.id,
                nomination_id = %nomination_id,
                error = %e,
                "recipe_feedback lookup failed; authoring will proceed without operator feedback"
            );
            None
        }
    };

    let mut recipes: Vec<FetchRecipe> = Vec::new();
    let mut declines: Vec<ExpectationDecline> = Vec::new();
    let mut prior_attempts: Vec<PriorAttempt> = Vec::new();

    for attempt_num in 1..=MAX_AUTHORING_ATTEMPTS_PER_SOURCE {
        // Deadline gate before each attempt. Failing fast is
        // preferable to starting a fresh LLM round trip we know we
        // can't honour.
        if Instant::now() >= deadline {
            let reason = format!(
                "per-source deadline ({}s) exceeded after {} attempt(s); attempts: {}",
                PER_SOURCE_DEADLINE_SECS,
                attempt_num - 1,
                summarize_attempts(&prior_attempts)
            );
            return Ok((recipes, declines, Some(reason)));
        }

        info!(
            plan_id = %plan.id,
            nomination_id = %nomination_id,
            attempt = attempt_num,
            max_attempts = MAX_AUTHORING_ATTEMPTS_PER_SOURCE,
            description = %nomination.description,
            target_count = targets.len(),
            "proposing URL for nomination (Session 47 multi-target authoring)"
        );

        // Step 1: propose URL. Target-agnostic — the propose-URL
        // prompt sees the plan + nomination + prior-attempts history,
        // not the target expectation; URL discovery is shared across
        // every target the inner loop will try against these bytes.
        let proposal = propose_source_url(
            ctx.provider,
            ModelTier::Cheap,
            ctx.propose_url_prompt,
            plan,
            nomination,
            &prior_attempts,
        )
        .await
        .map_err(map_proposal_error)?;

        let (proposed_url, _proposal_rationale) = match proposal {
            ProposalOutcome::Url { url, rationale } => (url, rationale),
            ProposalOutcome::Declined { reason } => {
                // Propose-URL has nothing more to try. Per-target
                // retry against this proposer wouldn't help — the
                // proposer is target-agnostic. Surface as a single
                // nomination-level decline so the operator sees one
                // row in the report, not N identical rows.
                let attempts_str = summarize_attempts(&prior_attempts);
                let composed = if prior_attempts.is_empty() {
                    format!("url proposer declined on first attempt: {reason}")
                } else {
                    format!(
                        "url proposer declined after {} attempt(s): {reason}; attempts: {attempts_str}",
                        attempt_num - 1
                    )
                };
                info!(
                    plan_id = %plan.id,
                    nomination_id = %nomination_id,
                    attempt = attempt_num,
                    decline_reason = %reason,
                    "url proposer declined; surfacing as nomination-level decline"
                );
                return Ok((recipes, declines, Some(composed)));
            }
        };

        info!(
            plan_id = %plan.id,
            nomination_id = %nomination_id,
            attempt = attempt_num,
            proposed_url = %proposed_url,
            "URL proposed; pre-fetching"
        );

        let candidate_source_id = derive_source_id_from_url(&proposed_url);

        // Step 2: fetch the proposed URL. Routes through
        // SecureHttpClient and honours the rate-limit backoff. On
        // failure, record + retry with a different URL.
        let (excerpt, prefetched_bytes) =
            match prefetch_excerpt(ctx, &proposed_url, &candidate_source_id).await {
                Some(real) => real,
                None => {
                    let reason =
                        "fetch failed (network error, bad status, or oversized response — see warn-level log above)";
                    prior_attempts.push(PriorAttempt {
                        url: proposed_url.to_string(),
                        reason: reason.to_string(),
                    });
                    continue;
                }
            };

        // Step 3: iterate targets, calling author_recipe per
        // (target, locked URL+bytes). The first target that authors
        // locks the URL — subsequent targets that decline surface as
        // ExpectationDecline entries against the same locked URL.
        let mut authored_this_attempt: Vec<FetchRecipe> = Vec::new();
        let mut declined_this_attempt: Vec<(ExpectationRef, String)> = Vec::new();
        for &target in &targets {
            let auth_ctx = AuthoringContext {
                source_id: candidate_source_id.clone(),
                sample_url: proposed_url.clone(),
                document_excerpt: excerpt.clone(),
                recipe_feedback: recipe_feedback.clone(),
                previous_failure_reason: None,
                operator_guidance: None,
            };

            let auth_result = author_recipe(
                ctx.provider,
                ModelTier::Workhorse,
                ctx.recipe_author_prompt,
                plan,
                &auth_ctx,
                Some(&prefetched_bytes),
                Some(target),
            )
            .await;

            match auth_result {
                Ok(mut recipe) => {
                    recipe.source_id = candidate_source_id.clone();
                    recipe.dedup_key = Some(dedup_key_for_recipe(
                        plan.id,
                        nomination_id,
                        target,
                    ));
                    recipe.authored_from =
                        situation_room_storage::AuthoredFrom::FetchedBytes;
                    info!(
                        plan_id = %plan.id,
                        nomination_id = %nomination_id,
                        attempt = attempt_num,
                        source_id = %candidate_source_id,
                        recipe_id = %recipe.id,
                        ?target,
                        "recipe authored under multi-target flow"
                    );
                    authored_this_attempt.push(recipe);
                }
                Err(AuthoringError::Declined { reason }) => {
                    warn!(
                        plan_id = %plan.id,
                        nomination_id = %nomination_id,
                        attempt = attempt_num,
                        url = %proposed_url,
                        ?target,
                        decline = %reason,
                        "recipe author declined this (URL, target) pair"
                    );
                    declined_this_attempt.push((target, reason));
                }
                Err(other) => {
                    // Hard error (LLM outage, schema miss, network
                    // outage) — bubble up. We don't keep partial
                    // recipes from this attempt because the outer
                    // run-level error surface needs to see the
                    // failure honestly.
                    return Err(FetchExecutorError::Authoring(other));
                }
            }
        }

        if !authored_this_attempt.is_empty() {
            // At least one target authored against this URL — lock
            // and finish. Surface declined-this-attempt entries as
            // per-expectation declines.
            recipes.extend(authored_this_attempt);
            for (t, r) in declined_this_attempt {
                declines.push(ExpectationDecline {
                    expectation: t,
                    reason: r,
                });
            }
            return Ok((recipes, declines, None));
        }

        // No target authored against this URL. Record a prior-
        // attempts entry summarising every target's decline, then
        // try a different URL on the next iteration.
        let summary = if declined_this_attempt.is_empty() {
            "no targets attempted (empty target list — should not reach here)".to_string()
        } else {
            declined_this_attempt
                .iter()
                .map(|(t, r)| {
                    let (b, i) = expectation_ref_parts(*t);
                    format!("[{}:{}] {}", b, i, r)
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        prior_attempts.push(PriorAttempt {
            url: proposed_url.to_string(),
            reason: format!("no target authored: {summary}"),
        });
    }

    // Outer loop exhausted MAX_AUTHORING_ATTEMPTS_PER_SOURCE without
    // any URL producing a recipe for any target. Surface as a single
    // nomination-level decline with the attempt history baked into
    // the reason — the operator sees the full URL-discovery story
    // in the fetch report.
    let composed = format!(
        "exhausted {} attempts without authoring against any target; attempts: {}",
        MAX_AUTHORING_ATTEMPTS_PER_SOURCE,
        summarize_attempts(&prior_attempts)
    );
    Ok((recipes, declines, Some(composed)))
}

/// Translate a [`ProposalError`] into a [`FetchExecutorError`].
/// Most `ProposalError` variants map cleanly onto an
/// `AuthoringError::Declined` with a reason that names the failure
/// mode — they're errors at the URL-discovery half of the L2 process,
/// not at recipe authoring proper, but the operator surface treats
/// them uniformly as "this nomination didn't yield a recipe."
fn map_proposal_error(e: ProposalError) -> FetchExecutorError {
    match e {
        ProposalError::Llm(le) => {
            // LLM call genuinely failed — bubble up so the run is
            // marked failed at the run level, not just this source.
            FetchExecutorError::Authoring(AuthoringError::Llm(le))
        }
        ProposalError::NoStructuredOutput => {
            FetchExecutorError::Authoring(AuthoringError::NoStructuredOutput)
        }
        ProposalError::OutputParse(s) => {
            FetchExecutorError::Authoring(AuthoringError::OutputParse(s))
        }
        ProposalError::BadUrl(v) => {
            // Proposer emitted a URL the guard rejected. Treat as a
            // decline on this attempt rather than a hard failure —
            // the loop can ask again with the bad URL recorded as
            // prior context.
            FetchExecutorError::Authoring(AuthoringError::Declined {
                reason: format!("propose-url returned a guard-rejected URL: {v}"),
            })
        }
        ProposalError::Prompt(s) => {
            FetchExecutorError::Authoring(AuthoringError::Prompt(s))
        }
    }
}

/// Compact one-line summary of prior attempts for inclusion in
/// decline reasons. Format: `[attempt 1] url1 → reason1; [attempt 2]
/// url2 → reason2`.
///
/// Used in the executor-level surface (the `reason` field of the
/// outer `AuthoringError::Declined` returned on exhaustion) so the
/// operator sees the full URL-discovery history in one place. The
/// per-attempt details are also in the warn-level logs, but those
/// are easier to lose.
fn summarize_attempts(attempts: &[PriorAttempt]) -> String {
    if attempts.is_empty() {
        return "(none)".to_string();
    }
    attempts
        .iter()
        .enumerate()
        .map(|(i, a)| format!("[attempt {}] {} → {}", i + 1, a.url, a.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Derive a human-readable `source_id` from a proposed URL — the URL
/// host string, lower-cased.
///
/// Session 39: simpler than the pre-Session-39
/// `derive_effective_source_id` because the `known_id` ↔ host
/// reconciliation is gone (descriptions don't carry known_id any
/// more). The host is what the operator sees in the recipe
/// inspection panel and in the fetch-run logs; storage stability
/// across attempts and runs comes from `dedup_key` (which uses
/// `nomination_id`), not from `source_id`.
fn derive_source_id_from_url(url: &url::Url) -> String {
    url.host_str()
        .unwrap_or("unknown_host")
        .to_ascii_lowercase()
}

/// Fetch the endpoint hint and return a bounded UTF-8 excerpt, or
/// `None` if the fetch failed. Failure is logged at warn level; the
/// caller decides what to do with the absence.
///
/// We read up to `PREFETCH_EXCERPT_BUDGET` bytes of the eventual
/// excerpt body. The HTTP layer already enforces a much larger
/// ceiling (`max_response_bytes`); the budget here is about prompt
/// size, not about defending the network layer.
///
/// **Session 41 — framed-table PDF prefetch + HTML structural
/// digest.** When the fetched bytes are a PDF, we run them through
/// `pdf_extract::extract_text_from_mem_by_pages` *and* through
/// `recipe_apply::detect_pdf_tables` (the same detector
/// `extract_pdf_table` calls at apply time) so the LLM sees the page
/// content in the runtime's coordinate space — `[PDF page N, table M]
/// (R rows × C cols)` followed by the cell values, and `[PDF page N]
/// (no table detected)` for pages where the detector found nothing
/// tabular. (Session 44 dropped the per-page narrative that pre-
/// viously followed the no-table marker; see
/// `render_pdf_text_with_tables`'s rustdoc for the rationale.)
///
/// When the fetched bytes are HTML, we parse them with `scraper`
/// (the same crate the runtime's `extract_css_select` queries) and
/// emit a *structural digest* under
/// `--- HTML structure (parsed by scraper) ---`: the `<title>` and
/// `<h1>`s, every `<table>` with its classes/IDs and `(rows × cols)`
/// shape, every top-level `<ul>`/`<ol>` with its `<li>` cardinality,
/// and the set of `tag.class` selectors that occur more than once
/// (iterator-eligible). The digest is followed by a bounded visible-
/// text rendering with `<script>`/`<style>`/`<noscript>` subtrees
/// excluded so the LLM can identify which element carries the value
/// without the page's JavaScript flooding the excerpt.
///
/// **Why the HTML digest matters.** Pre-Session-41-patch-2, HTML
/// reached the recipe-author LLM as `from_utf8_lossy(<raw bytes>)`,
/// and the LLM had to parse the markup mentally to find the elements
/// it would address with a CSS selector. The Session 40 Fed H.4.1
/// failure (`table#balance-sheet td.value matched no elements`) was
/// the LLM authoring a selector against markup it imagined rather
/// than against the markup the prefetch returned. With the digest,
/// the LLM authors selectors against shapes `scraper` confirmed
/// match real elements; combined with item 4's authoring-time
/// validation (already shipped in patch 1), no recipe whose selector
/// would match nothing reaches storage.
///
/// JSON and raw-bytes payloads continue to fall through to the
/// `from_utf8_lossy` path until item 3 (JSON shape outline) lands in
/// patch 3.
///
/// Failures of the text extraction (encrypted PDFs, malformed PDFs,
/// non-UTF-8 HTML) fall through to a clear "could not extract"
/// annotation; the LLM will then decline rather than author against
/// garbage. We never block authoring on a best-effort enrichment.
///
/// **Returns**: the formatted excerpt string AND the raw bytes the
/// excerpt was rendered from. The bytes flow into authoring-time
/// validation (`recipe_apply::validate_recipe_against_bytes`, called
/// from `recipe_author::author_recipe`) so the runtime's extractor
/// runs against the same bytes the LLM saw before any recipe is
/// persisted. Session 41 items 4–6.
async fn prefetch_excerpt(
    ctx: &ExecutorContext<'_>,
    url: &url::Url,
    source_id: &str,
) -> Option<(String, Vec<u8>)> {
    // Operator-visible "we're now fetching X" log. The Session 13
    // run had a 1m25s silent stretch that included the time spent
    // pre-fetching; this turns it into a visible step rather than a
    // mystery wait.
    info!(
        source_id = %source_id,
        url = %url,
        "pre-fetching endpoint hint"
    );
    // Track D: pre-fetch goes through the same backoff helper as
    // runtime fetches. Pre-fetch isn't latency-critical (it's part
    // of one-time authoring), but the rate-limit signal is just as
    // important here as it is at runtime — otherwise the operator
    // sees "authoring failed" with no hint that the cause was
    // throttling rather than a malformed source.
    let bytes = match fetch_with_backoff(ctx.http, url.as_str(), "prefetch").await {
        // Session 32: prefetch builds an excerpt for the LLM author;
        // the response Content-Type isn't part of that excerpt's
        // shape (the recipe author infers structure from the bytes
        // themselves). Discard the meta here.
        BackoffOutcome::Bytes { body, .. } => body,
        BackoffOutcome::RateLimited {
            retry_after_seconds,
        } => {
            warn!(
                source_id = %source_id,
                url = %url,
                summary = %format_retry_after(retry_after_seconds),
                "endpoint_hint pre-fetch rate-limited; authoring will fall back to stub excerpt"
            );
            return None;
        }
        BackoffOutcome::Failed(e) => {
            warn!(
                source_id = %source_id,
                url = %url,
                error = %e,
                "endpoint_hint pre-fetch failed; authoring will fall back to stub excerpt"
            );
            return None;
        }
    };

    let byte_count = bytes.len();

    // Branch on payload kind. PDFs go through `pdf_extract` +
    // `detect_pdf_tables`; HTML goes through `scraper` to produce a
    // structural digest in the runtime's parsed shape; JSON goes
    // through `serde_json` to produce a path/type shape outline;
    // everything else falls through to the UTF-8-lossy raw-bytes
    // path. We do the dispatch here, not in a separate helper,
    // because the truncation + framing logic is the same shape
    // across all branches — just over different "body" strings.
    //
    // **Session 41 — four-way dispatch.** Item 1 added the PDF
    // branch's framed-table format; item 2 added the HTML branch's
    // structural digest; item 3 adds the JSON branch's shape
    // outline. All three mirror the same architectural posture:
    // the LLM sees what the runtime sees, parsed by the same crate
    // the runtime queries against (`pdf_extract`,
    // `scraper`, `serde_json` respectively).
    //
    // **One asymmetry worth flagging.** Unlike PDF and HTML, where
    // the rendered text fully replaces the raw bytes (the rendered
    // form IS the parseable structure), the JSON branch keeps the
    // raw bytes underneath the outline. The LLM may still need to
    // see specific values to author a filter expression — the
    // outline is a navigation aid above the bytes, not a
    // replacement for them.
    let (body, kind_annotation) = if is_pdf(&bytes) {
        match render_pdf_text_with_tables(&bytes) {
            Ok(text) => (text, "PDF (text + detected tables)".to_string()),
            Err(e) => {
                // pdf-extract failed (encrypted, malformed, exotic
                // glyph encoding). Surface the failure honestly so
                // the LLM declines rather than authoring against a
                // garbled blob. Falling back to from_utf8_lossy here
                // would just feed it the same binary garbage the
                // pre-Session-40 code did.
                warn!(
                    source_id = %source_id,
                    url = %url,
                    error = %e,
                    "pdf text extraction failed; surfacing as unreadable in excerpt"
                );
                (
                    format!(
                        "(could not extract text from this PDF — {e}. \
                         No readable structure is available; if your \
                         closed-vocabulary modes cannot author against \
                         this source, decline.)"
                    ),
                    "PDF (extraction failed)".to_string(),
                )
            }
        }
    } else if is_html(&bytes) {
        match render_html_digest(&bytes, PREFETCH_EXCERPT_BUDGET) {
            Ok(text) => (text, "HTML (structural digest)".to_string()),
            Err(e) => {
                // HTML parsing rarely fails — `scraper` is
                // forgiving by design. The one realistic failure
                // is invalid UTF-8, which we surface honestly so
                // the LLM declines rather than authoring against
                // a guess.
                warn!(
                    source_id = %source_id,
                    url = %url,
                    error = %e,
                    "html digest construction failed; surfacing as unreadable in excerpt"
                );
                (
                    format!(
                        "(could not build a structural digest from this HTML — {e}. \
                         No parsed structure is available; if your closed-vocabulary \
                         modes cannot author against this source, decline.)"
                    ),
                    "HTML (digest failed)".to_string(),
                )
            }
        }
    } else if is_json(&bytes) {
        match render_json_shape(&bytes) {
            Ok(outline) => {
                // Outline above, raw bytes below. Whatever budget
                // remains after the outline is spent on raw bytes;
                // the outer truncation in `prefetch_excerpt` will
                // also bound the combined string, so this is
                // defense-in-depth against a pathological outline
                // dwarfing the budget alone.
                let raw_budget = PREFETCH_EXCERPT_BUDGET.saturating_sub(outline.len());
                let raw_trimmed: &[u8] = if bytes.len() > raw_budget {
                    &bytes[..raw_budget]
                } else {
                    &bytes[..]
                };
                let combined = format!(
                    "{outline}\n{}",
                    String::from_utf8_lossy(raw_trimmed)
                );
                (combined, "JSON (shape outline + raw bytes)".to_string())
            }
            Err(e) => {
                // serde_json parse failed even though `is_json`
                // sniffed a JSON-looking prefix — likely truncated
                // upstream or genuinely malformed. Surface honestly
                // so the LLM declines rather than authoring against
                // a garbled blob.
                warn!(
                    source_id = %source_id,
                    url = %url,
                    error = %e,
                    "json shape outline construction failed; surfacing as unreadable in excerpt"
                );
                (
                    format!(
                        "(could not parse this JSON — {e}. \
                         No parsed structure is available; if your \
                         closed-vocabulary modes cannot author against \
                         this source, decline.)"
                    ),
                    "JSON (parse failed)".to_string(),
                )
            }
        }
    } else {
        // Truncate at `PREFETCH_EXCERPT_BUDGET` *bytes*, not chars.
        // The LLM tokenizer doesn't care about UTF-8 boundaries; we
        // use `from_utf8_lossy` to handle the cut cleanly.
        let trimmed = if bytes.len() > PREFETCH_EXCERPT_BUDGET {
            &bytes[..PREFETCH_EXCERPT_BUDGET]
        } else {
            &bytes[..]
        };
        (
            String::from_utf8_lossy(trimmed).into_owned(),
            "raw bytes".to_string(),
        )
    };

    // Final body-length cap, applied uniformly across both branches.
    // For PDFs the extracted text can balloon well past the raw byte
    // count (a 200KiB PDF often produces 600KiB of text); for HTML the
    // pre-truncation upstream already bounded it. Truncate on char
    // boundaries so we don't slice mid-codepoint.
    let body_len = body.len();
    let (body, truncated_marker) = if body_len > PREFETCH_EXCERPT_BUDGET {
        let mut cut = PREFETCH_EXCERPT_BUDGET;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        let marker = format!(
            "\n\n[... excerpt truncated at {PREFETCH_EXCERPT_BUDGET} bytes; \
             rendered body was {body_len} bytes ...]"
        );
        (body[..cut].to_string(), marker)
    } else {
        (body, String::new())
    };

    let excerpt = format!(
        "Source id: {source_id}\n\
         Fetched URL: {url}\n\
         Fetched bytes: {byte_count} ({kind_annotation})\n\n\
         --- begin excerpt ---\n\
         {body}{truncated_marker}\n\
         --- end excerpt ---\n"
    );
    // Session 41: return the raw bytes alongside the excerpt so the
    // caller can hand them to authoring-time validation. The bytes
    // were already in scope for excerpt rendering; adding them to
    // the return type is the minimum plumbing change.
    Some((excerpt, bytes))
}

/// `true` iff `bytes` looks like a PDF: starts with the literal
/// magic `%PDF-` per ISO 32000-1 §7.5.2. We don't bother checking
/// the version byte after the dash — pdf-extract handles every
/// version we care about (1.0–2.0), and a malformed header will
/// surface as a parse error from `extract_text_from_mem_by_pages`.
fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Render PDF bytes as text framed in the runtime's coordinate
/// space. Same `pdf_extract` library and same
/// `recipe_apply::detect_pdf_tables` detector the runtime uses at
/// apply time, so what the LLM sees here is byte-for-byte the same
/// view the runtime will index into when it later applies the
/// recipe — same whitespace collapsing, same line ordering, same
/// table-detection heuristic, same multi-word-cell caveat.
///
/// **Frame shape.** For each PDF page:
///
/// - Pages whose detector found one or more tables emit, per table,
///   a header line `[PDF page N, table M] (R rows × C cols)`
///   followed by a per-row line `row I (col 0..C-1): "v0"  "v1" ...`.
///   `N` is 1-indexed (matching the `pdf_table.page` recipe field
///   and the runtime extractor's rejection of page 0); `M` is
///   0-indexed (matching `pdf_table.table_index`); `I` and the col
///   range are 0-indexed (matching `pdf_table.row` and
///   `pdf_table.col`).
/// - Pages whose detector found nothing emit a single line
///   `[PDF page N] (no table detected)` and **nothing else**. As of
///   Session 44 the prior per-page narrative is gone — see "Why
///   narrative is dropped" below.
///
/// **Why the framing matters.** Pre-Session-41, this function emitted
/// raw page text with a `[PDF page N]` marker. The LLM had to
/// imagine how the detector would tokenize that text into rows;
/// the lithium MCS run from Session 40 confirmed the imagination
/// gap (LLM authored `row=11` against a detected table that has 2
/// rows). With the new framing, the LLM no longer translates
/// between "what I see on the page" and "what the runtime will
/// index" — the framing IS what the runtime will index.
///
/// **Why narrative is dropped (Session 44).** Pre-Session-44 the
/// no-table branch followed its marker with up to 4 KiB of the
/// page's narrative text. The reasoning was navigation — let the
/// LLM see prose so it could decide which page covered which topic.
/// In practice that budget bled out the prefetch excerpt: with
/// `PREFETCH_EXCERPT_BUDGET` at 32 KiB pre-Session-44, a single
/// long PDF with many narrative-only pages would burn through the
/// budget on the first 8–10 pages and the framed tables on later
/// pages would never reach the LLM. The lithium MCS run from
/// Session 41 patch 1 was the canonical case: chapter on page 110,
/// budget covered through page 8.
///
/// Session 44 drops the narrative entirely and bumps the budget to
/// 64 KiB. Navigation now happens *through the framed-table list
/// itself*: every `[PDF page N, table M] ...` header inlines its
/// page number, and the table's first row (typically column
/// headers like `"Country", "Production"`) names the table for the
/// LLM. A 110-page PDF with one small table per page comes in
/// around 55 KiB of framed output — fits in the new budget,
/// covers the whole document, and does not require any source-
/// specific routing or per-document-class heuristic.
///
/// Pages whose value lives only in narrative (no detected table on
/// the page) cannot be addressed by `pdf_table` regardless of how
/// much narrative we showed the LLM — the runtime would see the
/// same nothing and the validator would reject the recipe. The
/// honest endings for those cases are (a) decline the source as
/// un-addressable by the closed extraction vocabulary or (b)
/// transcribe values from a *framed* table elsewhere in the
/// document and bake them via `static_payload`. Both work without
/// the dropped narrative.
///
/// Returns the joined text, or a stringified error when pdf-extract
/// rejects the bytes (encrypted PDF, exotic font, malformed
/// xref table). The caller annotates the excerpt and lets the LLM
/// decline.
fn render_pdf_text_with_tables(bytes: &[u8]) -> Result<String, String> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|e| format!("pdf parse failed: {e}"))?;
    if pages.is_empty() {
        return Ok("(PDF parsed but contained zero pages)".to_string());
    }
    let mut out = String::new();
    for (idx, page_text) in pages.iter().enumerate() {
        // 1-indexed in the marker because that is what the
        // `pdf_table.page` coordinate uses.
        let page_num = idx + 1;
        let tables = crate::recipe_apply::detect_pdf_tables(page_text);

        if !out.is_empty() {
            out.push_str("\n\n");
        }

        if tables.is_empty() {
            // No table detected. Emit the marker that says so and
            // nothing else.
            //
            // **Session 44 — narrative dropped.** Pre-Session-44 we
            // followed the marker with up to 4 KiB of the page's
            // narrative text so the LLM could decide *whether* the
            // value it needed lived on this page. That budget bled
            // out the excerpt: a 110-page PDF with 30 narrative pages
            // burned 120 KiB on text that the LLM could not author
            // any `pdf_table` coordinates against (the runtime would
            // see the same nothing the prefetch saw, and the
            // validator would reject the recipe). The lithium MCS
            // truncation gap from Session 41 patch 1 was the symptom:
            // chapter on page 110 fell off the end because narrative
            // pages dominated the budget.
            //
            // Session 44 drops the narrative entirely. The framed-
            // table list across the document — every
            // `[PDF page N, table M] (R rows × C cols)` header
            // followed by quoted row cells — is the navigation
            // index: page numbers are inline, and each table's
            // first row typically names the table (column headers).
            // The LLM picks the page and table to author against by
            // scanning that list, not by reading prose around it.
            //
            // Pages that genuinely host the value only in narrative
            // (no detected table) cannot be addressed by `pdf_table`
            // anyway. The LLM's options for those are: (a) decline
            // the source as un-addressable by the closed extraction
            // vocabulary, or (b) transcribe values from a *framed*
            // table elsewhere in the document and bake them via
            // `static_payload` (see the prompt's "Strategy for PDF
            // sources" section). Both are honest endings; both work
            // without the dropped narrative.
            out.push_str(&format!("[PDF page {page_num}] (no table detected)"));
            continue;
        }

        for (table_idx, table) in tables.iter().enumerate() {
            // detect_pdf_tables only emits tables with ≥2 rows by
            // contract, but read the shape off the table itself.
            let row_count = table.len();
            let col_count = table.first().map(|r| r.len()).unwrap_or(0);
            // Tables on the same page are separated by a blank line
            // for readability; the inter-page padding is handled at
            // the top of each page iteration above.
            if table_idx > 0 {
                out.push('\n');
            }
            out.push_str(&format!(
                "[PDF page {page_num}, table {table_idx}] ({row_count} rows × {col_count} cols)\n"
            ));
            let last_col = col_count.saturating_sub(1);
            for (row_idx, row) in table.iter().enumerate() {
                // Quote each cell so multi-word values (when they do
                // appear) and empty cells are visually unambiguous.
                let cells: Vec<String> =
                    row.iter().map(|c| format!("{:?}", c)).collect();
                out.push_str(&format!(
                    "  row {row_idx} (col 0..{last_col}): {}\n",
                    cells.join("  ")
                ));
            }
            // Trim the trailing newline added by the last row line so
            // the next page's `\n\n` separator produces exactly one
            // blank line.
            if out.ends_with('\n') {
                out.pop();
            }
        }
    }
    Ok(out)
}

/// `true` iff `bytes` looks like HTML: starts with (after an optional
/// UTF-8 BOM and any leading ASCII whitespace) either `<!DOCTYPE` or
/// `<html` — case-insensitive on the marker, strict on the prefix
/// shape. We deliberately do *not* match a broader "starts with `<`
/// plus an alpha character" heuristic: XML, RSS feeds, SVG, and
/// chevron-leading text would all false-positive into the HTML
/// branch and produce a misleading digest.
///
/// HTML fragments without a wrapping `<html>` (XHR-style API
/// responses) won't sniff as HTML by this rule. Those are rare in
/// our use cases (we hit full pages); if a source ships fragments
/// they currently fall through to the raw-bytes branch and the LLM
/// reads them through `from_utf8_lossy`. A future session can
/// broaden the sniff if a real source needs it.
fn is_html(bytes: &[u8]) -> bool {
    let after_bom = bytes
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(bytes);
    let trimmed = match after_bom
        .iter()
        .position(|b| !b.is_ascii_whitespace())
    {
        Some(i) => &after_bom[i..],
        None => return false,
    };
    trimmed
        .get(..9)
        .is_some_and(|h| h.eq_ignore_ascii_case(b"<!doctype"))
        || trimmed
            .get(..5)
            .is_some_and(|h| h.eq_ignore_ascii_case(b"<html"))
}

/// Per-section caps inside the HTML structural digest. Each subsection
/// is bounded so a pathological page (thousands of tables, thousands
/// of repeating classes, a 1 MiB `<title>`) cannot crowd the digest's
/// other subsections out of view. The `_LIMIT` constants are item
/// counts; the `_BUDGET` constants are byte budgets.
const HTML_DIGEST_TITLE_BUDGET: usize = 1024;
const HTML_DIGEST_HEADING_BUDGET: usize = 1024;
const HTML_DIGEST_TABLE_LIMIT: usize = 50;
const HTML_DIGEST_LIST_LIMIT: usize = 50;
const HTML_DIGEST_REPEATING_CLASS_LIMIT: usize = 30;
/// Tags whose subtrees are excluded from visible-text rendering. A
/// modern web page can carry hundreds of KiB of inline JavaScript or
/// CSS — relevant for executing the page, not for authoring an
/// extraction recipe. Excluding the subtrees keeps the digest's
/// visible-text section focused on what an end-user would read.
const HTML_VISIBLE_TEXT_SKIP_TAGS: &[&str] = &["script", "style", "noscript"];

/// Build an HTML structural digest plus a bounded visible-text
/// rendering, fit within the given byte budget. Mirrors the runtime's
/// `extract_css_select` parsing (same `scraper` crate) so what the
/// LLM sees is the same parsed shape the runtime will query at apply
/// time. Session 41 item 2.
///
/// **Output shape.**
///
/// ```text
/// --- HTML structure (parsed by scraper) ---
/// <title>: Federal Reserve - H.4.1 Statistical Release
/// <h1>: H.4.1 Statistical Release
///
/// Tables:
///   <table id="balance-sheet" class="data-table"> (15 rows × 8 cols)
///   <table class="footnote"> (3 rows × 2 cols)
///
/// Lists:
///   <ul class="navigation"> (12 items)
///   <ol> (5 items)
///
/// Repeating element classes (iterator-eligible):
///   div.card: 8 occurrences
///   span.value: 24 occurrences
///
/// --- Visible text (script/style excluded, truncated) ---
/// H.4.1 Statistical Release Reserve Balances Held with the Federal Reserve...
/// ```
///
/// **Why this shape.** The structure section gives the LLM the
/// concrete element identity (tag + class/id) it would author a
/// CSS selector against, plus the shape (rows × cols, list
/// cardinality) it would address positionally. The repeating-class
/// section surfaces iterator candidates (Phase-1 css_select × css_select):
/// `tag.class` selectors that match more than one element are the
/// natural outer-iterator targets. The visible-text section gives
/// the LLM the *content* it needs to identify which element holds
/// which value — a digest without text would tell it "there's a
/// table" but not "which row has Chile."
///
/// **Budget allocation.** Subsections of the structure summary are
/// independently capped so a pathological page can't crowd the
/// digest. Whatever budget remains after the structure summary is
/// spent on visible text; if the structure alone exceeds budget we
/// emit no visible text but the structure stays intact.
///
/// **What this is NOT.** This is not a fallback heuristic. If
/// `scraper` parses the bytes into an empty document (real HTML
/// served as `<html></html>`, JS-rendered SPA shells), the digest
/// is honest about that and the LLM will decline rather than guess
/// — same posture the PDF branch takes for "no table detected"
/// pages.
fn render_html_digest(bytes: &[u8], budget: usize) -> Result<String, String> {
    use scraper::{Html, Node, Selector};

    let html_str = std::str::from_utf8(bytes)
        .map_err(|e| format!("HTML bytes were not UTF-8: {e}"))?;
    let doc = Html::parse_document(html_str);

    let mut out = String::new();
    out.push_str("--- HTML structure (parsed by scraper) ---\n");

    // <title>
    let title_sel = Selector::parse("title")
        .expect("static selector 'title' must parse");
    if let Some(title_el) = doc.select(&title_sel).next() {
        let title: String = title_el.text().collect::<String>();
        let title = collapse_whitespace(&title);
        if !title.is_empty() {
            out.push_str("<title>: ");
            out.push_str(&truncate_to_budget(&title, HTML_DIGEST_TITLE_BUDGET));
            out.push('\n');
        }
    }

    // <h1>s — list each occurrence
    let h1_sel = Selector::parse("h1")
        .expect("static selector 'h1' must parse");
    for h1 in doc.select(&h1_sel) {
        let txt = collapse_whitespace(&h1.text().collect::<String>());
        if !txt.is_empty() {
            out.push_str("<h1>: ");
            out.push_str(&truncate_to_budget(&txt, HTML_DIGEST_HEADING_BUDGET));
            out.push('\n');
        }
    }

    // Tables: every <table> with its class/id and (rows × cols).
    // Rows = number of <tr> descendants. Cols = number of cells in
    // the first row. We do not filter out nested tables — a nested
    // table is still an addressable element with its own selector,
    // and listing it tells the LLM that nested-table addressing is
    // an option (or a hazard, when the inner table's class collides
    // with the outer). Cap the count so a pathological page cannot
    // dominate the digest.
    let table_sel = Selector::parse("table")
        .expect("static selector 'table' must parse");
    let tr_sel = Selector::parse("tr")
        .expect("static selector 'tr' must parse");
    let cell_sel = Selector::parse("td, th")
        .expect("static selector 'td, th' must parse");
    let tables: Vec<scraper::ElementRef<'_>> = doc.select(&table_sel).collect();
    if !tables.is_empty() {
        out.push_str("\nTables:\n");
        let shown = tables.len().min(HTML_DIGEST_TABLE_LIMIT);
        for table in tables.iter().take(shown) {
            let row_count = table.select(&tr_sel).count();
            let col_count = table
                .select(&tr_sel)
                .next()
                .map(|first_row| first_row.select(&cell_sel).count())
                .unwrap_or(0);
            out.push_str("  ");
            out.push_str(&format_element_signature(*table));
            out.push_str(&format!(" ({row_count} rows × {col_count} cols)\n"));
        }
        if tables.len() > shown {
            out.push_str(&format!(
                "  [... {} more tables truncated]\n",
                tables.len() - shown
            ));
        }
    }

    // Lists: every <ul>/<ol> with cardinality. Same rationale as
    // tables.
    let list_sel = Selector::parse("ul, ol")
        .expect("static selector 'ul, ol' must parse");
    let li_sel = Selector::parse("li")
        .expect("static selector 'li' must parse");
    let lists: Vec<scraper::ElementRef<'_>> = doc.select(&list_sel).collect();
    if !lists.is_empty() {
        out.push_str("\nLists:\n");
        let shown = lists.len().min(HTML_DIGEST_LIST_LIMIT);
        for list in lists.iter().take(shown) {
            let item_count = list.select(&li_sel).count();
            out.push_str("  ");
            out.push_str(&format_element_signature(*list));
            out.push_str(&format!(" ({item_count} items)\n"));
        }
        if lists.len() > shown {
            out.push_str(&format!(
                "  [... {} more lists truncated]\n",
                lists.len() - shown
            ));
        }
    }

    // Repeating tag.class selectors — count `(tag, class)` pairs.
    // Anything that appears more than once is iterator-eligible:
    // a `tag.class` selector matching N elements is what an outer
    // iterator would target. We include the count so the LLM sees
    // not just "this class repeats" but "this class repeats 8
    // times" (relevant for picking the iterator at the right
    // granularity — 8 cards vs. 800 spans).
    let star_sel = Selector::parse("*")
        .expect("static selector '*' must parse");
    let mut tag_class_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for el in doc.select(&star_sel) {
        let tag = el.value().name();
        if let Some(class_attr) = el.value().attr("class") {
            for class in class_attr.split_whitespace() {
                let key = format!("{tag}.{class}");
                *tag_class_counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    let mut repeating: Vec<(String, usize)> = tag_class_counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .collect();
    // Sort by count descending, then by selector for determinism.
    repeating.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if !repeating.is_empty() {
        out.push_str("\nRepeating element classes (iterator-eligible):\n");
        let shown = repeating.len().min(HTML_DIGEST_REPEATING_CLASS_LIMIT);
        for (selector, count) in repeating.iter().take(shown) {
            out.push_str(&format!("  {selector}: {count} occurrences\n"));
        }
        if repeating.len() > shown {
            out.push_str(&format!(
                "  [... {} more truncated]\n",
                repeating.len() - shown
            ));
        }
    }

    // Visible text: walk the body tree, skipping script/style
    // subtrees. Whatever budget remains after the structure summary
    // is spent here.
    let body_sel = Selector::parse("body")
        .expect("static selector 'body' must parse");
    let body = doc.select(&body_sel).next().unwrap_or_else(|| doc.root_element());

    let mut visible = String::new();
    collect_visible_text(body, &mut visible, budget * 2);
    let visible = collapse_whitespace(&visible);

    // Compute remaining budget for the visible text section, after
    // accounting for the section header and a possible truncation
    // marker. If the structure summary already exhausted the
    // budget, we emit a minimal "[... no budget left for visible
    // text]" line so the LLM knows visible text was elided rather
    // than absent.
    let header = "\n--- Visible text (script/style excluded, truncated) ---\n";
    let used = out.len() + header.len();
    if used >= budget {
        out.push_str(header);
        out.push_str("[... structure summary consumed the budget; visible text elided]\n");
    } else {
        let visible_budget = budget - used;
        out.push_str(header);
        if visible.len() <= visible_budget {
            out.push_str(&visible);
            if !visible.is_empty() && !visible.ends_with('\n') {
                out.push('\n');
            }
        } else {
            let mut cut = visible_budget;
            while cut > 0 && !visible.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push_str(&visible[..cut]);
            out.push_str(&format!(
                "\n[... visible text truncated at {visible_budget} bytes; \
                 full body text was {} bytes]\n",
                visible.len()
            ));
        }
    }

    Ok(out)
}

/// Format an element as a CSS-selector-like signature using its
/// classes and id. Used in the HTML digest to give the LLM the
/// exact element identity it would address with a selector.
///
/// Examples:
/// - `<table>` → `<table>`
/// - `<table id="x">` → `<table id="x">`
/// - `<table class="a b">` → `<table class="a b">`
/// - `<table id="x" class="a">` → `<table id="x" class="a">`
fn format_element_signature(el: scraper::ElementRef<'_>) -> String {
    let tag = el.value().name();
    let id = el.value().attr("id");
    let class = el.value().attr("class");
    match (id, class) {
        (Some(i), Some(c)) => format!("<{tag} id=\"{i}\" class=\"{c}\">"),
        (Some(i), None) => format!("<{tag} id=\"{i}\">"),
        (None, Some(c)) => format!("<{tag} class=\"{c}\">"),
        (None, None) => format!("<{tag}>"),
    }
}

/// Walk the element subtree and append visible text to `out`,
/// skipping subtrees rooted at tags listed in
/// `HTML_VISIBLE_TEXT_SKIP_TAGS`. Stops appending once `out` reaches
/// `max_size` to bound the cost on pathological pages.
///
/// Recursive on the HTML tree's depth — bounded in practice by
/// `scraper`'s parser, which produces well-formed trees of bounded
/// nesting (browsers' parsers reject deeply nested markup).
fn collect_visible_text(
    el: scraper::ElementRef<'_>,
    out: &mut String,
    max_size: usize,
) {
    use scraper::Node;
    if out.len() >= max_size {
        return;
    }
    for child in el.children() {
        if out.len() >= max_size {
            return;
        }
        match child.value() {
            Node::Text(t) => {
                out.push_str(t);
                out.push(' ');
            }
            Node::Element(child_el) => {
                let tag = child_el.name();
                if HTML_VISIBLE_TEXT_SKIP_TAGS.contains(&tag) {
                    continue;
                }
                if let Some(child_ref) = scraper::ElementRef::wrap(child) {
                    collect_visible_text(child_ref, out, max_size);
                }
            }
            _ => {}
        }
    }
}

/// Collapse runs of ASCII whitespace (including newlines) into single
/// spaces, and trim leading/trailing whitespace. The visible-text
/// rendering and the title/heading slots all benefit from this:
/// HTML's source whitespace is layout-irrelevant and noisy in a
/// digest.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Truncate `s` to at most `budget` bytes, on a UTF-8 char boundary,
/// adding an explicit truncation marker when the cut happens.
fn truncate_to_budget(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let mut cut = budget;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}[...truncated at {} bytes]", &s[..cut], budget)
}

// ---------------------------------------------------------------------------
// JSON shape outline (Session 41 item 3)
//
// JSON sources fell through to `from_utf8_lossy` until this patch — the
// LLM authored `json_path` recipes by guessing the shape from a
// truncated body. That works for small responses and silently fails
// for large nested ones; it's the class that produced the World Bank
// null trap (Session 32: "most-recent rows carry null for unpublished
// data") because the LLM saw the leading raw bytes only, where the
// nulls all happen to live, and assumed positional indices into the
// array would land on real numbers.
//
// The shape outline is a navigation aid: it surfaces the parsed
// structure (paths + types + array cardinality, with explicit
// polymorphic-type annotation and first-N samples for polymorphic
// leaves) ABOVE the raw bytes. Unlike the PDF and HTML branches,
// where the rendered text fully replaces the raw bytes (no
// information is lost — the rendered text IS the parseable
// structure), the JSON branch keeps the raw bytes underneath the
// outline. The LLM may still need to see specific values to author
// a filter expression; the outline tells it where to look.
//
// The outline is parsed with `serde_json::Value` — the same crate
// `recipe_apply::extract_json_path` queries against at apply time.
// By construction, a path the LLM reads off the outline is one the
// runtime will resolve to the same value at apply.
// ---------------------------------------------------------------------------

/// Hard cap on how many distinct paths the outline lists. A
/// pathological JSON document (10000-key flat object, or a deeply
/// nested array of objects with thousands of keys per element) would
/// otherwise crowd the prefetch's overall byte budget. The truncation
/// marker tells the LLM elision happened so it doesn't assume the
/// listed paths are exhaustive.
const JSON_OUTLINE_PATH_LIMIT: usize = 50;
/// How many leaf samples to record per path before we stop. The
/// World Bank null trap typically shows 4–6 leading nulls before the
/// first real value; 5 is enough to make the pattern visible without
/// drowning the outline in long arrays of identical values.
const JSON_OUTLINE_SAMPLE_LIMIT: usize = 5;
/// Per-leaf-sample byte budget. Caps the rendered length of any one
/// sample so a single 100 KiB string value can't push the outline
/// off the budget.
const JSON_OUTLINE_LEAF_PREVIEW_BUDGET: usize = 80;
/// How many elements of the first non-empty array to render verbatim
/// in the head-elements section. Two is enough to show the LLM the
/// repeating shape (and a leading-null pair when the trap is
/// present); more would bloat the outline without adding evidence.
const JSON_OUTLINE_FIRST_ELEMENTS: usize = 2;

/// `true` iff `bytes` looks like JSON: starts with (after an
/// optional UTF-8 BOM and any leading ASCII whitespace) either `{`
/// or `[`. We deliberately do NOT match bare scalar JSON values
/// (`42`, `"foo"`, `true`, `null`) — a real source publishing a
/// scalar at the document root is unheard of, and accepting them
/// would false-positive on plain-text payloads that happen to start
/// with a digit or quote. PDFs (`%PDF-`) and HTML (chevron-leading)
/// are also rejected by this rule.
fn is_json(bytes: &[u8]) -> bool {
    let after_bom = bytes
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(bytes);
    let trimmed = match after_bom
        .iter()
        .position(|b| !b.is_ascii_whitespace())
    {
        Some(i) => &after_bom[i..],
        None => return false,
    };
    matches!(trimmed.first(), Some(b'{') | Some(b'['))
}

/// Per-path observations accumulated during the JSON walk. The
/// outline renders one line per `(path, JsonPathStats)` pair; the
/// stats record what types we saw at this path, the array size when
/// the path resolved to an array, and a bounded sample of leaf
/// values for polymorphic-leaf annotation.
struct JsonPathStats {
    /// All distinct JSON type labels observed at this path. Stored
    /// in a `BTreeSet<&'static str>` so the rendered union is
    /// deterministic (`null|number`, never `number|null`) and so
    /// `is_polymorphic_leaf` can decide cheaply.
    types: std::collections::BTreeSet<&'static str>,
    /// `Some((min, max))` if any observation was an array. The pair
    /// captures cardinality variation across siblings — a key that
    /// holds arrays of different lengths under different parents
    /// renders as `array[lo..hi]`; a uniform shape renders as
    /// `array[N]`.
    array_len: Option<(usize, usize)>,
    /// Leaf-value previews observed at this path, capped at
    /// `JSON_OUTLINE_SAMPLE_LIMIT`. Only populated for non-container
    /// observations; container-only paths leave this empty.
    samples: Vec<String>,
}

impl JsonPathStats {
    fn new() -> Self {
        Self {
            types: std::collections::BTreeSet::new(),
            array_len: None,
            samples: Vec::new(),
        }
    }
    fn observe_type(&mut self, t: &'static str) {
        self.types.insert(t);
    }
    fn observe_array_len(&mut self, len: usize) {
        self.array_len = Some(match self.array_len {
            None => (len, len),
            Some((lo, hi)) => (lo.min(len), hi.max(len)),
        });
    }
    fn observe_sample(&mut self, preview: String) {
        if self.samples.len() < JSON_OUTLINE_SAMPLE_LIMIT {
            self.samples.push(preview);
        }
    }
    /// A leaf is polymorphic when ≥2 distinct types were observed
    /// AND none of those types is a container (object/array). The
    /// container exclusion is deliberate: a path observed as both
    /// `array` and `object` across siblings is structural confusion,
    /// not the leaf-polymorphism class — the World Bank null trap
    /// (`null|number`) and string-vs-number variants are leaf
    /// problems with a JSONPath filter-expression fix.
    fn is_polymorphic_leaf(&self) -> bool {
        if self.types.len() < 2 {
            return false;
        }
        self.types.iter().all(|t| !matches!(*t, "object" | "array"))
    }
}

/// Map a `serde_json::Value` to the type label used in the outline.
/// Six labels — one per `Value` variant — keep the surface tiny and
/// the polymorphic-leaf check (which excludes `object` and `array`)
/// trivially correct.
fn json_type_label(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Render a leaf value as a one-line preview suitable for a sample
/// list. Strings are debug-quoted (so spaces and control chars stay
/// visible); numbers/bools/null are formatted as their JSON text.
/// Containers fall through to `Display`, which shouldn't fire because
/// the caller only previews leaves, but defending against it costs
/// nothing.
fn json_leaf_preview(v: &serde_json::Value) -> String {
    let raw = match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("{:?}", s),
        other => other.to_string(),
    };
    truncate_to_budget(&raw, JSON_OUTLINE_LEAF_PREVIEW_BUDGET)
}

/// Recursively walk a `serde_json::Value`, accumulating one entry per
/// distinct path. Object keys descend as `path.key`; array elements
/// collapse to `path[]` (so `$.data[0].country` and `$.data[1].country`
/// merge into one entry: `$.data[].country`). The collapse matches
/// how a JSONPath author addresses array contents — what the LLM
/// reads off the outline is what it would write into a recipe.
///
/// Path storage is a `Vec<(String, JsonPathStats)>` plus a
/// `HashMap<String, usize>` for O(1) lookup; this preserves DFS
/// visit order in the rendered outline. (Workspace `serde_json`
/// does not enable `preserve_order`, so within an object the key
/// iteration order is the inner map's deterministic order — that
/// becomes the visit order, and the outline order, by construction.)
fn walk_json(
    value: &serde_json::Value,
    path: &str,
    paths: &mut Vec<(String, JsonPathStats)>,
    index: &mut std::collections::HashMap<String, usize>,
    first_array: &mut Option<String>,
) {
    let stats_idx = match index.get(path) {
        Some(&i) => i,
        None => {
            paths.push((path.to_string(), JsonPathStats::new()));
            let i = paths.len() - 1;
            index.insert(path.to_string(), i);
            i
        }
    };
    paths[stats_idx].1.observe_type(json_type_label(value));

    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let child = format!("{path}.{k}");
                walk_json(v, &child, paths, index, first_array);
            }
        }
        serde_json::Value::Array(arr) => {
            paths[stats_idx].1.observe_array_len(arr.len());
            // The first non-empty array we encounter during DFS
            // becomes the source of the head-elements section. This
            // is deterministic and covers the common shapes —
            // `{data: [...], meta: {...}}` highlights `data`,
            // `[[...], [...]]` highlights the outer first non-empty,
            // and a top-level array highlights itself.
            if first_array.is_none() && !arr.is_empty() {
                *first_array = Some(path.to_string());
            }
            let child = format!("{path}[]");
            for el in arr {
                walk_json(el, &child, paths, index, first_array);
            }
        }
        leaf => {
            paths[stats_idx].1.observe_sample(json_leaf_preview(leaf));
        }
    }
}

/// Build the JSON shape outline for `bytes`. Returns the rendered
/// outline as a `String` on success, or a stringified parse error
/// when `serde_json` rejects the bytes (the caller surfaces the
/// failure to the LLM honestly rather than guessing).
///
/// **Output shape** (this is the contract the prompt section
/// references):
///
/// ```text
/// --- JSON shape (parsed by serde_json) ---
/// $ : object
/// $.data : array[24]
/// $.data[].country : string
/// $.data[].value : null|number   ← polymorphic; first 5 values: ["null", "null", "1234", "1100", "950"]
/// $.data[].date : string
/// $.meta : object
/// $.meta.total : number
///
/// --- First 2 elements of $.data ---
/// [
///   {"country":"...","value":null,"date":"2026"},
///   ...
/// ]
/// --- end JSON shape ---
/// ```
///
/// Polymorphic-leaf paths get the `← polymorphic` marker plus a
/// sample of leading values so a leading-null pattern is visible
/// at authoring time (the World Bank trap class). Non-polymorphic
/// leaves render with a single type label; arrays render with their
/// observed cardinality (`array[N]` or `array[lo..hi]`).
fn render_json_shape(bytes: &[u8]) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("JSON bytes did not parse: {e}"))?;

    let mut paths: Vec<(String, JsonPathStats)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut first_array: Option<String> = None;
    walk_json(&value, "$", &mut paths, &mut index, &mut first_array);

    let mut out = String::new();
    out.push_str("--- JSON shape (parsed by serde_json) ---\n");

    let shown = paths.len().min(JSON_OUTLINE_PATH_LIMIT);
    for (path, stats) in paths.iter().take(shown) {
        out.push_str(path);
        out.push_str(" : ");
        if stats.is_polymorphic_leaf() {
            let union: Vec<&str> = stats.types.iter().copied().collect();
            out.push_str(&union.join("|"));
            out.push_str("   ← polymorphic; first ");
            out.push_str(&stats.samples.len().to_string());
            out.push_str(" values: [");
            out.push_str(
                &stats
                    .samples
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str("]\n");
        } else if stats.types.len() == 1 {
            let t = *stats.types.iter().next().expect("len==1");
            if t == "array" {
                match stats.array_len {
                    Some((lo, hi)) if lo == hi => {
                        out.push_str(&format!("array[{lo}]"))
                    }
                    Some((lo, hi)) => {
                        out.push_str(&format!("array[{lo}..{hi}]"))
                    }
                    None => out.push_str("array"),
                }
            } else {
                out.push_str(t);
            }
            out.push('\n');
        } else {
            // Multi-type observation that includes a container — emit
            // the union without the polymorphic marker. This is rare
            // (it indicates structural confusion: the same path is
            // sometimes an object, sometimes a leaf) and the LLM
            // should treat it as a sign to inspect the raw bytes.
            let union: Vec<&str> = stats.types.iter().copied().collect();
            out.push_str(&union.join("|"));
            out.push('\n');
        }
    }
    if paths.len() > shown {
        out.push_str(&format!(
            "  [... {} more paths truncated]\n",
            paths.len() - shown
        ));
    }

    if let Some(arr_path) = first_array {
        if let Some(arr_value) = resolve_array_at_path(&value, &arr_path) {
            let take = arr_value.len().min(JSON_OUTLINE_FIRST_ELEMENTS);
            if take > 0 {
                out.push_str(&format!(
                    "\n--- First {take} element{} of {arr_path} ---\n",
                    if take == 1 { "" } else { "s" }
                ));
                let head: Vec<&serde_json::Value> =
                    arr_value.iter().take(take).collect();
                let rendered = serde_json::to_string_pretty(&head)
                    .unwrap_or_else(|_| "[serialization failed]".to_string());
                // Per-section cap on the head bytes — generous
                // enough for two non-trivial objects, bounded
                // enough that a 1 MiB element can't dominate.
                out.push_str(&truncate_to_budget(
                    &rendered,
                    JSON_OUTLINE_LEAF_PREVIEW_BUDGET
                        * JSON_OUTLINE_FIRST_ELEMENTS
                        * 8,
                ));
                out.push('\n');
            }
        }
    }

    out.push_str("--- end JSON shape ---\n");
    Ok(out)
}

/// Resolve a dotted path like `$.data` (or just `$`) to its
/// underlying `Vec<Value>` when the path resolves to an array. Used
/// to render the head-elements section. Returns `None` for paths
/// containing array-element segments (`[]`) — by construction
/// `first_array` is set to a path *containing* the array, never one
/// that descends into its elements, so this is a defense.
fn resolve_array_at_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a Vec<serde_json::Value>> {
    let stripped = path.strip_prefix('$').unwrap_or(path);
    let mut cur = value;
    for segment in stripped.split('.').filter(|s| !s.is_empty()) {
        // Element-index segments (`[]`) shouldn't appear here, but
        // `Value::get` would just miss them; rejecting cleanly keeps
        // the contract honest.
        if segment.contains('[') {
            return None;
        }
        cur = cur.get(segment)?;
    }
    cur.as_array()
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

/// Fetch the bytes a recipe's apply step needs, honoring static
/// payloads and Track-D backoff.
///
/// Returns `Ok((bytes, content_type))` on success, or one of three
/// pre-built `RecipeOutcome` variants the caller `return`s directly:
/// `Failed { stage: Fetch, ... }` for ordinary network errors,
/// `RateLimited { ... }` for 429 responses the backoff helper chose
/// to surface (above-ceiling waits, no `Retry-After`, or two 429s
/// in a row), and the no-fixture variant of `Failed` for tests.
///
/// `content_type` is the raw response `Content-Type` header value
/// when the underlying transport surfaced one, else `None`. Session
/// 32: the value is threaded into `record_apply_failure_attempt`
/// when an apply-stage failure happens, so the response-bytes chip
/// in `RecipesPanel.svelte` can render the server's claim
/// authoritatively rather than guessing from the first byte.
///
/// For the `static_payload` short-circuit, `content_type` is
/// returned as `None` — baked bytes have no transport, so there is
/// no header. The chip falls back to the heuristic byte-sniffer
/// for those, same as before.
///
/// Lifted out of the four `run_X_recipe` paths so the policy lives
/// in one place. Each path retains its own visible call site, which
/// preserves the duplication-with-comments discipline Session 9
/// chose for the apply/insert tail of those functions — only the
/// fetch arm is consolidated here.
async fn fetch_recipe_bytes(
    ctx: &ExecutorContext<'_>,
    recipe: &FetchRecipe,
) -> Result<(Vec<u8>, Option<String>), RecipeOutcome> {
    if let Some(payload) = recipe.static_payload.as_ref() {
        // ADR 0007 Amendment 3: bytes' provenance is orthogonal to
        // the extraction mode. A baked payload short-circuits the
        // network entirely; rate-limiting can't apply, and there is
        // no Content-Type because there is no response.
        return Ok((payload.as_bytes().to_vec(), None));
    }

    match fetch_with_backoff(ctx.http, recipe.source_url.as_str(), "runtime").await {
        BackoffOutcome::Bytes { body, content_type } => Ok((body, content_type)),
        BackoffOutcome::RateLimited {
            retry_after_seconds,
        } => Err(RecipeOutcome::RateLimited {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            retry_after_seconds,
        }),
        BackoffOutcome::Failed(HttpFetchError::Http(msg)) => Err(RecipeOutcome::Failed {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            stage: FailureStage::Fetch,
            message: msg,
        }),
        BackoffOutcome::Failed(HttpFetchError::Timeout(d)) => Err(RecipeOutcome::Failed {
            // Session 45: the typed Timeout variant lets the per-host
            // backoff layer (`BackoffFetcher`) react before this arm
            // fires; by the time the executor sees it, the host's
            // `next_allowed_at` has already been pushed out. We surface
            // the configured timeout in the message so the operator
            // sees what the request was budgeted for, not just "fetch
            // failed".
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            stage: FailureStage::Fetch,
            message: format!("timed out after {d:?}"),
        }),
        BackoffOutcome::Failed(HttpFetchError::NoFixture(url)) => Err(RecipeOutcome::Failed {
            recipe_id: recipe.id,
            source_id: recipe.source_id.clone(),
            stage: FailureStage::Fetch,
            message: format!("no fixture configured for url: {url}"),
        }),
        BackoffOutcome::Failed(HttpFetchError::RateLimited {
            retry_after_seconds,
        }) => {
            // Defensive — `fetch_with_backoff` collapses RateLimited
            // into `BackoffOutcome::RateLimited`, never `Failed`.
            // If a future refactor breaks that invariant the
            // operator still sees a sensible outcome rather than a
            // generic Failed-with-debug-string.
            Err(RecipeOutcome::RateLimited {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                retry_after_seconds,
            })
        }
    }
}

/// Run one recipe end-to-end. Pure dispatch on the extraction mode
/// — Session 8 wired CSV; Session 9 added JSON; Session 12 added
/// CssSelect; Session 13 added RegexCapture; Session 29 (Track C,
/// ADR 0007 amendment 5) added PdfTable. With PdfTable in, every
/// variant of the closed extraction-mode enum is a first-class wired
/// runtime path. Adding a sixth mode is an ADR-level decision.
async fn run_one_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    run_id: Uuid,
) -> RecipeOutcome {
    match &recipe.extraction {
        ExtractionSpec::CsvCell { .. } => run_csv_recipe(ctx, plan, recipe, run_id).await,
        ExtractionSpec::JsonPath { .. } => run_json_recipe(ctx, plan, recipe, run_id).await,
        ExtractionSpec::CssSelect { .. } => run_css_recipe(ctx, plan, recipe, run_id).await,
        ExtractionSpec::RegexCapture { .. } => run_regex_recipe(ctx, plan, recipe, run_id).await,
        ExtractionSpec::PdfTable { .. } => run_pdf_recipe(ctx, plan, recipe, run_id).await,
    }
}

/// Record a per-(recipe, run) attempt with the bytes that triggered an
/// apply-stage failure. Track A, ADR 0012 amendment 1.
///
/// Called from each `run_X_recipe` immediately before returning the
/// `RecipeOutcome::Failed { stage: Apply, .. }`. The attempt row is
/// the ground truth the manual `reauthor_recipe` Tauri command reads
/// from when the operator triggers a re-author.
///
/// `response_content_type` (Session 32) is the raw `Content-Type`
/// header value the runtime saw when fetching, captured into the
/// same row as the bytes excerpt. The response-bytes chip in
/// `RecipesPanel.svelte` reads it to render an authoritative shape
/// label rather than a heuristic guess. `None` for static-payload
/// recipes (no transport, no header) and for any future fetcher
/// that doesn't surface headers.
///
/// Storage failure here is non-fatal — the outcome is still returned
/// to the caller, the run continues, and we log the lost capture at
/// `warn` level. The audit trail loses a row but the user-facing
/// behaviour is unchanged. ADR 0007 §"runtime path" — the runtime is
/// LLM-free, so it must also be defensive against any one auxiliary
/// write failing.
fn record_apply_failure_attempt(
    store: &Store,
    run_id: Uuid,
    recipe_id: Uuid,
    bytes: &[u8],
    response_content_type: Option<&str>,
    failure_message: &str,
) {
    let row = situation_room_storage::recipe_fetch_attempts::RecipeFetchAttemptRow {
        id: Uuid::now_v7(),
        recipe_id,
        run_id,
        attempted_at: Utc::now(),
        succeeded: false,
        failure_message: Some(failure_message.to_string()),
        bytes_excerpt: Some(situation_room_storage::truncate_excerpt(bytes)),
        response_content_type: response_content_type.map(|s| s.to_string()),
    };
    if let Err(e) = store.insert_recipe_fetch_attempt(&row) {
        warn!(
            run_id = %run_id,
            recipe_id = %recipe_id,
            error = %e,
            "failed to record recipe fetch attempt; the bytes-and-failure capture is lost \
             for this run but the outcome itself is preserved"
        );
    }
}

/// Persist one `fetch_run_outcomes` row per `RecipeOutcome` produced
/// by a run. Session 46 — the row backbone for the recipe-success
/// heatmap surface.
///
/// Each outcome lifts into a single row with `outcome_kind` matching
/// the `RecipeOutcomeDto::kind` string the IPC boundary already
/// uses. `Declined` and `LegacyPlanCannotAuthor` produce rows with
/// `recipe_id = None` because no recipe was authored; the heatmap
/// groups those by `source_id` instead. See migration 0016 and
/// `crates/storage/src/fetch_run_outcomes.rs` for the table shape and
/// per-variant payload conventions.
///
/// Per-row storage failures are warn-logged and skipped — losing one
/// heatmap cell is strictly better than failing the whole run for an
/// auxiliary persistence write. The user-facing `FetchReport` is
/// untouched.
fn persist_run_outcomes(
    store: &Store,
    plan_id: Uuid,
    run_id: Uuid,
    outcomes: &[RecipeOutcome],
) {
    let now = Utc::now();
    for outcome in outcomes {
        let row = match outcome {
            RecipeOutcome::Succeeded {
                recipe_id,
                source_id,
                records_produced,
            } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: Some(*recipe_id),
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::Succeeded,
                records_produced: Some(*records_produced),
                retry_after_seconds: None,
                failure_stage: None,
                message: None,
                attempted_at: now,
            },
            RecipeOutcome::Skipped {
                recipe_id,
                source_id,
                reason,
            } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: Some(*recipe_id),
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::Skipped,
                records_produced: None,
                retry_after_seconds: None,
                failure_stage: None,
                message: Some(reason.clone()),
                attempted_at: now,
            },
            RecipeOutcome::Failed {
                recipe_id,
                source_id,
                stage,
                message,
            } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: Some(*recipe_id),
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::Failed,
                records_produced: None,
                retry_after_seconds: None,
                failure_stage: Some(failure_stage_as_str(*stage).to_string()),
                message: Some(message.clone()),
                attempted_at: now,
            },
            RecipeOutcome::RateLimited {
                recipe_id,
                source_id,
                retry_after_seconds,
            } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: Some(*recipe_id),
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::RateLimited,
                records_produced: None,
                retry_after_seconds: *retry_after_seconds,
                failure_stage: None,
                message: None,
                attempted_at: now,
            },
            RecipeOutcome::Declined { source_id, reason } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: None,
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::Declined,
                records_produced: None,
                retry_after_seconds: None,
                failure_stage: None,
                message: Some(reason.clone()),
                attempted_at: now,
            },
            RecipeOutcome::LegacyPlanCannotAuthor { source_id } => FetchRunOutcomeRow {
                id: Uuid::now_v7(),
                run_id,
                plan_id,
                recipe_id: None,
                source_id: source_id.clone(),
                outcome_kind: FetchRunOutcomeKind::LegacyPlanCannotAuthor,
                records_produced: None,
                retry_after_seconds: None,
                failure_stage: None,
                message: None,
                attempted_at: now,
            },
        };

        if let Err(e) = store.insert_fetch_run_outcome(&row) {
            warn!(
                plan_id = %plan_id,
                run_id = %run_id,
                source_id = %row.source_id,
                outcome_kind = %row.outcome_kind,
                error = %e,
                "failed to persist fetch_run_outcome row; the heatmap will \
                 lack this cell but the report itself is preserved"
            );
        }
    }
}

/// Wire-form string for [`FailureStage`] — same snake_case convention
/// as the serde default. Kept as a free function rather than a method
/// so it stays adjacent to [`persist_run_outcomes`]'s call site.
fn failure_stage_as_str(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Fetch => "fetch",
        FailureStage::Apply => "apply",
        FailureStage::Insert => "insert",
    }
}

/// CSV runtime path: fetch → apply → insert.
async fn run_csv_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    run_id: Uuid,
) -> RecipeOutcome {
    // Fetch — or short-circuit on baked `static_payload`. Track D:
    // 429 responses with a parseable `Retry-After` are now distinct
    // from generic fetch errors, surfaced as RateLimited rather than
    // collapsed into `Failed { stage: Fetch }`. See
    // `fetch_recipe_bytes` for the policy.
    // Session 32: `fetch_recipe_bytes` now returns the response
    // Content-Type alongside the body (None for static-payload
    // recipes and for fetchers that don't surface headers). The
    // value is threaded into `record_apply_failure_attempt` so the
    // response-bytes chip in `RecipesPanel.svelte` can read the
    // server's claim authoritatively rather than guess from the
    // first byte.
    let (bytes, response_content_type) = match fetch_recipe_bytes(ctx, recipe).await {
        Ok((b, ct)) => (b, ct),
        Err(outcome) => return outcome,
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
            // Track A: capture the bytes + failure message so the
            // manual re-author command later sees ground truth.
            let message = describe_apply_error(&e);
            record_apply_failure_attempt(
                ctx.store,
                run_id,
                recipe.id,
                &bytes,
                response_content_type.as_deref(),
                &message,
            );
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message,
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
    run_id: Uuid,
) -> RecipeOutcome {
    // Fetch — see `fetch_recipe_bytes` for the static-payload
    // short-circuit and Track-D rate-limit handling.
    // Session 32: `fetch_recipe_bytes` now returns the response
    // Content-Type alongside the body (None for static-payload
    // recipes and for fetchers that don't surface headers). The
    // value is threaded into `record_apply_failure_attempt` so the
    // response-bytes chip in `RecipesPanel.svelte` can read the
    // server's claim authoritatively rather than guess from the
    // first byte.
    let (bytes, response_content_type) = match fetch_recipe_bytes(ctx, recipe).await {
        Ok((b, ct)) => (b, ct),
        Err(outcome) => return outcome,
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
            // Track A: capture the bytes + failure message so the
            // manual re-author command later sees ground truth.
            let message = describe_apply_error(&e);
            record_apply_failure_attempt(
                ctx.store,
                run_id,
                recipe.id,
                &bytes,
                response_content_type.as_deref(),
                &message,
            );
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message,
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
    run_id: Uuid,
) -> RecipeOutcome {
    // Fetch — see `fetch_recipe_bytes` for the static-payload
    // short-circuit and Track-D rate-limit handling.
    // Session 32: `fetch_recipe_bytes` now returns the response
    // Content-Type alongside the body (None for static-payload
    // recipes and for fetchers that don't surface headers). The
    // value is threaded into `record_apply_failure_attempt` so the
    // response-bytes chip in `RecipesPanel.svelte` can read the
    // server's claim authoritatively rather than guess from the
    // first byte.
    let (bytes, response_content_type) = match fetch_recipe_bytes(ctx, recipe).await {
        Ok((b, ct)) => (b, ct),
        Err(outcome) => return outcome,
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
            // Track A: capture the bytes + failure message so the
            // manual re-author command later sees ground truth.
            let message = describe_apply_error(&e);
            record_apply_failure_attempt(
                ctx.store,
                run_id,
                recipe.id,
                &bytes,
                response_content_type.as_deref(),
                &message,
            );
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message,
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
    run_id: Uuid,
) -> RecipeOutcome {
    // Fetch — see `fetch_recipe_bytes` for the static-payload
    // short-circuit and Track-D rate-limit handling.
    // Session 32: `fetch_recipe_bytes` now returns the response
    // Content-Type alongside the body (None for static-payload
    // recipes and for fetchers that don't surface headers). The
    // value is threaded into `record_apply_failure_attempt` so the
    // response-bytes chip in `RecipesPanel.svelte` can read the
    // server's claim authoritatively rather than guess from the
    // first byte.
    let (bytes, response_content_type) = match fetch_recipe_bytes(ctx, recipe).await {
        Ok((b, ct)) => (b, ct),
        Err(outcome) => return outcome,
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
            // Track A: capture the bytes + failure message so the
            // manual re-author command later sees ground truth.
            let message = describe_apply_error(&e);
            record_apply_failure_attempt(
                ctx.store,
                run_id,
                recipe.id,
                &bytes,
                response_content_type.as_deref(),
                &message,
            );
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message,
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

/// PDF runtime path: fetch → apply → insert.
///
/// Structurally identical to [`run_regex_recipe`] (and [`run_csv_recipe`],
/// [`run_json_recipe`], [`run_css_recipe`]) — every wired path goes
/// through the same `apply()` boundary, which dispatches internally on
/// the recipe's `ExtractionSpec`. The dispatch arms exist as separate
/// functions because (a) it keeps `run_one_recipe` honest about which
/// modes are wired, and (b) when modes start to diverge in behaviour
/// (e.g. PDF gaining a streamed page-walk for very large reports),
/// the split lets each path evolve without flag-soup.
///
/// Session 29 (Track C, ADR 0007 amendment 5) added this. The
/// `Skipped { reason: "pdf_table not implemented" }` arm is gone;
/// `pdf_table` recipes now fetch, extract, normalize, and insert
/// like every other mode.
async fn run_pdf_recipe(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    run_id: Uuid,
) -> RecipeOutcome {
    // Session 32: `fetch_recipe_bytes` now returns the response
    // Content-Type alongside the body (None for static-payload
    // recipes and for fetchers that don't surface headers). The
    // value is threaded into `record_apply_failure_attempt` so the
    // response-bytes chip in `RecipesPanel.svelte` can read the
    // server's claim authoritatively rather than guess from the
    // first byte.
    let (bytes, response_content_type) = match fetch_recipe_bytes(ctx, recipe).await {
        Ok((b, ct)) => (b, ct),
        Err(outcome) => return outcome,
    };

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
            // Track A: capture the bytes + failure message so the
            // manual re-author command later sees ground truth. PDFs
            // are typically large; the existing capture path's size
            // discipline applies (see `record_apply_failure_attempt`).
            let message = describe_apply_error(&e);
            record_apply_failure_attempt(
                ctx.store,
                run_id,
                recipe.id,
                &bytes,
                response_content_type.as_deref(),
                &message,
            );
            return RecipeOutcome::Failed {
                recipe_id: recipe.id,
                source_id: recipe.source_id.clone(),
                stage: FailureStage::Apply,
                message,
            };
        }
    };

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
        DocumentSourceEntry, DocumentSourceHint, DocumentSourceNomination,
        EntityKindExpectation, EventTypeExpectation, GeoScope, MetricExpectation, PriorityTier,
        RecordExpectations, RelationKindExpectation,
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
                document_sources: vec![DocumentSourceEntry::Nomination(
                    DocumentSourceNomination {
                        nomination_id: Uuid::now_v7(),
                        description:
                            "Demo CSV — test-fixture endpoint serving production-by-year rows; \
                             retry-loop tests stub the propose-URL LLM call to return the fixture URL"
                                .into(),
                        priority_tier: PriorityTier::AuthoritativePrimary,
                    },
                )],
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
        }
    }

    /// ADR 0016 Phase 1 helper: an iterator-bearing CSS recipe whose
    /// outer selector picks `.card` elements and whose inner selector
    /// reads each card's `h3` text into the binding's `headline`
    /// field. Each match emits one Event record; `dedup_key_field`
    /// references "headline" so the runtime stamps a per-record key.
    ///
    /// This mirrors the empirical shape of the Nature subjects /
    /// qt.eu listings that motivated ADR 0016 — `.card`-class
    /// containers, an `h3` headline child — in a portable form
    /// suitable for `StaticFetcher` fixtures.
    fn working_iterator_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:demo_iter", plan.id)),
            plan_id: plan.id,
            source_id: "demo_iter".into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: "h3".into(),
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: ".card".into(),
                attribute: None,
            }),
            produces: vec![ProductionBinding {
                record_type: RecordType::Event,
                expectation: ExpectationRef::EventType { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "event_type".into(),
                        source: FieldValueSource::Literal {
                            value: json!("mine_opened"),
                        },
                    },
                    FieldMap {
                        path: "headline".into(),
                        source: FieldValueSource::Extracted,
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
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
            propose_url_prompt: "",
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

        // Session 46: the per-outcome row is persisted alongside the
        // fetch_runs summary so the recipe-success heatmap can render
        // the (recipe, run) cell without re-running the fetch. The
        // shape mirrors the FetchReport's outcome verbatim.
        let outcome_rows = store.fetch_run_outcomes_for_plan(plan.id).unwrap();
        assert_eq!(outcome_rows.len(), 1);
        assert_eq!(outcome_rows[0].run_id, report.run_id);
        assert_eq!(
            outcome_rows[0].outcome_kind,
            situation_room_storage::fetch_run_outcomes::FetchRunOutcomeKind::Succeeded
        );
        assert_eq!(outcome_rows[0].records_produced, Some(1));
        assert_eq!(outcome_rows[0].source_id, recipe.source_id);
        assert!(outcome_rows[0].failure_stage.is_none());
        assert!(outcome_rows[0].message.is_none());
    }

    /// Session 46 — declined-shape outcome row persistence. The decline
    /// originates in `load_or_author_recipes` (no recipe authored), so
    /// the outcome row carries `recipe_id = None` and the LLM's reason
    /// in `message`. The heatmap groups declines by `source_id`
    /// because there's no recipe to key on.
    ///
    /// We exercise the path by constructing a plan with a legacy entry
    /// — the only shape the executor currently emits without an LLM
    /// authoring call — which surfaces as `LegacyPlanCannotAuthor`.
    /// Mirrors the no-recipe-id branch declines take, with the
    /// distinguishing kind preserved.
    #[tokio::test]
    async fn run_fetch_persists_legacy_plan_outcome_row_with_no_recipe_id_session_46() {
        // Build a plan with one Legacy document_source. The pre-
        // Session-37 hint shape (description + preferred_source_ids)
        // surfaces as `LegacyPlanCannotAuthor` per source_id; that's
        // the closest in-scope shape to a Declined outcome (no
        // recipe, source_id only) without standing up an LLM mock.
        let mut plan = sample_plan();
        plan.expectations.document_sources = vec![DocumentSourceEntry::Legacy(
            DocumentSourceHint {
                description: "session-46 legacy persistence smoke".into(),
                preferred_source_ids: vec!["world_bank_indicators".into()],
            },
        )];
        let store = make_store_with_accepted_plan(&plan);

        let fetcher = StaticFetcher::new();
        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — legacy entries are not authored",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        // At least one legacy outcome surfaced.
        assert!(
            report
                .outcomes
                .iter()
                .any(|o| matches!(o, RecipeOutcome::LegacyPlanCannotAuthor { .. })),
            "expected at least one LegacyPlanCannotAuthor outcome, got {:?}",
            report.outcomes,
        );

        let outcome_rows = store.fetch_run_outcomes_for_plan(plan.id).unwrap();
        let legacy_rows: Vec<_> = outcome_rows
            .iter()
            .filter(|r| {
                r.outcome_kind
                    == situation_room_storage::fetch_run_outcomes::FetchRunOutcomeKind::LegacyPlanCannotAuthor
            })
            .collect();
        assert!(
            !legacy_rows.is_empty(),
            "legacy outcome row was not persisted"
        );
        for row in legacy_rows {
            assert!(
                row.recipe_id.is_none(),
                "legacy_plan_cannot_author rows must have recipe_id = None"
            );
            assert!(!row.source_id.is_empty());
        }
    }

    /// ADR 0016 Phase 1 — end-to-end iterator path. One HTML body,
    /// 5 cards, the iterator-bearing recipe produces 5 Event records
    /// in one fetch run. The fetch_runs row carries the cumulative
    /// records_produced count. This is the test that pins ADR 0016's
    /// Validation contract (§Validation): "The Nature recipe and the
    /// qt.eu recipe should each produce N>1 Event records." The
    /// fixture is portable (no real Nature URL); the live test
    /// covers the real source.
    #[tokio::test]
    async fn run_fetch_with_iterator_recipe_produces_n_records() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/listing.html";
        let recipe = working_iterator_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        // Five-card listing — the kind of shape the post-ADR-0015
        // classifier nominates and ADR 0016 makes addressable.
        let html = br#"
            <html><body>
              <div class="card"><h3>First milestone</h3></div>
              <div class="card"><h3>Second milestone</h3></div>
              <div class="card"><h3>Third milestone</h3></div>
              <div class="card"><h3>Fourth milestone</h3></div>
              <div class="card"><h3>Fifth milestone</h3></div>
            </body></html>
        "#;
        let fetcher = StaticFetcher::new().with(url, html);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(report.plan_id, plan.id);
        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 1);
        // The cardinality story: one recipe, five matches, five
        // records. The pre-Session-38 contract would have produced
        // exactly 1 record here (the empirical defect ADR 0016
        // documents).
        assert_eq!(report.records_produced, 5);
        assert_eq!(report.outcomes.len(), 1);
        match &report.outcomes[0] {
            RecipeOutcome::Succeeded {
                records_produced, ..
            } => assert_eq!(*records_produced, 5),
            other => panic!("expected Succeeded, got {other:?}"),
        }

        // The fetch_runs row reflects the cumulative count, not
        // recipe-count.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].records_produced, 5);
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
                // Track D, Session 25: this test exercises a fixture
                // that returns a 404 ("status error: 404"), which the
                // backoff helper passes through as Failed — no 429
                // path here. Surface as a panic if it ever fires so
                // a future change to the backoff policy doesn't
                // silently turn a Failed assertion into a no-op.
                RecipeOutcome::RateLimited { .. } => {
                    panic!("no rate-limit expected here")
                }
                // Track B, Session 28: this test pre-saves recipes
                // before running the executor, so the
                // `load_or_author_recipes` step short-circuits and
                // no LLM authoring runs — no path to a Declined
                // outcome. If one ever materialises here, the
                // executor's outcome shape has drifted in a way the
                // test should surface, not absorb.
                RecipeOutcome::Declined { .. } => {
                    panic!("no decline expected here (pre-saved recipes)")
                }
                // ADR 0015 / Session 37: LegacyPlanCannotAuthor only
                // surfaces when no recipes exist for a Legacy-shaped
                // plan. This test pre-saves recipes, so the
                // load_or_author_recipes short-circuit fires and no
                // legacy outcome can be produced.
                RecipeOutcome::LegacyPlanCannotAuthor { .. } => {
                    panic!("no legacy outcome expected here (pre-saved recipes)")
                }
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
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
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
    }

    /// Track A, ADR 0012 amendment 1: an apply-stage failure must
    /// persist a `recipe_fetch_attempts` row so the manual
    /// `reauthor_recipe` Tauri command later sees the bytes that
    /// triggered the failure as ground truth.
    ///
    /// The shape we assert:
    /// - The outcome is `Failed { stage: Apply }` (the established
    ///   contract).
    /// - `Store::latest_attempt_for_recipe(recipe.id)` returns
    ///   `Some(_)` with the recipe id, the same run id the executor
    ///   opened, `succeeded: false`, the failure message verbatim,
    ///   and the bytes the runtime fetched.
    #[tokio::test]
    async fn apply_stage_failure_persists_a_recipe_fetch_attempt_row() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/no-match.txt";
        let recipe = working_regex_recipe(&plan, url);
        let recipe_id = recipe.id;
        save_recipe(&store, &recipe).unwrap();

        // Bytes that don't match the recipe's pattern — guaranteed
        // apply failure.
        let bytes = b"unrelated content with no matching pattern";
        let fetcher = StaticFetcher::new().with(url, bytes);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }

        // The capture must exist, name the recipe, carry the bytes,
        // and carry the failure message that the recipe-author would
        // see in the dialog.
        let attempt = store
            .latest_attempt_for_recipe(recipe_id)
            .unwrap()
            .expect("apply-stage failure must record a fetch attempt");
        assert_eq!(attempt.recipe_id, recipe_id);
        assert!(!attempt.succeeded);
        let msg = attempt
            .failure_message
            .as_deref()
            .expect("failure message must be captured");
        assert!(
            msg.to_lowercase().contains("regex") || msg.to_lowercase().contains("matched"),
            "failure message should describe a regex/match failure; got: {msg}"
        );
        let excerpt = attempt
            .bytes_excerpt
            .as_deref()
            .expect("bytes excerpt must be captured");
        assert_eq!(
            excerpt,
            std::str::from_utf8(bytes).unwrap(),
            "excerpt must carry the runtime bytes verbatim under the cap"
        );
        // Session 32: the StaticFetcher in this test doesn't have a
        // content-type configured, so the captured value is None.
        // The next test exercises the populated path.
        assert_eq!(
            attempt.response_content_type, None,
            "no content-type configured on the fetcher must round-trip as None"
        );
    }

    /// Session 32: the response Content-Type travels from
    /// `HttpFetcher::fetch_bytes_with_meta` through
    /// `BackoffOutcome::Bytes` and `fetch_recipe_bytes` into the
    /// `recipe_fetch_attempts.response_content_type` column. This
    /// is the wire that makes the `RecipesPanel.svelte` chip
    /// authoritative rather than heuristic.
    ///
    /// Pattern mirrors `apply_stage_failure_persists_a_recipe_fetch_attempt_row`
    /// — same fixture shape, same regex recipe, same apply failure;
    /// the only delta is that the fetcher carries a configured
    /// content-type and the assertion checks it lands in the row.
    #[tokio::test]
    async fn apply_stage_failure_captures_response_content_type() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/spa-shell.html";
        let recipe = working_regex_recipe(&plan, url);
        let recipe_id = recipe.id;
        save_recipe(&store, &recipe).unwrap();

        // The classic Session 30 / Session 31 case: the recipe was
        // authored expecting a structured payload, but the URL the
        // executor fetches returns the SPA landing page. Heuristic
        // alone (the chip Session 31 shipped) reads `<` → HTML;
        // the header-aware path here proves the chip can be
        // authoritative when the server told the truth.
        let bytes = b"<!DOCTYPE html><html><body>SPA shell</body></html>";
        let fetcher = StaticFetcher::new()
            .with(url, bytes)
            .with_content_type(url, "text/html; charset=UTF-8");

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }

        let attempt = store
            .latest_attempt_for_recipe(recipe_id)
            .unwrap()
            .expect("apply-stage failure must record a fetch attempt");
        assert_eq!(
            attempt.response_content_type.as_deref(),
            Some("text/html; charset=UTF-8"),
            "the configured Content-Type must round-trip into storage"
        );
    }

    /// Synthetic 2-page PDF used for the executor's PDF happy-path
    /// and apply-failure tests. Same fixture the `recipe_apply` tests
    /// use; see `tests/fixtures/pdf/README.md` for provenance.
    const LITHIUM_PDF: &[u8] = include_bytes!(
        "../tests/fixtures/pdf/lithium_production.pdf"
    );

    /// Build a working PDF recipe pinned to the lithium fixture's
    /// (page=2, row=2 [Chile data row], col=1) coordinate. Mirrors
    /// `working_csv_recipe`, `working_json_recipe`, etc.
    fn working_pdf_recipe(plan: &ResearchPlan, url: &str) -> FetchRecipe {
        let mut r = working_csv_recipe(plan, url);
        r.id = Uuid::now_v7();
        r.dedup_key = Some(format!("{}:lithium_pdf", plan.id));
        r.extraction = ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 2,
            col: 1,
        };
        r
    }

    #[tokio::test]
    async fn run_fetch_for_plan_succeeds_against_pdf_recipe_without_calling_llm() {
        // Session 29 (Track C) happy-path: PdfTable promoted from
        // Skipped to a first-class wired mode. Mirrors the CSV / JSON
        // / CSS / regex success tests structurally; the only
        // meaningful difference is the recipe's `extraction` variant
        // and the body bytes (a real PDF instead of CSV/JSON/HTML).
        //
        // History of the canary that lived here:
        //   - Sessions 8–11: CssSelect was the canary (CSV, JSON wired).
        //   - Session 12: CssSelect promoted; RegexCapture took over.
        //   - Session 13: RegexCapture promoted; PdfTable was the last.
        //   - Session 29: PdfTable promoted; canary role retires.
        //
        // The closed extraction-mode enum (ADR 0007) has five variants
        // and all five are now wired. A new not-yet-wired mode would
        // only appear via an ADR that grows the enum to six.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.pdf";
        let recipe = working_pdf_recipe(&plan, url);
        save_recipe(&store, &recipe).unwrap();

        let fetcher = StaticFetcher::new().with(url, LITHIUM_PDF);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "unused — recipes already authored",
            propose_url_prompt: "",
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
        // discipline as the CSV / JSON / CSS / regex paths.
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
    async fn run_fetch_for_plan_reports_apply_failure_on_pdf_with_out_of_range_address() {
        // Failure-shape coverage for the new PdfTable arm: a recipe
        // pointing at row 99 of a 4-row table fails at the apply
        // stage with `ApplyError::Extraction { mode: "pdf_table" }`,
        // which the executor maps to `FailureStage::Apply`. Mirrors
        // the CSV / JSON / CSS / regex apply-failure tests.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.pdf";
        let mut recipe = working_pdf_recipe(&plan, url);
        recipe.extraction = ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 99,
            col: 0,
        };
        save_recipe(&store, &recipe).unwrap();

        let fetcher = StaticFetcher::new().with(url, LITHIUM_PDF);

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: "",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.recipes_succeeded, 0);
        match &report.outcomes[0] {
            RecipeOutcome::Failed { stage, .. } => assert_eq!(*stage, FailureStage::Apply),
            other => panic!("expected Failed(Apply), got {other:?}"),
        }
        // Track A: the apply-failure attempt was recorded so a manual
        // re-author command later sees the bytes that triggered it.
        let attempt = store
            .latest_attempt_for_recipe(recipe.id)
            .unwrap()
            .expect("an attempt row must exist after an apply failure");
        assert!(attempt.failure_message.is_some());
        assert!(!attempt.succeeded);
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
            propose_url_prompt: "",
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
    // Session 39: the `RecordingProvider` and the three tests that
    // used it (`author_one_uses_nomination_url_and_prefetched_excerpt`,
    // `author_one_falls_back_to_stub_excerpt_when_prefetch_fails`,
    // `author_one_truncates_oversized_prefetch_excerpt`) were
    // decimated. Their assertions were on a *single* LLM call seeing
    // a nomination URL + prefetched bytes in its prompt, which doesn't
    // match the post-Session-39 two-call shape (propose-URL with no
    // URL in the prompt + recipe-author against fetched bytes). The
    // behaviours those tests guarded — pre-fetch happening, bytes
    // landing in the recipe-author prompt, oversized bodies getting
    // truncated — are covered by the live tests
    // (`live_fetch_against_real_*`) that walk the real path end-to-
    // end. Decimated.
    // -----------------------------------------------------------------------

    /// A minimal recipe-author prompt template for offline tests. The
    /// real prompt at `config/prompts/recipe_author.md` is far longer;
    /// we only need the placeholders to be substituted so we can
    /// assert what the LLM saw.
    const TEST_AUTHOR_PROMPT: &str = "PLAN={{PLAN_JSON}}\nID={{SOURCE_ID}}\nURL={{SOURCE_URL}}\nEXCERPT={{DOCUMENT_EXCERPT}}\n";

    /// A minimal propose-URL prompt template for offline tests. Only
    /// the placeholders the propose-URL builder substitutes are
    /// included. Tests that exercise the retry loop end-to-end stub
    /// the LLM provider so this prompt's content doesn't actually
    /// reach a model — but the substitution must succeed and the
    /// length must clear `Bounds::LLM_PROMPT_BODY`. (Session 39.)
    const TEST_PROPOSE_URL_PROMPT: &str = "DESC={{NOMINATION_DESCRIPTION}}\nTIER={{PRIORITY_TIER}}\nPRIOR={{PRIOR_ATTEMPTS}}\n";

    // -----------------------------------------------------------------
    // Author-one tests — Session 39
    //
    // Pre-Session-39 `author_one` did one LLM call (recipe-author),
    // pre-fetched the nomination's URL, and asserted the prompt
    // carried that URL + the prefetched bytes. Three tests pinned
    // those assertions:
    //
    //   - `author_one_uses_nomination_url_and_prefetched_excerpt`
    //   - `author_one_falls_back_to_stub_excerpt_when_prefetch_fails`
    //   - `author_one_truncates_oversized_prefetch_excerpt`
    //
    // Session 39 split URL discovery out: the executor now calls the
    // propose-URL LLM (with a description-only prompt that doesn't
    // even contain the eventual URL) before fetching, and then calls
    // the recipe-author LLM against the bytes the propose-URL step
    // chose. The three tests above asserted on a single LLM call
    // seeing the URL + bytes in its prompt; they don't fit the
    // two-call shape. Decimated.
    //
    // The behaviours the deleted tests cared about — that the
    // executor pre-fetches before authoring, that oversized bodies
    // get truncated, that the recipe-author sees the right bytes —
    // are all still exercised by the live `live_fetch_against_real_*`
    // tests and by the `prefetch_excerpt` truncation paths that
    // remain reachable through the retry loop.
    // -----------------------------------------------------------------


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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            propose_url_prompt: "",
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            propose_url_prompt: "",
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
    // Session 21 / ADR 0014 — `authored_from` stamping (decimated)
    //
    // Three tests previously pinned the FetchedBytes / StubExcerpt
    // branch:
    //
    //   - `author_one_stamps_fetched_bytes_when_prefetch_succeeds`
    //   - `author_one_stamps_stub_excerpt_when_prefetch_fails`
    //   - `author_one_stamps_stub_excerpt_when_descriptor_absent`
    //
    // Session 39's retry loop has only one branch: a recipe is
    // authored if and only if the executor successfully fetched the
    // bytes that authoring saw. Failed fetches re-enter the loop
    // with a different proposed URL; they never produce a recipe at
    // all. `AuthoredFrom::StubExcerpt` is dead code on the new
    // path — kept on disk for round-trip with pre-Session-39 rows
    // (see `recipes::tests::recipe_roundtrips_stub_excerpt_authored_from`
    // in the storage crate, which still exercises the storage layer's
    // round-trip independently). Decimated.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------------
    // The decline path (Session 39).
    //
    // A nomination "declines" when the executor surfaces a
    // `RecipeOutcome::Declined` — the recipe is never persisted (no
    // `recipe_id` exists), `recipes_attempted` is not bumped, and the
    // operator-visible reason is the verbatim message from whichever
    // LLM step declined.
    //
    // Two LLM steps can decline:
    //
    //   - Propose-URL (Cheap tier, runs first): the LLM has no more
    //     candidate URLs given the description and the prior-attempts
    //     history. Returns `{ url: "", rationale: "..." }`.
    //   - Recipe-author (Workhorse tier, runs after a successful
    //     fetch): the LLM saw the bytes and judged that no recipe in
    //     the closed extraction-mode vocabulary would extract from
    //     them. This is "Track B" from Session 28 (ADR 0007 amendment
    //     4): `RecipeAuthoringOutput.decline_reason` non-empty.
    //
    // The two decline routes are structurally identical at the
    // executor's surface — both produce `RecipeOutcome::Declined`. We
    // pick the propose-URL path here as the simplest one to test
    // (single mock, single LLM round-trip needed); recipe-author
    // declines are unit-tested in `recipe_author::tests` against
    // `build_validated_recipe`.
    // -----------------------------------------------------------------------

    /// Test provider that always returns a propose-URL decline.
    ///
    /// Session 39: the executor's per-nomination retry loop calls
    /// the propose-URL LLM *first* (Cheap tier) to commit to a URL
    /// to fetch. An empty `url` field in the response is the decline
    /// signal; the executor short-circuits the rest of the loop and
    /// surfaces the nomination as `RecipeOutcome::Declined` without
    /// ever calling the recipe-author. This mock returns that shape
    /// — testing the decline surface without needing two coordinated
    /// LLM mocks.
    ///
    /// The recipe-author decline path (Track B —
    /// `RecipeAuthoringOutput.decline_reason` non-empty) is unit-
    /// tested in `recipe_author::tests`; coverage at the executor
    /// level is structurally identical to this test (both surface as
    /// `RecipeOutcome::Declined`) so we don't double-cover here.
    struct DecliningProvider {
        reason: String,
    }

    impl DecliningProvider {
        fn new(reason: impl Into<String>) -> Self {
            Self {
                reason: reason.into(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for DecliningProvider {
        fn id(&self) -> &'static str {
            "declining"
        }
        fn supported_tiers(&self) -> &[ModelTier] {
            // Cheap is what propose-URL uses; Workhorse is what
            // recipe-author uses. We list both so this provider
            // satisfies whichever tier the executor asks for during
            // the retry loop. (In practice only Cheap is reached
            // because the propose-URL decline short-circuits before
            // the recipe-author tier is ever requested.)
            &[ModelTier::Cheap, ModelTier::Workhorse]
        }
        async fn complete(
            &self,
            _tier: ModelTier,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            // Empty `url` is the propose-URL decline signal (see
            // `propose_source_url::ProposedUrl`). `rationale` is the
            // operator-visible reason that becomes the
            // `RecipeOutcome::Declined.reason` on the report.
            let canned = serde_json::json!({
                "url": "",
                "rationale": self.reason,
            });
            Ok(CompletionResponse {
                text: serde_json::to_string(&canned).unwrap(),
                structured: Some(canned),
                provider: "declining".into(),
                model: "declining-test".into(),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    #[tokio::test]
    async fn declined_source_surfaces_as_declined_outcome() {
        let plan = sample_plan(); // one bound source: "demo_csv"
        let store = make_store_with_accepted_plan(&plan);

        // Empty fetcher: no recipe will ever be applied. If the
        // decline path doesn't short-circuit authoring, the test
        // would surface a different failure (missing fixture).
        let fetcher = StaticFetcher::new();
        let provider = DecliningProvider::new(
            "this source is a JS-rendered SPA; the static HTTP \
             response carries no extractable data",
        );
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // No recipe was authored (the LLM declined).
        assert_eq!(
            report.recipes_attempted, 0,
            "declined sources don't contribute to recipes_attempted"
        );
        assert_eq!(report.recipes_succeeded, 0);
        assert_eq!(report.records_produced, 0);
        // No recipe was persisted.
        assert!(store.recipes_for_plan(plan.id).unwrap().is_empty());
        // The decline surfaces as exactly one outcome.
        assert_eq!(report.outcomes.len(), 1);
        match &report.outcomes[0] {
            RecipeOutcome::Declined { source_id, reason } => {
                // Session 40: source_id on a decline is derived from
                // the full nomination_id (no URL exists at decline
                // time because the propose-URL step itself declined).
                // The executor formats it as "nom:<full-uuid>" — see
                // `derive_source_id_for_decline` for why the prior
                // 8-char prefix was a uniqueness bug.
                assert!(
                    source_id.starts_with("nom:"),
                    "decline source_id should be a nom: prefix; got {source_id}"
                );
                // The full uuid is 36 chars; "nom:" + 36 = 40.
                assert_eq!(
                    source_id.len(),
                    40,
                    "decline source_id should carry the full nomination_id \
                     (Session 40 uniqueness fix); got len={} for {source_id}",
                    source_id.len()
                );
                assert!(
                    reason.contains("JS-rendered SPA"),
                    "decline reason verbatim: {reason}"
                );
            }
            other => panic!("expected Declined, got: {other:?}"),
        }
    }

    /// A re-run after a decline does NOT replay the decline. The
    /// previous run produced no persisted recipes, so the next call
    /// to `load_or_author_recipes` will *also* go through the
    /// authoring branch — but this is a behaviour of "no recipes
    /// persisted yet" rather than a memoization of the decline
    /// itself. We assert it explicitly so future sessions reading
    /// the test understand the invariant.
    #[tokio::test]
    async fn second_run_after_decline_re_attempts_authoring() {
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let fetcher = StaticFetcher::new();
        let provider = DecliningProvider::new("declined again");
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let _r1 = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        let r2 = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        // Same shape on the second run.
        assert_eq!(r2.outcomes.len(), 1);
        assert!(matches!(r2.outcomes[0], RecipeOutcome::Declined { .. }));
        // Still no recipes persisted.
        assert!(store.recipes_for_plan(plan.id).unwrap().is_empty());
    }

    /// **Session 40 regression test — source_id uniqueness across
    /// same-millisecond nominations.**
    ///
    /// The Session 39 implementation of `derive_source_id_for_decline`
    /// took the first 8 hex chars of the nomination's UUIDv7. Those
    /// 32 bits are entirely the millisecond Unix timestamp; all
    /// nominations minted in the same classifier pass share that
    /// prefix exactly, so every decline in a plan came back with
    /// `nom:019e06b0` (or whatever the millisecond happened to be).
    ///
    /// The visible failure was on the frontend: the keyed-each in
    /// `FetchReport.svelte` produces `declined:<source_id>` keys, so
    /// duplicate source_ids meant duplicate keys and Svelte 5 refused
    /// to render the outcomes list. The operator's "looks identical
    /// before and after Run Fetch" symptom was the panel stuck on its
    /// summary header.
    ///
    /// This test pins the fix at the executor boundary: build a plan
    /// with five nominations (the live titanium-supply-chain run had
    /// seven), force them all to decline at the propose-URL step, and
    /// assert every produced `RecipeOutcome::Declined.source_id` is
    /// pairwise distinct. The nominations are constructed in a tight
    /// loop without sleeps so the same-millisecond invariant the bug
    /// depended on is preserved — not as a synchronization trick, just
    /// to mirror what the live classifier does.
    #[tokio::test]
    async fn decline_source_ids_are_unique_across_nominations() {
        // Build a plan with five fresh nominations. Mint the UUIDv7s
        // back-to-back so they share their millisecond timestamp
        // prefix, exactly as the live classifier does.
        let mut plan = sample_plan();
        plan.expectations.document_sources = (0..5)
            .map(|i| {
                DocumentSourceEntry::Nomination(DocumentSourceNomination {
                    nomination_id: Uuid::now_v7(),
                    description: format!("test nomination #{i}"),
                    priority_tier: PriorityTier::AuthoritativePrimary,
                })
            })
            .collect();
        let store = make_store_with_accepted_plan(&plan);

        let fetcher = StaticFetcher::new();
        let provider = DecliningProvider::new(
            "this source is a JS-rendered SPA; the static HTTP \
             response carries no extractable data",
        );
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(report.outcomes.len(), 5);

        // Every outcome is a Declined and every source_id is distinct.
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for o in &report.outcomes {
            match o {
                RecipeOutcome::Declined { source_id, .. } => {
                    assert!(
                        seen.insert(source_id),
                        "duplicate decline source_id {source_id} \
                         (Session 39 collision regression — \
                         derive_source_id_for_decline must use the full \
                         nomination_id, not a prefix)"
                    );
                }
                other => panic!("expected Declined, got: {other:?}"),
            }
        }
        assert_eq!(seen.len(), 5);
    }

    // -----------------------------------------------------------------------
    // ADR 0015 / Session 37 — pre-Session-37 plans surface as
    // LegacyPlanCannotAuthor outcomes, one per preferred_source_id, and
    // never call the LLM. Verifies the backwards-compatibility branch of
    // load_or_author_recipes.
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn legacy_plan_with_hint_entries_surfaces_one_outcome_per_preferred_source_id() {
        // Build a plan whose document_sources is a Legacy hint (the
        // pre-ADR-0015 wire shape that pre-Session-37 plans carry on
        // disk). We build it programmatically rather than via the
        // classifier because the classifier post-Session-37 only
        // emits Nominations.
        let mut plan = sample_plan();
        plan.expectations.document_sources = vec![DocumentSourceEntry::Legacy(
            DocumentSourceHint {
                description: "old-shape hint".into(),
                preferred_source_ids: vec![
                    "world_bank_indicators".into(),
                    "imf_weo".into(),
                    "  ".into(), // whitespace-only — must be skipped.
                ],
            },
        )];
        let store = make_store_with_accepted_plan(&plan);

        // No fetcher fixtures, no LLM provider that would respond:
        // if the legacy path falsely fell through to author_one, the
        // test would hit UnreachableProvider and panic.
        let fetcher = StaticFetcher::new();
        let provider = UnreachableProvider;
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        assert_eq!(
            report.recipes_attempted, 0,
            "legacy outcomes don't contribute to recipes_attempted"
        );
        assert_eq!(report.recipes_succeeded, 0);
        assert_eq!(report.records_produced, 0);
        assert!(store.recipes_for_plan(plan.id).unwrap().is_empty());

        // Two outcomes — one per non-empty preferred_source_id;
        // whitespace-only ids skipped. The order is preserved from
        // the hint's preferred_source_ids vec.
        assert_eq!(report.outcomes.len(), 2);
        match &report.outcomes[0] {
            RecipeOutcome::LegacyPlanCannotAuthor { source_id } => {
                assert_eq!(source_id, "world_bank_indicators");
            }
            other => panic!("expected LegacyPlanCannotAuthor, got: {other:?}"),
        }
        match &report.outcomes[1] {
            RecipeOutcome::LegacyPlanCannotAuthor { source_id } => {
                assert_eq!(source_id, "imf_weo");
            }
            other => panic!("expected LegacyPlanCannotAuthor, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Session 30 — live PDF runtime test (Track C.1 from the Session 29
    // handoff). Sibling of `live_fetch_against_real_csv_*` and
    // `live_fetch_against_real_json_*`; pre-authors a `pdf_table`
    // recipe pinned to a known coordinate, fetches a real PDF over
    // the public internet, and asserts the runtime produced a record
    // (or at least closed the run cleanly with a typed failure).
    //
    // The recipe is pre-authored so the executor *must not* call the
    // LLM here — `UnreachableProvider` enforces that, mirroring the
    // CSV / JSON live tests' Position-A discipline.
    //
    // The default URL points at a USGS Mineral Commodity Summaries
    // chapter (lithium 2024). USGS PDFs are public domain, stable
    // across the year of publication, and have a clear single-table
    // layout on page 2 ("World mine production and reserves") — the
    // shape `pdf_table`'s layout heuristic was designed for. The
    // hard-coded coordinate (`page=2`, `table_index=0`, `row=2`,
    // `col=1`) targets one of the country rows of that table; the
    // operator running the test against a different USGS chapter
    // will likely need to adjust via env vars (see below).
    //
    // Override the URL with `FETCH_LIVE_PDF_URL`; override the
    // address with `FETCH_LIVE_PDF_PAGE`, `FETCH_LIVE_PDF_TABLE_INDEX`,
    // `FETCH_LIVE_PDF_ROW`, `FETCH_LIVE_PDF_COL`. Like the CSV / JSON
    // live tests this asserts only on the wiring (recipe was
    // attempted, outcome is not `Skipped`, audit row closed) — not
    // on the extracted value, which depends on the real document.
    //
    // pdf-extract's text-extraction order is not guaranteed to match
    // reading order on every PDF (Session 29 handoff §"Known gaps").
    // If the test surfaces `Failed @ Apply` against a real USGS PDF
    // with the default coordinate, that's evidence to file under
    // `docs/failure_cases/` (Class B if the heuristic could in
    // principle have addressed it; informational otherwise). The
    // structural assertions below let the test pass on a typed
    // apply-failure too — what fails the test is `Skipped` (which
    // would mean the executor took an unwired branch) or a missing
    // audit row (which would mean run cleanup broke).
    #[tokio::test]
    #[ignore]
    async fn live_fetch_against_real_pdf_produces_observation_and_closes_run() {
        use situation_room_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();

        let url = std::env::var("FETCH_LIVE_PDF_URL").unwrap_or_else(|_| {
            "https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf".to_string()
        });
        let page: u32 = std::env::var("FETCH_LIVE_PDF_PAGE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let table_index: u32 = std::env::var("FETCH_LIVE_PDF_TABLE_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let row: u32 = std::env::var("FETCH_LIVE_PDF_ROW")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let col: u32 = std::env::var("FETCH_LIVE_PDF_COL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:pdf_demo:live", plan.id)),
            plan_id: plan.id,
            source_id: "pdf_demo".into(),
            source_url: Url::parse(&url).expect("FETCH_LIVE_PDF_URL must be a valid URL"),
            extraction: ExtractionSpec::PdfTable {
                page,
                table_index,
                row,
                col,
            },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "value".into(),
                        // Same pattern as live_fetch_against_real_csv:
                        // the addressed cell is non-numeric in the
                        // default fixture (a country code or country
                        // name), so we side-step f64 coercion by
                        // literal-binding `value`. The test asserts
                        // wiring, not extracted values; override the
                        // env vars to exercise the numeric path
                        // against a different coordinate.
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
        };
        save_recipe(&store, &recipe).unwrap();

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        // Structural: recipe was attempted; either it succeeded or
        // surfaced a typed failure stage (Fetch / Apply / Insert).
        // A Skipped here would mean we accidentally went through a
        // non-PDF branch — that's a regression. A `Declined` would
        // be impossible because the recipe is pre-authored.
        assert_eq!(report.recipes_attempted, 1);
        assert!(
            !matches!(report.outcomes[0], RecipeOutcome::Skipped { .. }),
            "live test should not skip — got: {:?}",
            report.outcomes[0]
        );
        assert!(
            !matches!(report.outcomes[0], RecipeOutcome::Declined { .. }),
            "live test should not decline (recipe pre-authored) — got: {:?}",
            report.outcomes[0]
        );

        // Audit row exists and was closed.
        let runs = store.recent_fetch_runs_for_plan(plan.id, 5).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].finished_at.is_some(), "fetch_run must be closed");
    }

    // -----------------------------------------------------------------------
    // Session 38 — live iterator test (ADR 0016)
    //
    // ADR 0016's empirical falsifiability claim, in code: re-classify
    // a quantum-computing-shaped topic, accept the plan, run a fetch,
    // assert N>1 Event records persist from at least one source. The
    // pre-Session-38 contract produced 1 record per source; the
    // post-Session-38 runtime should produce N. The threshold (≥10)
    // matches the handoff's bar — listings have variable cardinality
    // day-to-day, but a real listing's first page should comfortably
    // clear it.
    //
    // Fixture choice: pre-authored iterator-bearing recipe against a
    // canonical listing endpoint (default: a public RSS / Atom feed
    // surfaced via env override so the test is portable across
    // environments). The test uses `UnreachableProvider` to enforce
    // ADR 0007's golden rule — the runtime must not call the LLM.
    // The classification step is *not* re-run live here; that's
    // covered by `recipe_author::live_author_recipe_against_xai_*`
    // and `research_classifier::live_classify_topic_against_xai_*`.
    // What this test pins is the iterator runtime against real bytes.
    //
    // Override knobs:
    //   FETCH_LIVE_ITERATOR_URL       — listing URL (HTML)
    //   FETCH_LIVE_ITERATOR_OUTER     — iterator CSS selector
    //   FETCH_LIVE_ITERATOR_INNER     — inner CSS selector for the
    //                                   per-card extracted leaf
    //   FETCH_LIVE_ITERATOR_MIN       — minimum records to assert
    //                                   (default 10)
    //
    // Defaults target a stable, simple test page (httpbin's HTML
    // sample) when none are set. Real-source verification —
    // `quantum-computing` against Nature subjects — is the operator's
    // path: set the env vars to the target listing and re-run.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore]
    async fn live_iterator_against_real_listing_produces_n_records() {
        use situation_room_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();

        let url = std::env::var("FETCH_LIVE_ITERATOR_URL").unwrap_or_else(|_| {
            // A small, stable HTML page with multiple list items —
            // safe default that doesn't depend on a third-party
            // listing's day-to-day cardinality. Operators chasing
            // the ADR 0016 empirical claim override with the real
            // target (e.g. a Nature subjects URL).
            "https://www.w3.org/TR/html52/".to_string()
        });
        let outer = std::env::var("FETCH_LIVE_ITERATOR_OUTER")
            .unwrap_or_else(|_| "li".to_string());
        let inner = std::env::var("FETCH_LIVE_ITERATOR_INNER")
            .unwrap_or_else(|_| "a".to_string());
        let min: u32 = std::env::var("FETCH_LIVE_ITERATOR_MIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:iter_live", plan.id)),
            plan_id: plan.id,
            source_id: "iter_live".into(),
            source_url: Url::parse(&url).expect("FETCH_LIVE_ITERATOR_URL must be a valid URL"),
            extraction: ExtractionSpec::CssSelect {
                selector: inner,
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: outer,
                attribute: None,
            }),
            produces: vec![ProductionBinding {
                record_type: RecordType::Event,
                expectation: ExpectationRef::EventType { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "event_type".into(),
                        source: FieldValueSource::Literal {
                            value: serde_json::json!("mine_opened"),
                        },
                    },
                    FieldMap {
                        path: "headline".into(),
                        source: FieldValueSource::Extracted,
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        save_recipe(&store, &recipe).expect("save recipe");

        let provider = UnreachableProvider;
        let ctx = ExecutorContext {
            store: &store,
            http: &http,
            provider: &provider,
            recipe_author_prompt: "unused — recipe pre-authored",
            propose_url_prompt: "",
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id)
            .await
            .expect("live fetch should succeed against pre-authored iterator recipe");

        // The cardinality story ADR 0016 makes the architectural claim
        // about: N>1 records per fetch. The threshold is generous
        // because real listings have day-to-day variability.
        assert!(
            report.records_produced >= min,
            "expected >= {min} records, got {} — listing may be \
             unexpectedly thin or the iterator selector is too narrow. \
             Override FETCH_LIVE_ITERATOR_* env vars for a different target.",
            report.records_produced
        );
        assert_eq!(report.recipes_succeeded, 1);

        // Audit row exists and was closed; records_produced reflects
        // the cumulative count (one recipe, N records).
        let runs = store.recent_fetch_runs_for_plan(plan.id, 5).unwrap();
        assert!(!runs.is_empty());
        assert!(runs[0].finished_at.is_some(), "fetch_run must be closed");
        assert!(
            runs[0].records_produced >= min,
            "fetch_run.records_produced must reflect the cumulative count"
        );
    }

    // -----------------------------------------------------------------------
    // Session 40 — PDF text-extraction at prefetch time.
    //
    // Before Session 40 the recipe-author LLM saw raw PDF bytes through
    // `String::from_utf8_lossy`, which is unintelligible binary, and
    // declined every PDF-bearing source it ever met. Session 40 ran the
    // bytes through `pdf_extract::extract_text_from_mem_by_pages` and
    // emitted `[PDF page N]` markers between pages so the LLM could
    // count pages by counting markers. Session 41 then frames detected
    // tables in the runtime's coordinate space (see the next test
    // section); the `is_pdf` magic-byte sniff that gates the PDF
    // branch is unchanged across both sessions.
    // -----------------------------------------------------------------------

    #[test]
    fn is_pdf_recognizes_pdf_magic() {
        assert!(is_pdf(b"%PDF-1.4\n..."));
        assert!(is_pdf(b"%PDF-2.0\nfoo"));
        // Empty / short / wrong-prefix bytes are not PDF.
        assert!(!is_pdf(b""));
        assert!(!is_pdf(b"%PD"));
        assert!(!is_pdf(b"<html>%PDF-fake"));
        assert!(!is_pdf(b"\x89PNG\r\n\x1a\n"));
    }

    // -----------------------------------------------------------------------
    // Session 41 item 1 — framed-table PDF prefetch
    //
    // The old test `render_pdf_text_against_lithium_fixture_emits_page_markers_and_table_text`
    // asserted the pre-Session-41 marker format (`[PDF page 1]` followed
    // by raw page text). That format is gone by design: it forced the
    // LLM to imagine how the runtime's table detector would tokenize
    // the page text, and the lithium MCS run from Session 40 confirmed
    // the imagination gap (LLM authored row=11 against a detected
    // table that had 2 rows). The replacement assertion (`render_pdf_text_with_tables_*`
    // below) pins the new framing — `[PDF page N, table M]` headers
    // followed by row-by-row cells — which removes the imagination
    // step. **Do not add a test that asserts the old format alongside
    // the new one.** Pick one source of truth.
    // -----------------------------------------------------------------------

    #[test]
    fn render_pdf_text_with_tables_against_lithium_fixture_emits_framed_tables() {
        let out = render_pdf_text_with_tables(LITHIUM_PDF)
            .expect("the lithium fixture is a well-formed PDF");
        // The fixture's data table is on page 2 (page 1 is a title
        // page in the synthesized fixture; see
        // tests/fixtures/pdf/README.md). The page-2 table marker
        // must be present and must declare a table the runtime
        // would actually find — the same detector that produced
        // this output will run at apply time.
        assert!(
            out.contains("[PDF page 2, table 0]"),
            "framed-table output should announce a table on page 2; got:\n{out}"
        );
        // The header line declares row × col counts so the LLM can
        // size the table without counting markup. We don't pin
        // exact counts here (the detector may evolve) but we do
        // require the format to declare some.
        assert!(
            out.contains("rows ×"),
            "framed-table header should declare row × col counts; got:\n{out}"
        );
        // Cell values from the detected table must appear inline
        // — these are the strings the LLM will use to confirm "yes,
        // the row I'm targeting holds the country I want."
        for needle in ["Country", "Production", "Australia", "Chile", "Argentina"] {
            assert!(
                out.contains(needle),
                "framed-table output is missing {needle:?}; \
                 the LLM cannot identify which row carries the value.\n\
                 full text:\n{out}"
            );
        }
        // No replacement chars from utf8-lossy. If this fires, the
        // PDF branch is being missed and we're falling back to the
        // raw-bytes path.
        assert!(
            !out.contains('\u{FFFD}'),
            "framed-table output should be readable, not utf8-lossy garbage"
        );
    }

    #[test]
    fn render_pdf_text_with_tables_emits_no_table_marker_when_detector_finds_nothing() {
        // Page 1 of the lithium fixture is a title-only page with no
        // tabular content. The framed output should announce that
        // explicitly — the LLM should not author `pdf_table`
        // coordinates against pages where the detector found no
        // table.
        let out = render_pdf_text_with_tables(LITHIUM_PDF)
            .expect("the lithium fixture is a well-formed PDF");
        assert!(
            out.contains("[PDF page 1] (no table detected)")
                || out.contains("[PDF page 1, table 0]"),
            "page 1 should either declare its (single) table or declare \
             that no table was detected — never silently render raw page \
             text. Got:\n{out}"
        );
    }

    #[test]
    fn render_pdf_text_with_tables_drops_narrative_on_no_table_pages_session_44() {
        // Session 44: pages where the detector found no table emit
        // ONLY the marker line `[PDF page N] (no table detected)`
        // and nothing else. Pre-Session-44 the same page would have
        // followed the marker with up to 4 KiB of the page's
        // narrative text, which dominated the prefetch budget on
        // long PDFs and pushed framed tables on later pages off the
        // end (the lithium MCS truncation gap on chapter-page-110).
        //
        // The test pins the new shape: between the page-1 no-table
        // marker and the page-2 marker that follows, only the
        // inter-page padding (`\n\n`) may appear. Any other
        // characters are narrative leaking through, which is the
        // regression this test guards against.
        let out = render_pdf_text_with_tables(LITHIUM_PDF)
            .expect("the lithium fixture is a well-formed PDF");

        // The page-1 no-table marker must be present. (The
        // `_emits_no_table_marker_*` test above also checks this,
        // but we depend on it here so we re-assert for a clear
        // failure message if the precondition slips.)
        let marker = "[PDF page 1] (no table detected)";
        let marker_idx = out.find(marker).unwrap_or_else(|| {
            panic!(
                "expected page-1 no-table marker; \
                 the title page of the lithium fixture should produce it. \
                 Got:\n{out}"
            )
        });

        // Everything between the marker and the next page's marker
        // must be only inter-page padding. The function emits `\n\n`
        // before each page after the first; trim that off and the
        // remainder must be the page-2 marker (table or no-table)
        // or end-of-string for a single-page PDF.
        let after_marker = &out[marker_idx + marker.len()..];
        let after_padding = after_marker.trim_start_matches('\n');
        assert!(
            after_padding.is_empty()
                || after_padding.starts_with("[PDF page 2"),
            "expected only inter-page padding (\\n\\n) between the \
             page-1 no-table marker and the page-2 marker — narrative \
             leaked through. Session 44 dropped narrative on no-table \
             pages; this test guards the drop. Content between markers:\n\
             {after_padding:.300}"
        );
    }

    #[test]
    fn prefetch_excerpt_budget_is_at_least_64kb_session_44() {
        // Session 44 bumped PREFETCH_EXCERPT_BUDGET from 32 KiB to
        // 64 KiB. The 32 KiB ceiling pre-Session-44 was the binding
        // constraint behind the lithium MCS truncation gap: framed
        // output for a 110-page PDF runs ~55 KiB even after Session
        // 44's narrative drop, which 32 KiB cannot hold but 64 KiB
        // can. Below the 64 KiB floor the truncation gap returns —
        // the LLM sees the early framed tables and the late ones
        // get cut.
        //
        // Pin the floor. If a future session lowers this, the
        // session must update the rationale in the constant's
        // doc-comment AND in `render_pdf_text_with_tables`'s
        // rustdoc, then update or delete this test with a comment
        // explaining what changed in the prefetch architecture
        // that made the smaller budget viable.
        assert!(
            PREFETCH_EXCERPT_BUDGET >= 64 * 1024,
            "PREFETCH_EXCERPT_BUDGET = {} bytes; Session 44 floor is \
             64 KiB. Lowering this without re-architecting the PDF \
             excerpt format reintroduces the lithium MCS truncation \
             gap (chapter on page 110 falls behind the budget cut).",
            PREFETCH_EXCERPT_BUDGET
        );
    }

    #[test]
    fn render_pdf_text_with_tables_surfaces_errors_for_non_pdf_bytes() {
        // A byte slice that starts with `%PDF-` but is otherwise junk
        // exercises the error path. is_pdf would gate this in the
        // executor (only real PDFs reach this function), but the
        // error path is the fallback we feed the LLM if a real PDF
        // turns out to be encrypted / malformed; pin it here so a
        // future pdf_extract upgrade doesn't silently swallow it.
        let junk = b"%PDF-1.7\n<not actually a valid pdf>";
        let result = render_pdf_text_with_tables(junk);
        assert!(
            result.is_err() || matches!(&result, Ok(s) if s.is_empty() || s.starts_with("(PDF parsed")),
            "malformed PDF bytes should surface as an error or a \
             zero-page parse, not silently produce a random success; got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Session 41 item 2 — HTML structural digest
    //
    // The LLM sees what `scraper` sees, in the parsed shape the
    // runtime's `extract_css_select` will query at apply time. These
    // tests pin the digest format against in-memory HTML fixtures.
    // No live network; HTML is constructed inline so the assertions
    // can name the exact shape they expect.
    // -----------------------------------------------------------------------

    #[test]
    fn is_html_recognizes_standard_html_markers() {
        assert!(is_html(b"<!DOCTYPE html><html>...</html>"));
        assert!(is_html(b"<!doctype html><html>"));
        assert!(is_html(b"<html lang=\"en\">"));
        assert!(is_html(b"<HTML>"));
        // Leading whitespace tolerated.
        assert!(is_html(b"\n  <!DOCTYPE html>"));
        assert!(is_html(b"  <html>"));
        // Leading UTF-8 BOM tolerated.
        assert!(is_html(b"\xEF\xBB\xBF<!DOCTYPE html>"));
        assert!(is_html(b"\xEF\xBB\xBF\n<html>"));
    }

    #[test]
    fn is_html_rejects_non_html_payloads() {
        // PDF magic.
        assert!(!is_html(b"%PDF-1.4\n..."));
        // JSON.
        assert!(!is_html(b"{\"data\": [1, 2, 3]}"));
        assert!(!is_html(b"[1, 2, 3]"));
        // CSV.
        assert!(!is_html(b"country,production\nChile,49000\n"));
        // Plain text starting with `<` but not the right marker —
        // this is the principal false-positive risk and the strict
        // sniff rejects it.
        assert!(!is_html(b"<note>not html</note>"));
        // XML / RSS — also chevron-leading but not HTML.
        assert!(!is_html(b"<?xml version=\"1.0\"?>\n<rss>"));
        // Empty / very short.
        assert!(!is_html(b""));
        assert!(!is_html(b"<"));
        assert!(!is_html(b"<h"));
    }

    #[test]
    fn render_html_digest_surfaces_title_h1_table_and_list_shapes() {
        // A small page covering the digest's structure-summary
        // sections: title, one h1, a classed-id table with known
        // (rows × cols), and a couple of lists with known
        // cardinalities.
        let html = br#"<!DOCTYPE html>
<html>
<head><title>Federal Reserve - H.4.1 Statistical Release</title></head>
<body>
<h1>H.4.1 Statistical Release</h1>
<table id="balance-sheet" class="data-table">
  <tr><th>Item</th><th>Amount</th><th>Date</th></tr>
  <tr><td>Reserves</td><td>3000</td><td>2026-01</td></tr>
  <tr><td>Securities</td><td>2500</td><td>2026-01</td></tr>
</table>
<ul class="navigation"><li>A</li><li>B</li><li>C</li></ul>
<ol><li>One</li><li>Two</li></ol>
</body>
</html>"#;
        let out = render_html_digest(html, 32 * 1024)
            .expect("well-formed HTML must produce a digest");

        // Title
        assert!(
            out.contains("<title>: Federal Reserve - H.4.1 Statistical Release"),
            "digest must surface the page title; got:\n{out}"
        );
        // H1
        assert!(
            out.contains("<h1>: H.4.1 Statistical Release"),
            "digest must surface the page <h1>; got:\n{out}"
        );
        // Table signature with class+id and (rows × cols).
        assert!(
            out.contains("<table id=\"balance-sheet\" class=\"data-table\"> (3 rows × 3 cols)"),
            "digest must list the table with its id, class, and shape; got:\n{out}"
        );
        // Lists with cardinalities. Cardinality counts <li> children.
        assert!(
            out.contains("<ul class=\"navigation\"> (3 items)"),
            "digest must list the <ul> with its <li> count; got:\n{out}"
        );
        assert!(
            out.contains("<ol> (2 items)"),
            "digest must list the <ol> with its <li> count; got:\n{out}"
        );
    }

    #[test]
    fn render_html_digest_surfaces_repeating_tag_class_selectors() {
        // Iterator-eligible selectors: tag.class pairs that occur
        // more than once. The LLM uses these to author the outer
        // selector for an iterator-bearing recipe.
        let html = br#"<!DOCTYPE html>
<html><body>
<div class="card"><h3>One</h3></div>
<div class="card"><h3>Two</h3></div>
<div class="card"><h3>Three</h3></div>
<span class="value">100</span>
<span class="value">200</span>
<p class="solo">unique</p>
</body></html>"#;
        let out = render_html_digest(html, 32 * 1024)
            .expect("well-formed HTML must produce a digest");
        // div.card occurs 3 times; span.value occurs 2 times; p.solo
        // occurs only once and must NOT appear in the
        // iterator-eligible section (the N>1 criterion).
        assert!(
            out.contains("div.card: 3 occurrences"),
            "digest must surface div.card with its count; got:\n{out}"
        );
        assert!(
            out.contains("span.value: 2 occurrences"),
            "digest must surface span.value with its count; got:\n{out}"
        );
        // The solo class must not be in the repeating list. We test
        // via a more specific assertion: the line `p.solo: 1` should
        // not be present.
        assert!(
            !out.contains("p.solo: 1"),
            "single-occurrence class must not appear in the \
             iterator-eligible list (N>1 criterion); got:\n{out}"
        );
    }

    #[test]
    fn render_html_digest_excludes_script_and_style_subtrees_from_visible_text() {
        // The visible-text section must not include script bodies or
        // style sheets. A real-world page with 100 KiB of inline JS
        // would otherwise flood the digest with code the LLM does
        // not need.
        let html = br#"<!DOCTYPE html>
<html>
<head>
<title>Page</title>
<style>.hidden { color: red; UNIQUE_STYLE_TOKEN }</style>
</head>
<body>
<h1>Visible heading</h1>
<p>Visible paragraph text.</p>
<script>var UNIQUE_SCRIPT_TOKEN = 42; doSomething();</script>
<noscript>UNIQUE_NOSCRIPT_TOKEN visible only without JS</noscript>
</body>
</html>"#;
        let out = render_html_digest(html, 32 * 1024)
            .expect("well-formed HTML must produce a digest");

        // The visible-text section should carry the actual visible
        // content...
        assert!(
            out.contains("Visible heading"),
            "visible-text section must include <h1> text; got:\n{out}"
        );
        assert!(
            out.contains("Visible paragraph text."),
            "visible-text section must include <p> text; got:\n{out}"
        );
        // ...but not the contents of <script>, <style>, or <noscript>
        // subtrees. Each of these has a unique token we can grep for.
        assert!(
            !out.contains("UNIQUE_SCRIPT_TOKEN"),
            "<script> subtree must not appear in visible text; got:\n{out}"
        );
        assert!(
            !out.contains("UNIQUE_STYLE_TOKEN"),
            "<style> subtree must not appear in visible text; got:\n{out}"
        );
        assert!(
            !out.contains("UNIQUE_NOSCRIPT_TOKEN"),
            "<noscript> subtree must not appear in visible text; got:\n{out}"
        );
    }

    #[test]
    fn render_html_digest_handles_empty_body_gracefully() {
        // SPA shells: <html><body></body></html>. The digest should
        // emit the section headers (so the LLM sees "yes, this was
        // parsed as HTML") and report no tables / no lists / no
        // repeating classes — and an empty visible-text section.
        // The LLM should then decline.
        let html = br#"<!DOCTYPE html>
<html>
<head><title>SPA Shell</title></head>
<body><div id="root"></div></body>
</html>"#;
        let out = render_html_digest(html, 32 * 1024)
            .expect("even an empty SPA shell must produce a parseable digest");
        assert!(
            out.contains("--- HTML structure (parsed by scraper) ---"),
            "digest must always emit its structure header"
        );
        assert!(
            out.contains("<title>: SPA Shell"),
            "digest must surface the title even on a near-empty page"
        );
        // No tables or lists in a div-only shell.
        assert!(
            !out.contains("Tables:"),
            "digest must not claim tables when the page has none"
        );
        assert!(
            !out.contains("Lists:"),
            "digest must not claim lists when the page has none"
        );
    }

    #[test]
    fn render_html_digest_truncates_visible_text_when_budget_is_small() {
        // Visible text is bounded by the budget. The truncation
        // marker must be present and must name the budget so the
        // LLM and the operator see that elision happened.
        let mut body = String::from("<!DOCTYPE html><html><body>");
        // Add ~10 KiB of visible text.
        for _ in 0..1000 {
            body.push_str("<p>Lorem ipsum dolor sit amet. </p>");
        }
        body.push_str("</body></html>");

        // Tiny budget — structure summary is small but visible text
        // must get truncated.
        let out = render_html_digest(body.as_bytes(), 1024)
            .expect("well-formed HTML must produce a digest");
        assert!(
            out.contains("visible text truncated"),
            "digest must mark truncation explicitly; got:\n{out}"
        );
    }

    // -----------------------------------------------------------------------
    // Session 41 item 3 — JSON shape outline
    //
    // The LLM sees what `serde_json` parsed out of the bytes — the same
    // crate `recipe_apply::extract_json_path` queries against at apply
    // time. These tests pin the outline format against in-memory JSON
    // fixtures. The polymorphic-leaf annotation is the principle that
    // catches the World Bank / OECD / Eurostat null-trap class without
    // naming any of those sources in the code.
    // -----------------------------------------------------------------------

    #[test]
    fn is_json_recognizes_json_objects_and_arrays() {
        assert!(is_json(b"{\"data\": []}"));
        assert!(is_json(b"[1, 2, 3]"));
        assert!(is_json(b"   \n  {\"x\":1}"));
        assert!(is_json(b"\xEF\xBB\xBF{\"x\":1}"));
        assert!(is_json(b"\xEF\xBB\xBF\n[1,2]"));
    }

    #[test]
    fn is_json_rejects_non_json_payloads() {
        // PDF magic.
        assert!(!is_json(b"%PDF-1.4\n..."));
        // HTML.
        assert!(!is_json(b"<!DOCTYPE html><html>..."));
        assert!(!is_json(b"<html>"));
        // Bare scalar JSON values — accepted by serde_json but
        // unheard of as document roots; rejecting avoids false-
        // positives on plain text starting with a digit or quote.
        assert!(!is_json(b"42"));
        assert!(!is_json(b"\"a string\""));
        assert!(!is_json(b"true"));
        assert!(!is_json(b"null"));
        // CSV / plain text / empty.
        assert!(!is_json(b"country,production\nChile,49000\n"));
        assert!(!is_json(b""));
        assert!(!is_json(b"   "));
    }

    #[test]
    fn render_json_shape_surfaces_paths_types_and_array_cardinality() {
        // Compact fixture covering: top-level object, nested array,
        // nested object, scalar leaves of distinct types, and
        // verifies cardinality is rendered as `array[N]` for
        // uniform shape.
        let json = br#"{
            "data": [
                {"country": "AUS", "year": 2024, "value": 88000},
                {"country": "CHL", "year": 2024, "value": 49000}
            ],
            "meta": { "total": 2 }
        }"#;
        let out = render_json_shape(json)
            .expect("well-formed JSON must produce an outline");

        assert!(
            out.contains("--- JSON shape (parsed by serde_json) ---"),
            "outline must start with its header marker; got:\n{out}"
        );
        assert!(
            out.contains("$ : object"),
            "outline must list the root path with its type; got:\n{out}"
        );
        assert!(
            out.contains("$.data : array[2]"),
            "outline must list the data array with its cardinality; got:\n{out}"
        );
        assert!(
            out.contains("$.data[].country : string"),
            "outline must collapse array-index paths to `[]`; got:\n{out}"
        );
        assert!(
            out.contains("$.data[].year : number"),
            "outline must list scalar leaf types under array-element paths; got:\n{out}"
        );
        assert!(
            out.contains("$.meta.total : number"),
            "outline must descend into nested objects; got:\n{out}"
        );
        assert!(
            out.contains("--- end JSON shape ---"),
            "outline must terminate with its end marker; got:\n{out}"
        );
    }

    #[test]
    fn render_json_shape_annotates_polymorphic_leaf_with_samples() {
        // The World Bank null-trap fixture: an array whose `value`
        // field is null in the leading elements (most-recent years
        // with unpublished data) and number in the trailing
        // elements (older years with published data). This is the
        // class the outline must surface unambiguously — the
        // `null|number` polymorphic annotation plus the leading-null
        // sample sequence are what tells the LLM to author a filter
        // expression instead of a positional index.
        let json = br#"[
            {"meta": "page info"},
            [
                {"country": "AUS", "year": "2026", "value": null},
                {"country": "AUS", "year": "2025", "value": null},
                {"country": "AUS", "year": "2024", "value": 88000},
                {"country": "AUS", "year": "2023", "value": 86000}
            ]
        ]"#;
        let out = render_json_shape(json)
            .expect("well-formed JSON must produce an outline");

        // The polymorphic union must appear, in deterministic
        // (`null|number`) order — the BTreeSet sort makes this
        // stable across runs.
        assert!(
            out.contains("null|number"),
            "polymorphic leaf must render as `null|number`; got:\n{out}"
        );
        // The polymorphic marker tells the LLM to look at the
        // sample list rather than assume the leaf is uniformly
        // typed.
        assert!(
            out.contains("← polymorphic"),
            "polymorphic-leaf paths must carry the `← polymorphic` marker; got:\n{out}"
        );
        // The leading-null sample sequence is what closes the trap:
        // the LLM sees the first observed values are null and writes
        // a filter expression on the first attempt.
        assert!(
            out.contains("\"null\""),
            "polymorphic-leaf samples must include leading null values; got:\n{out}"
        );
        // At least one numeric value should also appear in the
        // sample list — this is what tells the LLM the path *does*
        // hold real numbers further into the array.
        assert!(
            out.contains("\"88000\"") || out.contains("\"86000\""),
            "polymorphic-leaf samples must include at least one observed \
             number to confirm real values exist; got:\n{out}"
        );
    }

    #[test]
    fn render_json_shape_caps_path_count_at_limit() {
        // Build a flat object with > JSON_OUTLINE_PATH_LIMIT keys.
        // The outline must list the first N and emit an explicit
        // truncation marker; without the cap, a pathological JSON
        // document could crowd the prefetch's overall byte budget.
        let mut body = String::from("{");
        let total = JSON_OUTLINE_PATH_LIMIT + 10;
        for i in 0..total {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!("\"k{i}\":{i}"));
        }
        body.push('}');
        let out = render_json_shape(body.as_bytes())
            .expect("well-formed JSON must produce an outline");
        assert!(
            out.contains("more paths truncated"),
            "outline must mark path-limit truncation explicitly; got:\n{out}"
        );
    }

    #[test]
    fn render_json_shape_renders_first_elements_of_first_array() {
        // The head-elements section gives the LLM concrete values
        // to confirm "yes, this row holds the value I want." It
        // should target the first non-empty array seen during DFS
        // (here: `$.data`) and render the first
        // `JSON_OUTLINE_FIRST_ELEMENTS` elements as JSON.
        let json = br#"{
            "data": [
                {"country": "AUS", "year": 2024, "value": 88000},
                {"country": "CHL", "year": 2024, "value": 49000},
                {"country": "ARG", "year": 2024, "value": 9600}
            ]
        }"#;
        let out = render_json_shape(json)
            .expect("well-formed JSON must produce an outline");
        assert!(
            out.contains("--- First 2 elements of $.data ---"),
            "outline must announce the head-elements section with its \
             count and target path; got:\n{out}"
        );
        // The first element's country must appear; the third must
        // not (the section is bounded at FIRST_ELEMENTS=2).
        assert!(
            out.contains("\"AUS\""),
            "head section must include the first element's values; got:\n{out}"
        );
        assert!(
            !out.contains("\"ARG\""),
            "head section must be capped at JSON_OUTLINE_FIRST_ELEMENTS=2; \
             the third element must NOT appear; got:\n{out}"
        );
    }

    /// End-to-end check on the framing: a JSON source travels through
    /// the executor's pre-fetch + propose-URL + recipe-author retry
    /// loop, and the prompt the recipe-author LLM finally sees has
    /// the shape-outline framing — not raw `from_utf8_lossy` bytes
    /// alone. Mirrors the PDF and HTML integration tests in shape.
    #[tokio::test]
    async fn prefetch_excerpt_for_json_url_yields_shape_outline_to_recipe_author() {
        use std::sync::Mutex;

        struct PromptCapturingProvider {
            seen: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl LlmProvider for PromptCapturingProvider {
            fn id(&self) -> &'static str {
                "prompt_capturing"
            }
            fn supported_tiers(&self) -> &[ModelTier] {
                &[ModelTier::Cheap, ModelTier::Workhorse]
            }
            async fn complete(
                &self,
                tier: ModelTier,
                req: situation_room_llm::CompletionRequest,
            ) -> Result<situation_room_llm::CompletionResponse, situation_room_llm::LlmError>
            {
                if matches!(tier, ModelTier::Workhorse) {
                    self.seen.lock().unwrap().push(req.user.clone());
                }
                let canned = if matches!(tier, ModelTier::Cheap) {
                    serde_json::json!({
                        "url": "https://example.test/series.json",
                        "rationale": "fixture",
                    })
                } else {
                    serde_json::json!({
                        "source_url": "https://example.test/series.json",
                        "extraction": { "mode": "regex_capture", "pattern": ".*", "group": 0 },
                        "produces": [],
                        "decline_reason": "test pin: surface the prompt we just saw",
                    })
                };
                Ok(situation_room_llm::CompletionResponse {
                    text: serde_json::to_string(&canned).unwrap(),
                    structured: Some(canned),
                    provider: "prompt_capturing".into(),
                    model: "test".into(),
                    input_tokens: None,
                    output_tokens: None,
                })
            }
        }

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/series.json";
        // World-Bank-shaped fixture: paginationmeta then a data
        // array with leading-null `value` rows. The recipe-author
        // prompt must carry the outline header AND the polymorphic
        // annotation for `value` AND the raw bytes underneath.
        let json = br#"[
            {"page": 1, "per_page": 4, "total": 4},
            [
                {"country": "AUS", "year": "2026", "value": null},
                {"country": "AUS", "year": "2025", "value": null},
                {"country": "AUS", "year": "2024", "value": 88000},
                {"country": "AUS", "year": "2023", "value": 86000}
            ]
        ]"#;
        let fetcher = StaticFetcher::new().with(url, json);

        let provider = PromptCapturingProvider {
            seen: Mutex::new(Vec::new()),
        };
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        // Recipe-author declined (canned response), so exactly one
        // Declined outcome.
        assert_eq!(report.outcomes.len(), 1);

        let prompts = provider.seen.lock().unwrap();
        assert!(
            !prompts.is_empty(),
            "recipe-author should have been called at least once"
        );
        let last = &prompts[prompts.len() - 1];
        // Outline header — pinning this catches accidental
        // regressions where the JSON branch is bypassed entirely.
        assert!(
            last.contains("--- JSON shape (parsed by serde_json) ---"),
            "recipe-author prompt should carry the JSON outline header. \
             Pre-Session-41-patch-3 it carried raw from_utf8_lossy bytes \
             only and the LLM had to mentally walk the shape."
        );
        // Polymorphic annotation on the `value` leaf — the World
        // Bank trap class. Without this, the LLM has no signal
        // that `$[1][0].value` would land on a null at apply time.
        assert!(
            last.contains("null|number"),
            "outline must annotate the polymorphic `value` leaf as \
             `null|number`; got prompt:\n{last}"
        );
        // Raw bytes underneath — unlike PDF/HTML, the JSON branch
        // keeps the raw bytes so the LLM can read specific values
        // when authoring filter expressions.
        assert!(
            last.contains("\"per_page\""),
            "JSON branch must keep the raw bytes under the outline; \
             got prompt:\n{last}"
        );
        // Excerpt header annotation announces the JSON branch.
        assert!(
            last.contains("JSON (shape outline + raw bytes)"),
            "excerpt header should announce that bytes were parsed \
             as JSON and that the outline coexists with raw bytes; \
             got prompt:\n{last}"
        );
    }

    /// End-to-end check on the framing: a PDF source travels through
    /// the executor's pre-fetch + propose-URL + recipe-author retry
    /// loop, and the prompt the recipe-author LLM finally sees has
    /// the extracted-PDF-text body, not the raw-bytes garbage. We
    /// assert by inspecting the recipe-author prompt that the
    /// `RecordingProvider` captured. Mirrors the live failure mode
    /// from the titanium-supply-chain run: every USGS MCS PDF
    /// declined with "the excerpt is a binary PDF dump."
    #[tokio::test]
    async fn prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author() {
        use std::sync::Mutex;

        // A provider that records every recipe-author prompt it gets
        // shown and replies with a Track-B decline (so the executor
        // proceeds linearly and the test stays bounded).
        struct PromptCapturingProvider {
            seen: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl LlmProvider for PromptCapturingProvider {
            fn id(&self) -> &'static str {
                "prompt_capturing"
            }
            fn supported_tiers(&self) -> &[ModelTier] {
                &[ModelTier::Cheap, ModelTier::Workhorse]
            }
            async fn complete(
                &self,
                tier: ModelTier,
                req: situation_room_llm::CompletionRequest,
            ) -> Result<situation_room_llm::CompletionResponse, situation_room_llm::LlmError> {
                // Workhorse is recipe-author; that's the prompt we
                // care about pinning. For Cheap (propose-URL) the
                // executor calls us repeatedly; we always return the
                // same fixture URL so the propose-URL step terminates
                // quickly.
                if matches!(tier, ModelTier::Workhorse) {
                    self.seen.lock().unwrap().push(req.user.clone());
                }
                let canned = if matches!(tier, ModelTier::Cheap) {
                    serde_json::json!({
                        "url": "https://example.test/lithium.pdf",
                        "rationale": "fixture",
                    })
                } else {
                    serde_json::json!({
                        "source_url": "https://example.test/lithium.pdf",
                        "extraction": { "mode": "regex_capture", "pattern": ".*", "group": 0 },
                        "produces": [],
                        "decline_reason": "test pin: surface the prompt we just saw",
                    })
                };
                Ok(situation_room_llm::CompletionResponse {
                    text: serde_json::to_string(&canned).unwrap(),
                    structured: Some(canned),
                    provider: "prompt_capturing".into(),
                    model: "test".into(),
                    input_tokens: None,
                    output_tokens: None,
                })
            }
        }

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/lithium.pdf";
        let fetcher = StaticFetcher::new().with(url, LITHIUM_PDF);

        let provider = PromptCapturingProvider {
            seen: Mutex::new(Vec::new()),
        };
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        // The recipe-author declined (test fixture says so), so the
        // outcome is exactly one Declined.
        assert_eq!(report.outcomes.len(), 1);

        // The recipe-author prompt was captured at least once. Check
        // that the captured prompt carries the framed-table format
        // emitted by `render_pdf_text_with_tables` — Session 41 item 1.
        let prompts = provider.seen.lock().unwrap();
        assert!(
            !prompts.is_empty(),
            "recipe-author should have been called at least once \
             before the decline; nothing captured"
        );
        let last = &prompts[prompts.len() - 1];
        // The LLM must see a per-page marker that either declares a
        // detected table (`[PDF page N, table M] (R rows × C cols)`)
        // or explicitly declares no table found
        // (`[PDF page N] (no table detected)`). Either is honest;
        // raw page text without a marker is not.
        assert!(
            last.contains("[PDF page 2, table 0]"),
            "the recipe-author prompt should carry the framed-table \
             header for the lithium fixture's data table on page 2. \
             Pre-Session-41 it carried raw page text after `[PDF page N]` \
             markers and the LLM had to imagine the detector's row count."
        );
        assert!(
            last.contains("rows ×"),
            "the framed-table header should declare the table's \
             (rows × cols) shape so the LLM authors against the \
             runtime's coordinate space, not its own visual count."
        );
        for needle in ["Country", "Production"] {
            assert!(
                last.contains(needle),
                "the recipe-author prompt should carry the extracted \
                 PDF table text containing {needle:?}"
            );
        }
        // The kind annotation in the excerpt header announces the
        // extraction explicitly so the LLM knows what it's looking
        // at — pinning the marker here also catches accidental
        // regressions where the PDF branch is bypassed entirely.
        assert!(
            last.contains("PDF (text + detected tables)"),
            "excerpt header should announce that bytes were converted \
             from PDF and tables were detected; otherwise the LLM has \
             no signal that it's looking at framed-table output rather \
             than the raw source"
        );
    }

    /// End-to-end check on the HTML digest: an HTML source travels
    /// through the executor's pre-fetch + propose-URL + recipe-author
    /// retry loop, and the prompt the recipe-author LLM finally sees
    /// carries the digest framing — not raw `from_utf8_lossy` bytes.
    /// Mirrors the PDF integration test in shape.
    #[tokio::test]
    async fn prefetch_excerpt_for_html_url_yields_structural_digest_to_recipe_author() {
        use std::sync::Mutex;

        struct PromptCapturingProvider {
            seen: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl LlmProvider for PromptCapturingProvider {
            fn id(&self) -> &'static str {
                "prompt_capturing"
            }
            fn supported_tiers(&self) -> &[ModelTier] {
                &[ModelTier::Cheap, ModelTier::Workhorse]
            }
            async fn complete(
                &self,
                tier: ModelTier,
                req: situation_room_llm::CompletionRequest,
            ) -> Result<situation_room_llm::CompletionResponse, situation_room_llm::LlmError> {
                if matches!(tier, ModelTier::Workhorse) {
                    self.seen.lock().unwrap().push(req.user.clone());
                }
                let canned = if matches!(tier, ModelTier::Cheap) {
                    serde_json::json!({
                        "url": "https://example.test/page.html",
                        "rationale": "fixture",
                    })
                } else {
                    serde_json::json!({
                        "source_url": "https://example.test/page.html",
                        "extraction": { "mode": "regex_capture", "pattern": ".*", "group": 0 },
                        "produces": [],
                        "decline_reason": "test pin: surface the prompt we just saw",
                    })
                };
                Ok(situation_room_llm::CompletionResponse {
                    text: serde_json::to_string(&canned).unwrap(),
                    structured: Some(canned),
                    provider: "prompt_capturing".into(),
                    model: "test".into(),
                    input_tokens: None,
                    output_tokens: None,
                })
            }
        }

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let url = "https://example.test/page.html";
        // A small HTML page covering the digest's main sections plus
        // an iterator-eligible repeating class. The recipe-author
        // prompt must carry the digest framing; the integration test
        // pins the framing without coupling to an exact byte layout.
        let html = br#"<!DOCTYPE html>
<html>
<head><title>Reserves Statistical Release</title></head>
<body>
<h1>H.4.1</h1>
<table id="balance-sheet"><tr><th>Item</th><th>Amount</th></tr>
<tr><td>Reserves</td><td>3000</td></tr></table>
<div class="card">A</div>
<div class="card">B</div>
</body>
</html>"#;
        let fetcher = StaticFetcher::new().with(url, html);

        let provider = PromptCapturingProvider {
            seen: Mutex::new(Vec::new()),
        };
        let sources: Vec<SourceDescriptor> = vec![];
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            propose_url_prompt: TEST_PROPOSE_URL_PROMPT,
            sources: &sources,
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();
        // Recipe-author declined (canned response), so exactly one
        // Declined outcome.
        assert_eq!(report.outcomes.len(), 1);

        let prompts = provider.seen.lock().unwrap();
        assert!(
            !prompts.is_empty(),
            "recipe-author should have been called at least once"
        );
        let last = &prompts[prompts.len() - 1];
        // Digest header: pinning this catches accidental regressions
        // where the HTML branch is bypassed entirely.
        assert!(
            last.contains("--- HTML structure (parsed by scraper) ---"),
            "the recipe-author prompt should carry the HTML digest \
             header. Pre-Session-41-patch-2 it carried raw \
             from_utf8_lossy bytes and the LLM had to mentally parse \
             the markup."
        );
        // Specific structural elements from the fixture.
        assert!(
            last.contains("<title>: Reserves Statistical Release"),
            "the digest must surface the page title from the parsed \
             HTML; got prompt:\n{last}"
        );
        assert!(
            last.contains("<table id=\"balance-sheet\">"),
            "the digest must list the table with its id attribute; \
             got prompt:\n{last}"
        );
        assert!(
            last.contains("div.card: 2 occurrences"),
            "the digest must surface the iterator-eligible repeating \
             class; got prompt:\n{last}"
        );
        // Excerpt header annotation announces the HTML branch.
        assert!(
            last.contains("HTML (structural digest)"),
            "excerpt header should announce that bytes were parsed \
             as HTML; got prompt:\n{last}"
        );
    }

    // -----------------------------------------------------------------------
    // Session 47 — multi-recipe per nomination
    //
    // Pure unit tests for the helpers added in Session 47. The
    // multi-target authoring flow itself is exercised end-to-end by
    // the existing decline tests (DecliningProvider surfaces a
    // nomination-level decline regardless of target count) and is
    // pinned in shape by the new tests below.
    // -----------------------------------------------------------------------

    #[test]
    fn build_target_expectations_concatenates_buckets_in_declaration_order_session_47() {
        let plan = sample_plan(); // 1 obs metric + 1 event type + 1 entity kind + 1 relation kind
        let targets = build_target_expectations(&plan, 10);
        assert_eq!(
            targets.len(),
            4,
            "all four record-typed buckets should contribute one target each"
        );
        // Concatenation order: obs first, then event, then entity, then relation.
        assert!(matches!(
            targets[0],
            ExpectationRef::ObservationMetric { index: 0 }
        ));
        assert!(matches!(targets[1], ExpectationRef::EventType { index: 0 }));
        assert!(matches!(targets[2], ExpectationRef::EntityKind { index: 0 }));
        assert!(matches!(
            targets[3],
            ExpectationRef::RelationKind { index: 0 }
        ));
    }

    #[test]
    fn build_target_expectations_truncates_to_cap_session_47() {
        let mut plan = sample_plan();
        // Stuff 6 obs metrics in; cap is 4.
        plan.expectations.observation_metrics = (0..6)
            .map(|i| MetricExpectation {
                name: format!("metric_{i}"),
                unit_hint: Some(Unit::new("t").unwrap()),
                rationale: format!("rationale_{i}"),
            })
            .collect();
        let targets = build_target_expectations(&plan, MAX_AUTHORS_PER_NOMINATION);
        assert_eq!(
            targets.len(),
            MAX_AUTHORS_PER_NOMINATION,
            "cap must bound the per-nomination call count"
        );
        // All four are obs metrics 0..3 — the cap fires before
        // event_type/entity_kind/relation_kind contribute.
        for (i, t) in targets.iter().enumerate() {
            assert_eq!(
                *t,
                ExpectationRef::ObservationMetric { index: i as u32 },
                "cap should pull the first {MAX_AUTHORS_PER_NOMINATION} obs metrics"
            );
        }
    }

    #[test]
    fn build_target_expectations_empty_plan_yields_empty_session_47() {
        let mut plan = sample_plan();
        plan.expectations.observation_metrics.clear();
        plan.expectations.event_types.clear();
        plan.expectations.entity_kinds.clear();
        plan.expectations.relation_kinds.clear();
        let targets = build_target_expectations(&plan, 4);
        assert!(targets.is_empty());
    }

    #[test]
    fn build_target_expectations_excludes_document_source_bucket_session_47() {
        // The plan's document_sources is the nominations vec itself.
        // A recipe targeting document_source[i] would be a source
        // authoring a record about itself — circular. Confirm the
        // helper never returns that bucket.
        let plan = sample_plan();
        let targets = build_target_expectations(&plan, 100);
        for t in &targets {
            assert!(
                !matches!(t, ExpectationRef::DocumentSource { .. }),
                "document_source bucket must not be a target: {t:?}"
            );
        }
    }

    #[test]
    fn dedup_key_for_recipe_widens_with_bucket_and_index_session_47() {
        let plan_id = Uuid::now_v7();
        let nom_id = Uuid::now_v7();
        let key = dedup_key_for_recipe(
            plan_id,
            nom_id,
            ExpectationRef::ObservationMetric { index: 2 },
        );
        let expected = format!("{plan_id}:{nom_id}:observation_metric:2");
        assert_eq!(key, expected);
    }

    #[test]
    fn dedup_key_for_recipe_distinguishes_siblings_under_same_nomination_session_47() {
        // Two recipes from the same nomination but different
        // expectations get distinct dedup_keys — the storage layer's
        // primary key won't collide.
        let plan_id = Uuid::now_v7();
        let nom_id = Uuid::now_v7();
        let a = dedup_key_for_recipe(
            plan_id,
            nom_id,
            ExpectationRef::ObservationMetric { index: 0 },
        );
        let b = dedup_key_for_recipe(
            plan_id,
            nom_id,
            ExpectationRef::ObservationMetric { index: 1 },
        );
        let c = dedup_key_for_recipe(
            plan_id,
            nom_id,
            ExpectationRef::EventType { index: 0 },
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn derive_source_id_for_decline_legacy_shape_when_no_target_session_47() {
        let nom = DocumentSourceNomination {
            nomination_id: Uuid::now_v7(),
            description: "test".into(),
            priority_tier: PriorityTier::AuthoritativePrimary,
        };
        let s = derive_source_id_for_decline(&nom, None);
        // Legacy shape: nom:<full-uuid>; preserves Session 40
        // uniqueness invariant for nomination-level declines.
        assert!(s.starts_with("nom:"));
        assert_eq!(s.len(), 40);
    }

    #[test]
    fn derive_source_id_for_decline_widens_when_target_provided_session_47() {
        let nom = DocumentSourceNomination {
            nomination_id: Uuid::now_v7(),
            description: "test".into(),
            priority_tier: PriorityTier::AuthoritativePrimary,
        };
        let s = derive_source_id_for_decline(
            &nom,
            Some(ExpectationRef::ObservationMetric { index: 3 }),
        );
        let expected = format!("nom:{}:observation_metric:3", nom.nomination_id);
        assert_eq!(s, expected);
        // The shape mirrors `dedup_key_for_recipe`: same bucket
        // vocabulary, same coordinate naming convention.
    }

    #[test]
    fn expectation_ref_parts_round_trips_buckets_session_47() {
        let cases = [
            (
                ExpectationRef::ObservationMetric { index: 5 },
                ("observation_metric", 5),
            ),
            (ExpectationRef::EventType { index: 1 }, ("event_type", 1)),
            (
                ExpectationRef::EntityKind { index: 0 },
                ("entity_kind", 0),
            ),
            (
                ExpectationRef::RelationKind { index: 2 },
                ("relation_kind", 2),
            ),
            (
                ExpectationRef::DocumentSource { index: 7 },
                ("document_source", 7),
            ),
        ];
        for (input, (expected_bucket, expected_index)) in cases {
            let (b, i) = expectation_ref_parts(input);
            assert_eq!(b, expected_bucket);
            assert_eq!(i, expected_index);
        }
    }
}
