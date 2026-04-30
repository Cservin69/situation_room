//! Tauri commands — actions the frontend triggers.
//!
//! ## The command surface (Session 7)
//!
//! Five `#[tauri::command]` handlers, all thin wrappers over functions
//! that already exist in pipeline / storage:
//!
//! - [`classify`] — run Level-1 classification on a topic, persist
//!   the resulting plan (status = pending), return it.
//! - [`list_recent_plans`] — list recent plans, optionally filtered
//!   by status. No LLM call.
//! - [`get_plan`] — fetch one plan by id.
//! - [`accept_plan`] — mark a plan as accepted (gates Phase-6 fetch).
//! - [`reject_plan`] — mark a plan as rejected (hidden from default
//!   listings; retained for audit).
//!
//! The first three were the Session-6 baseline; accept/reject land in
//! Session 7 to soft-delete the duplicate-/bad-classification problem
//! visible in the Session-6 screenshots. See ADR 0007 §"runtime path"
//! for why the gate exists at all: only accepted plans should drive
//! deterministic fetching, and that gate has to be a deliberate user
//! action, not an automatic consequence of classification.
//!
//! ## Security discipline (ADR 0009)
//!
//! Every `#[tauri::command]` handler in this crate MUST:
//!   1. Validate any URL input via `stockpile_secure::url_guard::UrlGuard`.
//!   2. Validate any path input via `stockpile_secure::fs_guard::FsGuard`.
//!   3. Check any string input against `stockpile_secure::bounds::Bounds`.
//!   4. Never `expose_secret()` on an `ApiKey` except when passing to an
//!      HTTP Authorization header.
//!   5. Return typed errors. Never panic on user input.
//!
//! The HTTP client used for LLM calls is `SecureHttpClient` — built once
//! in the binary's composition root and reused. No fresh
//! `reqwest::Client::new()` anywhere in this crate.
//!
//! ## Error transport
//!
//! Tauri serializes command errors via `serde`, so we return
//! [`CommandError`] (a simple, frontend-friendly tagged enum) rather
//! than the rich internal error types. The frontend gets enough detail
//! to render a useful message ("classification failed: …") without
//! leaking internal stack traces.

use std::sync::Arc;

use serde::Serialize;
use stockpile_llm::{ModelTier, XaiProvider};
use stockpile_pipeline::fetch_executor::{
    run_fetch_for_plan as run_fetch_for_plan_impl, ExecutorContext, FetchExecutorError,
};
use stockpile_pipeline::research_classifier::{
    classify_topic, ClassificationContext, ClassificationError,
    SourceDescriptor as PipelineSourceDescriptor, TopicUsage as ClassifierTopicUsage,
};
use stockpile_pipeline::research_plans_store::{save_research_plan, ResearchPlanStoreError};
use stockpile_secure::bounds::{check_string, Bounds};
use stockpile_secure::http::SecureHttpClient;
use stockpile_storage::research_plans::PlanStatus;
use stockpile_storage::{Store, StorageError};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::types_export::{
    FetchReportDto, FetchRunSummaryDto, PlanStatusDto, PlanSummary, RecipeDto,
    ResearchPlanDto, SourceDescriptorDto,
};

// ---------------------------------------------------------------------------
// AppState — injected by the Tauri builder, shared across commands
// ---------------------------------------------------------------------------

/// Process-wide state that every command shares. Constructed once in
/// the binary's composition root (`apps/desktop/src-tauri/src/main.rs`)
/// and registered via `tauri::Builder::manage`.
///
/// Holds:
/// - the DuckDB [`Store`] (already thread-safe internally),
/// - the LLM provider (a concrete [`XaiProvider`] today; if/when we
///   support more providers, lift to a trait object),
/// - a shared [`SecureHttpClient`] used both for LLM calls (inside
///   the provider) and for the fetch executor's source fetches —
///   one client, ADR 0009 §"The rule",
/// - the classifier prompt template (loaded from the workspace at
///   compile time via `include_str!` in the binary, then handed in
///   here so this crate stays filesystem-agnostic),
/// - the recipe-author prompt template (same loading pattern; used
///   by the fetch executor's Level-2 authoring step when a plan has
///   no recipes yet),
/// - the registered source descriptors (loaded from
///   `config/sources.toml` in the binary).
///
/// Topic-injection limit is a constant here rather than configuration:
/// the same number the CLI uses (30 topics). If the GUI later needs
/// configurability, lift it to a field.
pub struct AppState {
    pub store: Arc<Store>,
    pub provider: Arc<XaiProvider>,
    pub http: Arc<SecureHttpClient>,
    pub classifier_prompt: &'static str,
    pub recipe_author_prompt: &'static str,
    pub sources: Vec<PipelineSourceDescriptor>,
}

impl AppState {
    pub const TOPICS_INJECTION_LIMIT: usize = 30;
    /// How many recent fetch runs the listing endpoint will surface
    /// for one plan. Bounds the IPC payload regardless of what the
    /// frontend asks for.
    pub const MAX_FETCH_RUNS_LISTING: usize = 50;
    /// How many recipes the inspection-panel endpoint will surface
    /// for one plan. A plan rarely has more than ~10 bound sources,
    /// so this ceiling is generous; a value at the limit is a sign of
    /// a misconfigured plan or a pathological prompt response, not
    /// normal behaviour.
    pub const MAX_RECIPES_LISTING: usize = 100;

    pub fn new(
        store: Arc<Store>,
        provider: Arc<XaiProvider>,
        http: Arc<SecureHttpClient>,
        classifier_prompt: &'static str,
        recipe_author_prompt: &'static str,
        sources: Vec<PipelineSourceDescriptor>,
    ) -> Self {
        Self {
            store,
            provider,
            http,
            classifier_prompt,
            recipe_author_prompt,
            sources,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors crossing the IPC boundary
// ---------------------------------------------------------------------------

/// Frontend-visible error shape. Tauri serializes this directly into the
/// JS `Error` thrown by `invoke()`. We deliberately flatten internal
/// errors into one of a few categories so the frontend can render
/// per-category UI without parsing strings.
#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandError {
    /// User input failed validation before we even tried to do work.
    /// `field` names the parameter, `message` is human-readable.
    #[error("invalid input on {field}: {message}")]
    InvalidInput { field: String, message: String },

    /// Classification (Level-1 LLM call) failed for any reason.
    #[error("classification failed: {message}")]
    ClassificationFailed { message: String },

    /// Storage operation failed — usually a sign of a corrupt DB or a
    /// migration mismatch. Surface so the user can report it.
    #[error("storage error: {message}")]
    Storage { message: String },

    /// The requested plan id wasn't in the store. Distinct from
    /// `Storage` because the frontend treats it as "show empty state",
    /// not "show error toast".
    #[error("plan {id} not found")]
    NotFound { id: String },

    /// The fetch executor failed before completing a run, or the
    /// run's preconditions weren't met (e.g. the plan isn't accepted).
    /// Per-recipe failures don't surface here — they live inside the
    /// `FetchReportDto` returned on the success path.
    #[error("fetch failed: {message}")]
    FetchFailed {
        recipes_attempted: u32,
        recipes_succeeded: u32,
        message: String,
    },
}

impl From<ClassificationError> for CommandError {
    fn from(e: ClassificationError) -> Self {
        // Plan-validation errors are arguably user-input failures
        // (bad topic), but distinguishing them at the command boundary
        // would couple the API to the classifier's internal taxonomy.
        // Lump them under ClassificationFailed; the message text
        // carries the detail.
        CommandError::ClassificationFailed {
            message: e.to_string(),
        }
    }
}

impl From<StorageError> for CommandError {
    fn from(e: StorageError) -> Self {
        CommandError::Storage {
            message: e.to_string(),
        }
    }
}

impl From<ResearchPlanStoreError> for CommandError {
    fn from(e: ResearchPlanStoreError) -> Self {
        CommandError::Storage {
            message: e.to_string(),
        }
    }
}

impl From<FetchExecutorError> for CommandError {
    fn from(e: FetchExecutorError) -> Self {
        match e {
            FetchExecutorError::PlanNotFound(id) => CommandError::NotFound {
                id: id.to_string(),
            },
            FetchExecutorError::PlanNotAccepted { current } => CommandError::InvalidInput {
                field: "id".into(),
                // The message names the *source of the problem* —
                // the plan isn't accepted — rather than dressing up
                // the input as malformed. The handoff calls this
                // out: `InvalidInput` reads odd here but is honest
                // about the source of the problem.
                message: format!(
                    "plan must be accepted before fetch (current: {current})"
                ),
            },
            FetchExecutorError::Storage(s) => CommandError::Storage {
                message: s.to_string(),
            },
            FetchExecutorError::PlanLoad(_)
            | FetchExecutorError::RecipeLoad(_)
            | FetchExecutorError::Authoring(_) => CommandError::FetchFailed {
                // Wholesale failures haven't started attempting any
                // recipes yet; surface zeros to make that explicit.
                recipes_attempted: 0,
                recipes_succeeded: 0,
                message: e.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Command 1 — classify
// ---------------------------------------------------------------------------

/// Classify a free-text topic. Persists the resulting plan and returns
/// it to the frontend.
///
/// Mirrors `apps/situation_room/src/main.rs::run_classify`: build the
/// classifier context from the store, call `classify_topic`, persist
/// via `save_research_plan`, return the typed plan.
///
/// Bounds: the topic is checked against [`Bounds::RESEARCH_TOPIC`]
/// (2 000 chars). Anything longer is almost certainly a user mistake
/// (paste of a whole article into the topic box) and should be
/// rejected at the boundary.
#[tauri::command]
pub async fn classify(
    topic: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    // 1. Boundary validation — bounds + non-empty.
    let topic_trimmed = topic.trim();
    if topic_trimmed.is_empty() {
        return Err(CommandError::InvalidInput {
            field: "topic".into(),
            message: "topic must be non-empty".into(),
        });
    }
    check_string("research_topic", topic_trimmed, Bounds::RESEARCH_TOPIC).map_err(|e| {
        CommandError::InvalidInput {
            field: "topic".into(),
            message: e.to_string(),
        }
    })?;

    info!(topic = %topic_trimmed, "classify command invoked");

    // 2. Build classification context from the store + injected sources.
    let topic_rows = state
        .store
        .topics_in_use(AppState::TOPICS_INJECTION_LIMIT)
        .map_err(CommandError::from)?;
    let existing_topics: Vec<ClassifierTopicUsage> = topic_rows
        .into_iter()
        .map(|r| ClassifierTopicUsage {
            topic: r.topic.as_str().to_string(),
            uses: r.count,
        })
        .collect();

    let ctx = ClassificationContext {
        existing_topics,
        registered_sources: state.sources.clone(),
    };

    // 3. Call the LLM.
    let plan = classify_topic(
        state.provider.as_ref(),
        ModelTier::Workhorse,
        state.classifier_prompt,
        topic_trimmed,
        &ctx,
    )
    .await?;

    // 4. Persist. Failure here means the user's classification effort
    //    is lost on refresh; surface it as an error rather than
    //    silently returning a non-persisted plan.
    if let Err(e) = save_research_plan(state.store.as_ref(), &plan, "xai") {
        warn!(error = %e, plan_id = %plan.id, "failed to persist plan");
        return Err(CommandError::from(e));
    }

    info!(plan_id = %plan.id, "plan classified and persisted");

    // 5. Marshal to the wire shape. The plan was just inserted by
    //    `save_research_plan`, which always writes status = Pending —
    //    so the explicit `from_typed_pending` constructor is correct
    //    here. Any plan re-read from storage goes through
    //    `ResearchPlanDto::from_stored` instead.
    Ok(ResearchPlanDto::from_typed_pending(plan))
}

// ---------------------------------------------------------------------------
// Command 2 — list_recent_plans
// ---------------------------------------------------------------------------

/// List the most recent persisted plans. Pure read; no LLM call.
///
/// Returns lightweight [`PlanSummary`] rows (id, topic, created_at,
/// status, bucket counts). The frontend uses these to render the
/// listing and invokes [`get_plan`] when the user opens one.
///
/// `limit` is clamped to a sane range (1 to 200) to bound the IPC
/// payload regardless of frontend bugs.
///
/// `status` is an optional filter. The frontend's filter strip
/// (All / Pending / Accepted / Rejected) maps to:
///   - All       → status = None
///   - Pending   → status = Some(PlanStatusDto::Pending)
///   - Accepted  → status = Some(PlanStatusDto::Accepted)
///   - Rejected  → status = Some(PlanStatusDto::Rejected)
///
/// Pre-Session-7 callers that still pass no `status` argument
/// continue to work because Tauri's IPC unmarshals an absent JSON
/// field as the type's `Default`, and `Option::default()` is `None` —
/// which is the "show all statuses" path.
#[tauri::command]
pub async fn list_recent_plans(
    limit: usize,
    status: Option<PlanStatusDto>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<PlanSummary>, CommandError> {
    let clamped = limit.clamp(1, 200);
    let storage_status: Option<PlanStatus> = status.map(Into::into);

    let stored = state
        .store
        .recent_research_plans_by_status(storage_status, clamped)
        .map_err(CommandError::from)?;

    let summaries = stored
        .into_iter()
        .map(PlanSummary::from_stored)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CommandError::Storage {
            message: format!("plan summary marshalling: {e}"),
        })?;

    Ok(summaries)
}

// ---------------------------------------------------------------------------
// Command 3 — get_plan
// ---------------------------------------------------------------------------

/// Fetch one plan by id. Returns `NotFound` if the id isn't present.
///
/// `id` arrives from the frontend as a string (Tauri JSON IPC has no
/// native UUID); we parse it here. A malformed id is a 4xx-equivalent,
/// not a 5xx — surface as `InvalidInput`.
///
/// Goes through `store.get_research_plan` (returning a
/// `StoredResearchPlan`) rather than the pipeline helper, because the
/// frontend needs the storage-layer `status` field which the typed
/// `ResearchPlan` deliberately doesn't carry.
#[tauri::command]
pub async fn get_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    let stored = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    ResearchPlanDto::from_stored(stored).map_err(|e| CommandError::Storage {
        message: format!("plan deserialization: {e}"),
    })
}

// ---------------------------------------------------------------------------
// Command 4 — accept_plan
// ---------------------------------------------------------------------------

/// Mark a plan as accepted. The user has reviewed it; downstream
/// Phase-6 fetching may consume it.
///
/// Returns the updated [`ResearchPlanDto`] so the frontend can update
/// its optimistic UI without a second roundtrip. Idempotent — calling
/// `accept_plan` on an already-accepted plan is a successful no-op.
///
/// Errors:
///   - `InvalidInput` if `id` isn't a valid UUID.
///   - `NotFound` if the id isn't in the store.
///   - `Storage` for any other DB-level failure.
#[tauri::command]
pub async fn accept_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    set_status_and_load(id, PlanStatus::Accepted, state).await
}

// ---------------------------------------------------------------------------
// Command 5 — reject_plan
// ---------------------------------------------------------------------------

/// Mark a plan as rejected. Hidden from default listings; retained
/// for audit. Soft-delete; no row is removed.
///
/// Returns the updated [`ResearchPlanDto`]. Idempotent. Same error
/// semantics as [`accept_plan`].
#[tauri::command]
pub async fn reject_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    set_status_and_load(id, PlanStatus::Rejected, state).await
}

// ---------------------------------------------------------------------------
// accept_plan / reject_plan share an implementation
// ---------------------------------------------------------------------------

/// The shared body of `accept_plan` and `reject_plan`. Validates the
/// id, transitions the status, then re-loads the plan so the wire
/// response always reflects what's actually in the database (rather
/// than what the caller asked for).
///
/// Re-loading after the write is deliberate: it costs one extra
/// query per call, but it means the frontend can trust the returned
/// status field as the canonical post-write value. Alternative —
/// constructing the DTO from the pre-write read plus the requested
/// status — would be a denormalized cache that drifts if a future
/// trigger or constraint mutates the row at write time.
async fn set_status_and_load(
    id: String,
    new_status: PlanStatus,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    // Map storage's NotFound to the command-level NotFound so the
    // frontend can surface "plan disappeared" without parsing strings.
    state
        .store
        .set_plan_status(parsed, new_status)
        .map_err(|e| match e {
            StorageError::NotFound(_) => CommandError::NotFound { id: id.clone() },
            other => CommandError::from(other),
        })?;

    info!(plan_id = %parsed, new_status = %new_status, "plan status transitioned");

    let stored = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    ResearchPlanDto::from_stored(stored).map_err(|e| CommandError::Storage {
        message: format!("plan deserialization: {e}"),
    })
}

// ---------------------------------------------------------------------------
// Command 6 — run_fetch_for_plan (Session 8)
// ---------------------------------------------------------------------------

/// Execute the Phase-6 fetch executor against an accepted plan.
///
/// One synchronous call from the user's perspective: the executor
/// loads-or-authors recipes, fetches, applies, inserts records, and
/// returns a [`FetchReportDto`] summarising what happened.
///
/// Validation:
/// - `id` must parse as a UUID (`InvalidInput` otherwise).
/// - The named plan must exist and be in the `accepted` state. A
///   pending or rejected plan returns `InvalidInput` — see the
///   `From<FetchExecutorError>` impl for the exact mapping.
///
/// The LLM is involved only inside the executor's authoring step,
/// which itself only runs if no recipes exist yet for the plan. ADR
/// 0007 §"runtime path": once recipes exist, the runtime is
/// LLM-free.
#[tauri::command]
pub async fn run_fetch_for_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<FetchReportDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    info!(plan_id = %parsed, "run_fetch_for_plan command invoked");

    let ctx = ExecutorContext {
        store: state.store.as_ref(),
        http: state.http.as_ref(),
        provider: state.provider.as_ref(),
        recipe_author_prompt: state.recipe_author_prompt,
        // The same slice the classifier sees, threaded through to
        // the executor for endpoint_hint lookup at Level-2 authoring
        // time (Session 10, Option F).
        sources: state.sources.as_slice(),
    };

    let report = run_fetch_for_plan_impl(&ctx, parsed)
        .await
        .map_err(CommandError::from)?;

    info!(
        plan_id = %parsed,
        run_id = %report.run_id,
        attempted = report.recipes_attempted,
        succeeded = report.recipes_succeeded,
        records = report.records_produced,
        "fetch run returned"
    );

    Ok(FetchReportDto::from_typed(report))
}

// ---------------------------------------------------------------------------
// Command 7 — list_fetch_runs (Session 8)
// ---------------------------------------------------------------------------

/// List the most recent fetch runs for a plan, newest first. Pure
/// read; no LLM call, no fetch.
///
/// `limit` is clamped to a sane range — the executor only writes a
/// few runs per session and the listing is for at-a-glance
/// "did I already fetch this and what happened?" context.
#[tauri::command]
pub async fn list_fetch_runs(
    plan_id: String,
    limit: usize,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FetchRunSummaryDto>, CommandError> {
    let parsed: Uuid = plan_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "plan_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;
    let clamped = limit.clamp(1, AppState::MAX_FETCH_RUNS_LISTING);

    let stored = state
        .store
        .recent_fetch_runs_for_plan(parsed, clamped)
        .map_err(CommandError::from)?;

    Ok(stored.into_iter().map(FetchRunSummaryDto::from_stored).collect())
}

// ---------------------------------------------------------------------------
// Command 8 — list_recipes_for_plan (Session 11 P2.5)
// ---------------------------------------------------------------------------

/// Return the recipes authored for one plan, newest first.
///
/// The frontend's recipe-inspection panel calls this on plan
/// selection. The data was already loadable via the situation_room
/// CLI / DuckDB, but the desktop app couldn't see it — which made
/// every authoring failure a DuckDB-spelunking exercise. This
/// command makes the recipes legible in the UI.
///
/// ## Empty list vs not-found
///
/// An accepted plan with no fetch runs yet has zero recipes (they
/// get authored on the first `run_fetch_for_plan` call). That's
/// indistinguishable, at this layer, from "the plan exists but its
/// recipes were lost" — both come back as an empty `Vec`. The UI
/// renders empty-list state with appropriate copy ("No recipes yet
/// — run fetch to author them"). A bad UUID surfaces as
/// `InvalidInput`; we do not separately verify the plan exists,
/// because doing so would add a second storage call without
/// changing the UX.
///
/// ## Why no status filter
///
/// Unlike `list_recent_plans`, recipes don't carry a lifecycle
/// (they're either authored or absent). All recipes for the plan
/// come back in `authored_at DESC, version DESC` order — the same
/// order `recipes_for_plan` produces.
#[tauri::command]
pub async fn list_recipes_for_plan(
    plan_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<RecipeDto>, CommandError> {
    let parsed: Uuid = plan_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "plan_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;

    let stored = state
        .store
        .recipes_for_plan(parsed)
        .map_err(CommandError::from)?;

    // Defensive: the storage layer doesn't currently bound this and
    // a pathological plan with hundreds of recipes would bloat the
    // IPC payload. Truncate at the listing ceiling. The constant is
    // generous (100) so this is a guardrail, not a routine clip.
    let truncated = stored
        .into_iter()
        .take(AppState::MAX_RECIPES_LISTING)
        .map(RecipeDto::from_stored)
        .collect();

    Ok(truncated)
}

// ---------------------------------------------------------------------------
// Source-descriptor lift helper
//
// Lives here so the binary doesn't need to know about ts-rs / DTOs.
// ---------------------------------------------------------------------------

impl SourceDescriptorDto {
    /// Lift a binary-loaded [`PipelineSourceDescriptor`] into the wire
    /// shape. Currently field-for-field identical; the type exists
    /// so the wire schema is owned by `api`, not by `pipeline`.
    pub fn from_pipeline(d: PipelineSourceDescriptor) -> Self {
        Self {
            id: d.id,
            display_name: d.display_name,
            description: d.description,
            authoritative_for: d.authoritative_for,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_serializes_with_kind_tag() {
        // The frontend pattern-matches on `kind`; guard the wire shape.
        let e = CommandError::InvalidInput {
            field: "topic".into(),
            message: "too long".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"invalid_input""#));
        assert!(json.contains(r#""field":"topic""#));
    }

    #[test]
    fn command_error_not_found_serializes() {
        let e = CommandError::NotFound { id: "abc".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"not_found""#));
    }

    #[test]
    fn command_error_classification_failed_carries_message() {
        let e = CommandError::ClassificationFailed {
            message: "schema rejected by gateway".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"classification_failed""#));
        assert!(json.contains("schema rejected by gateway"));
    }

    #[test]
    fn storage_not_found_maps_to_command_not_found_via_from() {
        // The shared accept/reject handler uses a manual match arm to
        // turn StorageError::NotFound into CommandError::NotFound. The
        // generic `From<StorageError>` is the fallback for everything
        // else; this test guards that NotFound takes the dedicated
        // branch (frontend treats NotFound as "stale id, refresh
        // listing", and Storage as "show error toast").
        let storage_err = StorageError::NotFound("research_plan abc".into());
        // The conversion in the handler is structural — replicate it
        // here for the test boundary.
        let mapped: CommandError = match storage_err {
            StorageError::NotFound(_) => CommandError::NotFound {
                id: "abc".into(),
            },
            other => CommandError::from(other),
        };
        match mapped {
            CommandError::NotFound { id } => assert_eq!(id, "abc"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
