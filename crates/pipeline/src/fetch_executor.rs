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
    fetch_runs::FetchRunRow, research_plans::PlanStatus, Store,
};

use crate::fetch_backoff::{fetch_with_backoff, format_retry_after, BackoffOutcome};
use crate::http_fetcher::{FetchError as HttpFetchError, HttpFetcher};
use crate::recipe_apply::{apply, ApplyContext, ApplyError};
use crate::recipe_author::{author_recipe, AuthoringContext, AuthoringError};
use crate::recipes::{ExtractionSpec, FetchRecipe};
use crate::recipes_store::{
    load_latest_recipes_for_plan, load_recipes_for_plan, save_recipe, RecipeStoreError,
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
    /// Source descriptors for the executor.
    ///
    /// **Doc-narrowed under ADR 0015 (Session 37).** Production
    /// authoring no longer consults this slice — the executor reads
    /// each nomination's `endpoint_url` directly from
    /// `plan.expectations.document_sources` (see `author_one`). The
    /// slice survives only because two surfaces still touch it: the
    /// `#[ignore]` live tests author hand-crafted recipes against the
    /// `csv_demo` / `json_demo` fixtures in `config/sources.toml`,
    /// and `apps_common::sources::load_source_descriptors` still
    /// parses that two-entry file at startup. Pass `&[]` from any new
    /// composition root.
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
            source_url = %nomination.endpoint_url,
            known_id = ?nomination.known_id,
            position,
            total,
            "authoring nomination"
        );
        match author_one(ctx, plan, nomination).await {
            Ok(recipe) => {
                save_recipe(ctx.store, &recipe)?;
                authored.push(recipe);
            }
            Err(FetchExecutorError::Authoring(AuthoringError::Declined { reason })) => {
                // The decline's source_id mirrors the recipe's
                // post-authoring source_id derivation — known_id
                // when present, host otherwise. We can't ask the
                // recipe (it doesn't exist), so derive it locally.
                let source_id =
                    derive_source_id_for_decline(nomination);
                info!(
                    plan_id = %plan.id,
                    source_id = %source_id,
                    position,
                    total,
                    decline_reason = %reason,
                    "recipe author declined for this source; surfacing as RecipeOutcome::Declined"
                );
                declines.push(RecipeOutcome::Declined { source_id, reason });
            }
            Err(e) => {
                warn!(
                    plan_id = %plan.id,
                    source_url = %nomination.endpoint_url,
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
        declined_or_legacy = declines.len(),
        "authoring recipes for plan: complete"
    );

    Ok((authored, declines))
}

/// Source-id derivation for a `Declined` outcome where no recipe
/// exists. Mirrors `author_one`'s post-authoring stamp logic so the
/// operator sees the same id whether the source declined or
/// authored: known_id when present, URL host otherwise.
///
/// Failure to parse the URL falls back to a placeholder id — the
/// nomination already passed `UrlGuard::check` at classify time, so
/// a parse failure here is unexpected; logging and falling back is
/// safer than panicking.
fn derive_source_id_for_decline(nomination: &DocumentSourceNomination) -> String {
    if let Some(known) = nomination.known_id.as_deref() {
        let trimmed = known.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    match url::Url::parse(&nomination.endpoint_url) {
        Ok(u) => u.host_str().unwrap_or("unknown_host").to_string(),
        Err(_) => "unknown_host".to_string(),
    }
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

/// Author one recipe for one (plan, nomination) pair.
///
/// This is the only function in the executor that calls the LLM. It
/// runs at most once per (plan, nomination) pair — see the
/// `load_or_author_recipes` callers — and the result is persisted so
/// subsequent runs of the same plan don't re-author.
///
/// ## What the LLM sees (Session 10, Option F + ADR 0015 / Session 37)
///
/// The author needs three things to do its job well: (a) what the
/// research is about, (b) where the data lives, and (c) what shape
/// it has.
///
/// - **(a) The plan** comes from `plan`.
/// - **(b) The URL** comes from the nomination's `endpoint_url`. ADR
///   0015 retired the descriptor-lookup path: the LLM emitted the URL
///   at classify time after consulting the sources memory, and that
///   URL has already passed `UrlGuard::check`. The executor uses it
///   directly and never synthesizes a placeholder. There is no
///   `https://example.invalid/...` fallback in the post-Session-37
///   path — a missing URL is a classifier-side error that the
///   classifier already would have rejected.
/// - **(c) The excerpt** is the result of pre-fetching `endpoint_url`
///   (UTF-8 lossy, truncated to `PREFETCH_EXCERPT_BUDGET`). When the
///   pre-fetch fails (network error, DNS failure, response too large,
///   server returned an error status), we fall back to a stub excerpt
///   — but we still pass the real `endpoint_url` as the sample, so
///   the LLM at least has a real target to author against.
///
/// ## `source_id` derivation
///
/// After authoring, the executor stamps the recipe's `source_id`:
///
/// 1. If `nomination.known_id` is present *and* the URL host
///    verifies, use `known_id` (the `recipes.source_id` rows match
///    the historical registry-shaped ids — `world_bank_indicators`,
///    `usgs_mcs` — for backwards compatibility with stored history).
/// 2. Otherwise derive from URL host (e.g. `api.worldbank.org`,
///    `apps.fas.usda.gov`). Host-derived ids are first-class — the
///    sources memory query treats both shapes identically.
///
/// "Host verifies" is a lightweight token-overlap heuristic
/// (`host_verifies_known_id`) — exact identity isn't possible because
/// known_id is snake_case (`world_bank_indicators`) and host is
/// dotted (`api.worldbank.org`). When the heuristic fails we log a
/// warning and the URL wins for identity. ADR 0015 §"`known_id` is
/// optional, LLM-side".
///
/// `dedup_key` is `{plan_id}:{source_id}` so subsequent re-runs upsert
/// by version rather than create parallel recipes.
async fn author_one(
    ctx: &ExecutorContext<'_>,
    plan: &ResearchPlan,
    nomination: &DocumentSourceNomination,
) -> Result<FetchRecipe, FetchExecutorError> {
    // Parse the URL. UrlGuard already accepted it at classify time;
    // a parse failure here is unexpected. Surface it through the
    // existing `InvalidRecipe` error path rather than propagating an
    // url::ParseError variant the call chain doesn't carry.
    let sample_url = nomination.endpoint_url.parse::<url::Url>().map_err(|e| {
        FetchExecutorError::Authoring(AuthoringError::InvalidRecipe(format!(
            "nomination endpoint_url failed to parse despite UrlGuard acceptance: {} ({})",
            nomination.endpoint_url, e
        )))
    })?;

    // Derive the source_id we'll stamp on the eventual recipe and use
    // for logs / dedup_key. Computing it here (rather than after
    // authoring) means the operator-visible logs through this
    // authoring step name the same id the recipe carries.
    let effective_source_id = derive_effective_source_id(nomination, &sample_url);

    // Build the document excerpt. Prefer real bytes from the URL
    // the LLM nominated; fall back to a stub describing the
    // nomination.
    //
    // ADR 0014: which branch we took here is the load-bearing
    // signal for `authored_from`. We track it as a boolean alongside
    // the excerpt and stamp the recipe after authoring.
    let (excerpt, used_real_bytes) =
        match prefetch_excerpt(ctx, &sample_url, &effective_source_id).await {
            Some(real) => (real, true),
            None => (
                stub_excerpt(plan, &effective_source_id, Some(sample_url.as_str())),
                false,
            ),
        };

    // Look up any operator feedback the user attached to this
    // (plan, source) pair via the recipe-inspection panel. ADR 0013:
    // the feedback persists across re-authoring (keyed by plan_id +
    // source_id, not recipe_id).
    let recipe_feedback = match ctx
        .store
        .recipe_feedback_for_source(plan.id, &effective_source_id)
    {
        Ok(Some(stored)) => Some(stored.note),
        Ok(None) => None,
        Err(e) => {
            warn!(
                plan_id = %plan.id,
                source_id = %effective_source_id,
                error = %e,
                "recipe_feedback lookup failed; authoring will proceed without operator feedback"
            );
            None
        }
    };

    let auth_ctx = AuthoringContext {
        source_id: effective_source_id.clone(),
        sample_url,
        document_excerpt: excerpt,
        recipe_feedback,
        previous_failure_reason: None,
        operator_guidance: None,
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
    recipe.source_id = effective_source_id.clone();
    recipe.dedup_key = Some(format!("{}:{}", plan.id, effective_source_id));
    // ADR 0014: stamp the authoring provenance signal.
    recipe.authored_from = if used_real_bytes {
        situation_room_storage::AuthoredFrom::FetchedBytes
    } else {
        situation_room_storage::AuthoredFrom::StubExcerpt
    };
    info!(
        plan_id = %plan.id,
        source_id = %effective_source_id,
        recipe_id = %recipe.id,
        authored_from = recipe.authored_from.as_str(),
        "recipe authored; provenance stamped"
    );

    Ok(recipe)
}

/// Pick the `source_id` to stamp on the recipe being authored for
/// this nomination. ADR 0015 §"`known_id` precedence":
///
/// 1. `nomination.known_id` (whitespace-trimmed) when present **and**
///    `host_verifies_known_id` agrees the URL host is consistent.
/// 2. Otherwise the URL's host string.
///
/// On a known_id ↔ host mismatch we log a warning and the URL wins
/// for identity. The verification is "lightweight and does not mask
/// LLM mistakes; it surfaces them." (ADR 0015).
fn derive_effective_source_id(
    nomination: &DocumentSourceNomination,
    sample_url: &url::Url,
) -> String {
    let host = sample_url.host_str().unwrap_or("unknown_host").to_string();
    match nomination.known_id.as_deref() {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                host
            } else if host_verifies_known_id(sample_url, trimmed) {
                trimmed.to_string()
            } else {
                warn!(
                    known_id = %trimmed,
                    host = %host,
                    "nomination known_id did not verify against URL host; URL wins for identity"
                );
                host
            }
        }
        None => host,
    }
}

/// Token-overlap heuristic: does the URL's host contain any 4+
/// character `known_id` token, or vice versa?
///
/// Examples:
/// - `world_bank_indicators` ↔ `api.worldbank.org` → matches on the
///   shared substring "world" (and "bank") → verifies.
/// - `arxiv` ↔ `arxiv.org` → matches on "arxiv" → verifies.
/// - `world_bank_indicators` ↔ `imf.org` → no shared 4+ char token →
///   does not verify.
///
/// The heuristic is intentionally lightweight per ADR 0015. It
/// catches the common case (the LLM's known_id matches the host) and
/// surfaces the obvious mismatches (the LLM stamped a known_id
/// against a wholly unrelated URL). It does not try to be a registry-
/// canonicalisation algorithm — that surface was deliberately retired.
fn host_verifies_known_id(url: &url::Url, known_id: &str) -> bool {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() || known_id.is_empty() {
        return false;
    }

    // Split host on '.' and known_id on '_' for token-by-token
    // comparison. Tokens of length < 4 are too short to be
    // meaningful evidence of identity overlap (host suffixes "org",
    // "com", "gov", "uk" match too easily).
    const MIN_TOKEN_LEN: usize = 4;
    let id_lower = known_id.to_ascii_lowercase();

    let id_tokens: Vec<&str> = id_lower
        .split('_')
        .filter(|t| t.len() >= MIN_TOKEN_LEN)
        .collect();
    let host_tokens: Vec<&str> = host
        .split('.')
        .filter(|t| t.len() >= MIN_TOKEN_LEN)
        .collect();

    // (a) Any sufficiently-long known_id token appears in the host
    //     string verbatim (catches `world` ⊂ `worldbank`, `arxiv` ⊂
    //     `arxiv`).
    for tok in &id_tokens {
        if host.contains(tok) {
            return true;
        }
    }
    // (b) Any sufficiently-long host token appears in the known_id
    //     string verbatim (catches `worldbank` ⊂ `world_bank_…` when
    //     the host had no dots between the words).
    let id_concat = id_lower.replace('_', "");
    for tok in &host_tokens {
        if id_concat.contains(tok) {
            return true;
        }
    }
    false
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
                        description: "Demo CSV".into(),
                        // Aligned with the test-fixture URL convention
                        // (`https://api.example.com/...`) — the executor's
                        // post-ADR-0015 author_one fetches the nomination
                        // URL directly, so the StaticFetcher fixtures must
                        // be keyed off whatever URL this fixture sets.
                        endpoint_url: "https://api.example.com/csv-demo.csv".into(),
                        priority_tier: PriorityTier::AuthoritativePrimary,
                        known_id: Some("demo_csv".into()),
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
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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
            prior_recipe_id: None,
            reauthor_reason: None,
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
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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

    // -----------------------------------------------------------------
    // Author-one tests — ADR 0015 / Session 37 update
    //
    // Pre-ADR-0015, `author_one(ctx, plan, source_id)` resolved the
    // sample URL via a `SourceDescriptor` lookup against
    // `ctx.sources` and synthesized `https://example.invalid/{id}`
    // when no descriptor (or no `endpoint_hint`) was found. ADR 0015
    // retired both code paths: the executor reads the URL directly
    // off `DocumentSourceNomination::endpoint_url`, with no
    // descriptor lookup and no placeholder synthesis.
    //
    // Three tests that exercised the deleted paths were retired and
    // are documented at the bottom of this block. The four below
    // assert what survives:
    //   - the nomination URL appears in the prompt verbatim,
    //   - the prefetched body lands in the prompt,
    //   - prefetch failure preserves the nomination URL but stamps
    //     StubExcerpt,
    //   - oversized prefetch bodies get truncated.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn author_one_uses_nomination_url_and_prefetched_excerpt() {
        // Happy path: the executor pre-fetches the nomination URL,
        // the prompt the LLM sees contains those bytes verbatim and
        // references the URL on the nomination — not a placeholder.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // The nomination URL on the sample plan. The fetcher must
        // serve real bytes here for the FetchedBytes branch to fire.
        let nomination_url = "https://api.example.com/csv-demo.csv";
        // The pre-fetch body and the recipe-execution body don't
        // need to be the same; the assertions only require that
        // the pre-fetch body lands in the prompt. Distinct bodies
        // make the two flows easy to tell apart in failure output.
        let nomination_body = b"country,production\nChile,49000\nAustralia,88000\n";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let recipe_body = b"country,production\nChile,49000\n";

        let fetcher = StaticFetcher::new()
            .with(nomination_url, nomination_body)
            .with(canned_recipe_url, recipe_body);

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            // ADR 0015 retired the descriptor lookup; the slice is
            // unused on the production authoring path. Pass empty so
            // a future test reader doesn't infer it's load-bearing.
            sources: &[],
        };

        let report = run_fetch_for_plan(&ctx, plan.id).await.unwrap();

        assert_eq!(provider.call_count(), 1);
        let prompt = provider.last_prompt();
        assert!(
            prompt.contains(nomination_url),
            "prompt should reference the nomination URL; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("example.invalid"),
            "ADR 0015: example.invalid placeholder synthesis was retired; got:\n{prompt}"
        );
        assert!(
            prompt.contains("Chile,49000"),
            "prompt should contain pre-fetched body; got:\n{prompt}"
        );

        assert_eq!(report.recipes_attempted, 1);
        assert_eq!(report.recipes_succeeded, 1);
        assert_eq!(report.records_produced, 1);
    }

    #[tokio::test]
    async fn author_one_falls_back_to_stub_excerpt_when_prefetch_fails() {
        // ADR 0015 path: no descriptor lookup, no placeholder
        // synthesis. The nomination URL is what the executor
        // attempts to pre-fetch; when that returns no fixture (or
        // any other prefetch failure), the executor falls back to
        // the stub excerpt but **still uses the nomination URL** as
        // the recipe-author's sample URL — the LLM sees a real
        // target it can refine, not a synthetic placeholder.
        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Don't fixture the nomination URL → prefetch returns
        // None → stub-excerpt branch fires. The recipe-execution
        // URL *is* fixtured so the rest of the run completes.
        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

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
        // The sample URL is the nomination URL even though the
        // pre-fetch failed.
        assert!(
            prompt.contains("https://api.example.com/csv-demo.csv"),
            "prompt should still carry the nomination URL on pre-fetch failure; got:\n{prompt}"
        );
        // The stub-excerpt path was taken: it surfaces the URL as
        // the documented endpoint with the failure marker.
        assert!(
            prompt.contains("Documented endpoint")
                || prompt.contains("pre-fetch failed"),
            "prompt should mark pre-fetch failure with the documented-endpoint hint; got:\n{prompt}"
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

        // The nomination URL the sample_plan carries — fixture it
        // with the oversized body so the prefetch runs against the
        // post-ADR-0015 URL the executor actually uses.
        let nomination_url = "https://api.example.com/csv-demo.csv";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let small_csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new()
            .with(nomination_url, body.as_slice())
            .with(canned_recipe_url, small_csv);

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &[],
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

    // -----------------------------------------------------------------
    // Retired tests — ADR 0015 / Session 37
    //
    // The following three tests previously exercised behaviour that
    // ADR 0015 deliberately removed:
    //
    //   - `author_one_falls_back_to_placeholder_when_no_endpoint_hint`
    //   - `author_one_falls_back_when_descriptor_absent`
    //   - `author_one_falls_back_when_endpoint_hint_unparseable`
    //
    // Each one asserted that the prompt contained `example.invalid`,
    // because the pre-Session-37 executor synthesized
    // `https://example.invalid/{source_id}` whenever the descriptor
    // lookup against `ctx.sources` returned no usable URL. Under ADR
    // 0015 that lookup is gone: the executor reads the URL directly
    // off `DocumentSourceNomination::endpoint_url`. There is nothing
    // to fall back to and no placeholder to synthesize. A nomination
    // without a usable URL fails classification's UrlGuard check
    // long before the executor sees the plan, so the executor never
    // encounters the case these tests were guarding.
    //
    // The two `author_one_stamps_stub_excerpt_*` siblings *survive*
    // — they assert the FetchedBytes vs StubExcerpt branch decision
    // through the persisted recipe's `authored_from`, which is
    // independent of how the URL is resolved.
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
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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
            prior_recipe_id: None,
            reauthor_reason: None,
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

        // Both URLs in the fixture: the nomination URL serves real
        // bytes for prefetch, and the recipe-execution URL serves
        // bytes for the run loop. ADR 0015 / Session 37: the
        // executor fetches the nomination URL directly — no
        // descriptor lookup, no `endpoint_hint` resolution.
        let nomination_url = "https://api.example.com/csv-demo.csv";
        let nomination_body = b"country,production\nChile,49000\n";
        let canned_recipe_url = "https://api.example.com/data.csv";
        let recipe_body = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new()
            .with(nomination_url, nomination_body)
            .with(canned_recipe_url, recipe_body);

        let provider = RecordingProvider::new();
        let ctx = ExecutorContext {
            store: &store,
            http: &fetcher,
            provider: &provider,
            recipe_author_prompt: TEST_AUTHOR_PROMPT,
            sources: &[],
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

    /// Stub-excerpt path: when pre-fetch fails (here: the nomination
    /// URL is *not* in the fixture map → NoFixture error), the recipe
    /// lands with `authored_from = StubExcerpt`. ADR 0014 motivating
    /// case (was the GDELT live-run failure mode pre-Session-37; in
    /// the post-ADR-0015 path the same branch fires for any
    /// nomination whose URL doesn't return usable bytes).
    #[tokio::test]
    async fn author_one_stamps_stub_excerpt_when_prefetch_fails() {
        use situation_room_storage::AuthoredFrom;

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        // Don't fixture the nomination URL. The recipe-execution URL
        // *is* fixtured so the run completes (the stub-authored
        // recipe still runs against the canned URL).
        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

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
            "pre-fetch failure must stamp StubExcerpt"
        );
    }

    /// ADR 0015 / Session 37: same outcome as
    /// `author_one_stamps_stub_excerpt_when_prefetch_fails` —
    /// retained as a separate pinning so future readers see that the
    /// "ctx.sources slice is empty" case has no effect on stamping.
    /// Pre-Session-37 this test exercised a distinct code path
    /// (no descriptor → `hint_for_prefetch == None` from the start);
    /// post-Session-37 the descriptor lookup is gone, so empty
    /// `ctx.sources` and any other slice produce identical
    /// behaviour. Kept as a regression guard against accidentally
    /// re-introducing a descriptor-lookup branch.
    #[tokio::test]
    async fn author_one_stamps_stub_excerpt_when_descriptor_absent() {
        use situation_room_storage::AuthoredFrom;

        let plan = sample_plan();
        let store = make_store_with_accepted_plan(&plan);

        let canned_recipe_url = "https://api.example.com/data.csv";
        let csv = b"country,production\nChile,49000\n";
        let fetcher = StaticFetcher::new().with(canned_recipe_url, csv);

        // Empty sources slice — under ADR 0015 this has no effect
        // on author_one. The nomination URL on the plan is what's
        // fetched; not fixtured here, so prefetch fails.
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
            "post-ADR-0015: descriptor absence is not a distinct branch; \
             prefetch failure stamps StubExcerpt regardless of ctx.sources"
        );
    }

    // -----------------------------------------------------------------------
    // Track B (Session 28, ADR 0007 amendment 4): the decline path.
    //
    // When the recipe-author LLM returns a `RecipeAuthoringOutput`
    // with a non-empty `decline_reason`, `build_validated_recipe`
    // surfaces it as `AuthoringError::Declined`, and
    // `load_or_author_recipes` lifts that into a
    // `RecipeOutcome::Declined` carried in the executor's outcomes
    // list. The recipe is NEVER persisted (no `recipe_id` exists),
    // and the run's `recipes_attempted` counter is NOT bumped — the
    // declined source did not contribute a recipe to attempt.
    //
    // The provider below is a `DecliningProvider`: a sibling of
    // `RecordingProvider` that returns a structured output with a
    // populated `decline_reason`. This is the only test scaffold in
    // the executor that exercises the decline channel end-to-end;
    // unit-level coverage of `build_validated_recipe`'s decline
    // checks lives in `recipe_author::tests`.
    // -----------------------------------------------------------------------

    /// Test provider that always returns a declined authoring output.
    /// Used by the executor's decline-path tests below.
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
            &[ModelTier::Workhorse]
        }
        async fn complete(
            &self,
            _tier: ModelTier,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            // The schema requires `source_url`, `extraction`, and
            // `produces` to be present (they're not `Option`); the
            // decline contract says they may be stubbed when
            // `decline_reason` is set. Stub them with values that
            // would fail downstream validation if the decline-check
            // didn't short-circuit — that's how we know the decline
            // path runs first.
            let canned = serde_json::json!({
                "source_url": "https://example.invalid/declined",
                "extraction": {
                    "mode": "csv_cell",
                    "column": "ignored",
                    "row_filter": null
                },
                "produces": [{
                    "record_type": "observation",
                    "expectation": { "list": "observation_metric", "index": 0 },
                    "field_mappings": [
                        { "path": "value", "source": { "kind": "extracted" } }
                    ]
                }],
                "static_payload": "",
                "decline_reason": self.reason,
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
                assert_eq!(source_id, "demo_csv");
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
            }],
            authored_at: Utc::now(),
            authored_by: "live_test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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
}
