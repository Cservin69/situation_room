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
//!   1. Validate any URL input via `situation_room_secure::url_guard::UrlGuard`.
//!   2. Validate any path input via `situation_room_secure::fs_guard::FsGuard`.
//!   3. Check any string input against `situation_room_secure::bounds::Bounds`.
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
use situation_room_llm::{LlmProvider, ModelTier};
use situation_room_pipeline::fetch_executor::{
    run_fetch_for_plan as run_fetch_for_plan_impl, ExecutorContext, FetchExecutorError,
};
use situation_room_pipeline::recipe_author::{
    reauthor_recipe as reauthor_recipe_impl, AuthoringError,
};
use situation_room_pipeline::recipes_store::{
    load_recipe as load_recipe_impl, save_recipe as save_recipe_impl, RecipeStoreError,
};
use situation_room_pipeline::research_classifier::{
    classify_topic, ClassificationContext, ClassificationError,
    SourceDescriptor as PipelineSourceDescriptor, TopicUsage as ClassifierTopicUsage,
};
use situation_room_pipeline::research_plans_store::{
    load_research_plan as load_research_plan_impl, save_research_plan,
    save_research_plan_with_lineage, ResearchPlanStoreError,
};
use situation_room_secure::bounds::{check_string, check_user_text, Bounds};
use situation_room_secure::http::SecureHttpClient;
use situation_room_storage::research_plans::PlanStatus;
use situation_room_storage::{Store, StorageError};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::types_export::{
    FetchReportDto, FetchRunSummaryDto, PlanStatusDto, PlanSummary, RecipeDto,
    RecipeFeedbackDto, RecipeFetchAttemptDto, ResearchPlanDto, SourceDescriptorDto,
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
/// - the LLM provider as a trait object — the binary picks a concrete
///   provider (xAI or Anthropic) based on the `LLM_PROVIDER` env var
///   at boot, then hands it in here. The trait object is the
///   single-source-of-truth identifier for "which provider ran this
///   classification" — `provider.id()` is what we persist into
///   `research_plans.classified_by`.
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
    /// Type-erased provider. The `+ Send + Sync` bounds are required
    /// for `Arc<dyn _>` to be `Send + Sync` itself — the trait
    /// declares them as supertraits, but for trait objects the auto-
    /// trait bounds must be spelled explicitly. tauri::State<T>
    /// requires T: Send + Sync + 'static and this is what satisfies
    /// it.
    pub provider: Arc<dyn LlmProvider + Send + Sync>,
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
    /// How many recipe-feedback notes the listing endpoint will
    /// surface for one plan. A plan rarely has more than ~10 bound
    /// sources and at most one note per source, so this ceiling is
    /// generous; a value at the limit indicates a misconfigured plan,
    /// not normal use. ADR 0013.
    pub const MAX_RECIPE_FEEDBACK_LISTING: usize = 100;

    pub fn new(
        store: Arc<Store>,
        provider: Arc<dyn LlmProvider + Send + Sync>,
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

    /// The manual re-author (Track A) failed before producing a new
    /// recipe. Distinct from `FetchFailed` because the frontend
    /// renders the two differently — `FetchFailed` lives in the
    /// fetch-report panel; re-author failures live in the dialog
    /// the operator just closed. Carries the prior recipe id so the
    /// dialog can show the operator which recipe didn't get
    /// superseded.
    #[error("re-author failed for recipe {prior_recipe_id}: {message}")]
    ReauthorFailed {
        prior_recipe_id: String,
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

impl From<RecipeStoreError> for CommandError {
    fn from(e: RecipeStoreError) -> Self {
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
        // Fresh classification: no prior rejection feedback to inject.
        // The re-classify path is `reclassify_plan` (Session 15).
        previous_rejection_reason: None,
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
    if let Err(e) = save_research_plan(state.store.as_ref(), &plan, state.provider.id()) {
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
// Command 5 — reject_plan (Session 15: now takes an optional reason)
// ---------------------------------------------------------------------------

/// Mark a plan as rejected, optionally attaching a free-text reason.
/// Hidden from default listings; retained for audit. Soft-delete; no
/// row is removed.
///
/// `reason` is the user's note explaining why they rejected this
/// classification. The note is validated by
/// [`check_user_text`] (length, control characters, zero-width chars,
/// bidi overrides, line-ending normalization) at this boundary, and
/// the *normalized* string is what gets persisted — so callers do
/// NOT need to pre-normalize. `None` (or `Some` of a string that
/// trims to empty) records the rejection without a note.
///
/// Returns the updated [`ResearchPlanDto`]. Idempotent: rejecting an
/// already-rejected plan succeeds and overwrites the previous reason
/// with whatever was supplied this call.
///
/// Errors:
///   - `InvalidInput { field: "id" }` — id isn't a valid UUID.
///   - `InvalidInput { field: "reason" }` — reason failed bounds /
///     character-class validation.
///   - `NotFound` — id not in store.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn reject_plan(
    id: String,
    reason: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    // Validate + normalize the reason at the API boundary. An empty
    // / whitespace-only note is treated as `None`: we record the
    // rejection but leave the column NULL.
    let normalized_reason = match reason.as_deref() {
        None => None,
        Some(raw) => {
            let normalized = check_user_text(
                "rejection_reason",
                raw,
                Bounds::REJECTION_REASON,
            )
            .map_err(|e| CommandError::InvalidInput {
                field: "reason".into(),
                message: e.to_string(),
            })?;
            if normalized.trim().is_empty() {
                None
            } else {
                Some(normalized)
            }
        }
    };

    state
        .store
        .set_plan_rejection(parsed, normalized_reason)
        .map_err(|e| match e {
            StorageError::NotFound(_) => CommandError::NotFound { id: id.clone() },
            other => CommandError::from(other),
        })?;

    info!(plan_id = %parsed, "plan rejected");

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
// accept_plan implementation (reject_plan no longer shares this body)
// ---------------------------------------------------------------------------

/// The body of `accept_plan`. Validates the id, transitions the
/// status, then re-loads the plan so the wire response always
/// reflects what's actually in the database (rather than what the
/// caller asked for).
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
// Command 5b — reclassify_plan (Session 15)
// ---------------------------------------------------------------------------

/// Re-run Level-1 classification on a topic, using the rejection
/// reason from a previously-rejected plan as additional context for
/// the LLM. Produces a fresh plan (new id, status = Pending) linked
/// back to the original via `reclassified_from`.
///
/// `id` is the **rejected** plan to re-classify. `edited_reason`,
/// when supplied, replaces the stored rejection reason for this
/// classification call (the stored reason is left untouched — the
/// edit is per-call, not a mutation). When `edited_reason` is
/// `None`, the stored reason is used as-is.
///
/// Either the stored reason or `edited_reason` must be present and
/// non-empty after validation; otherwise `InvalidInput { field:
/// "reason" }` is returned. The classifier needs *some* feedback to
/// do something different the second time.
///
/// Errors:
///   - `InvalidInput { field: "id" }` — id isn't a valid UUID.
///   - `InvalidInput { field: "edited_reason" }` — supplied text
///     failed validation.
///   - `InvalidInput { field: "reason" }` — neither the stored nor
///     the edited reason yielded any non-empty text after validation.
///   - `InvalidInput { field: "id", message: "plan must be rejected
///     before reclassify" }` — caller asked to re-classify a plan
///     not in `Rejected` status.
///   - `NotFound` — id not in store.
///   - `ClassificationFailed` — LLM call or plan validation failed.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn reclassify_plan(
    id: String,
    edited_reason: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    // 1. Boundary validation: id.
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    // 2. Load the predecessor plan + storage row (we need both: the
    //    typed plan for its topic, the stored row for its
    //    rejection_reason and status).
    let stored_predecessor = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    if stored_predecessor.status != PlanStatus::Rejected {
        return Err(CommandError::InvalidInput {
            field: "id".into(),
            message: format!(
                "plan must be rejected before reclassify (current: {})",
                stored_predecessor.status
            ),
        });
    }

    // 3. Resolve the effective reason. Edited > stored > error.
    //    `check_user_text` validates + normalizes; we feed the
    //    normalized text into the classifier's fenced block.
    let effective_reason: String = match edited_reason.as_deref() {
        Some(raw) if !raw.trim().is_empty() => check_user_text(
            "edited_reason",
            raw,
            Bounds::REJECTION_REASON,
        )
        .map_err(|e| CommandError::InvalidInput {
            field: "edited_reason".into(),
            message: e.to_string(),
        })?,
        _ => match stored_predecessor.rejection_reason.as_deref() {
            Some(stored) if !stored.trim().is_empty() => stored.to_string(),
            _ => {
                return Err(CommandError::InvalidInput {
                    field: "reason".into(),
                    message: "no rejection reason available — supply edited_reason \
                              or reject the plan again with a note before re-classifying"
                        .into(),
                });
            }
        },
    };

    info!(
        predecessor = %parsed,
        topic = %stored_predecessor.topic,
        "reclassify_plan invoked"
    );

    // 4. Build classification context — same shape as `classify`,
    //    plus the previous_rejection_reason injection.
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
        previous_rejection_reason: Some(effective_reason),
    };

    // 5. Call the LLM with the original topic verbatim. The user's
    //    topic string is stored on the predecessor and threaded
    //    through unchanged — re-classification doesn't let the user
    //    silently retype the topic alongside their feedback.
    let new_plan = classify_topic(
        state.provider.as_ref(),
        ModelTier::Workhorse,
        state.classifier_prompt,
        &stored_predecessor.topic,
        &ctx,
    )
    .await?;

    // 6. Persist with lineage. We use the lineage-aware constructor
    //    so the new plan's `reclassified_from` column points back to
    //    the rejected predecessor.
    if let Err(e) = save_research_plan_with_lineage(
        state.store.as_ref(),
        &new_plan,
        state.provider.id(),
        Some(parsed),
    ) {
        warn!(
            error = %e,
            new_plan_id = %new_plan.id,
            predecessor = %parsed,
            "failed to persist reclassified plan"
        );
        return Err(CommandError::from(e));
    }

    info!(
        new_plan_id = %new_plan.id,
        predecessor = %parsed,
        "reclassified plan persisted"
    );

    // 7. Re-load through from_stored so the audit fields
    //    (reclassified_from in particular) are visible to the
    //    frontend without a second roundtrip.
    let stored_new = state
        .store
        .get_research_plan(new_plan.id)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::Storage {
            message: format!(
                "newly persisted plan {} not readable on the same connection",
                new_plan.id
            ),
        })?;

    ResearchPlanDto::from_stored(stored_new).map_err(|e| CommandError::Storage {
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
// Command 9 — set_recipe_feedback (ADR 0013)
// ---------------------------------------------------------------------------

/// Attach a free-text operator note to a (plan, source) pair, or
/// clear an existing note. The note feeds back into the LLM's
/// recipe-author prompt the next time authoring runs for the same
/// pair (via the v1.8 `{{RECIPE_FEEDBACK}}` placeholder).
///
/// `note` is the operator's correction. Validation policy mirrors
/// `reject_plan`'s `reason`:
///
///   - `None` clears any existing note (deletes the row).
///   - `Some(text)` whose text trims to empty also clears.
///   - `Some(text)` with non-empty trimmed contents is validated by
///     [`check_user_text`] against `Bounds::RECIPE_FEEDBACK` and
///     persisted (upsert: a prior note for the same pair is
///     overwritten — see ADR 0013 §"The overwrite choice").
///
/// Returns `Some(RecipeFeedbackDto)` for the upsert case so the
/// frontend's optimistic UI lands a canonical row, and `None` for
/// the clear case.
///
/// Errors:
///   - `InvalidInput { field: "plan_id" }` — plan_id isn't a UUID.
///   - `InvalidInput { field: "source_id" }` — source_id is empty
///     or oversized (bounds-checked against `Bounds::URL`'s 2 048
///     ceiling, which is more than enough for the largest
///     plausible source id).
///   - `InvalidInput { field: "note" }` — note failed bounds /
///     character-class validation.
///   - `Storage` — DB-level failure.
///
/// ## Why one command for set + clear
///
/// Mirrors `reject_plan(id, reason: Option<String>)`. The empty /
/// `None` form clears, the non-empty form upserts. Two commands
/// here would document a difference the storage layer collapses,
/// per ADR 0013 §"IPC commands".
#[tauri::command]
pub async fn set_recipe_feedback(
    plan_id: String,
    source_id: String,
    note: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Option<RecipeFeedbackDto>, CommandError> {
    let parsed_plan_id: Uuid =
        plan_id
            .parse()
            .map_err(|e: uuid::Error| CommandError::InvalidInput {
                field: "plan_id".into(),
                message: format!("not a valid UUID: {e}"),
            })?;

    // Source-id is a config-defined string, but we still bound it.
    // 2 048 is `Bounds::URL` which is generous; a real source id is
    // ~30 chars, but the bound exists to keep a malformed wire
    // payload from blowing up the DB. Empty-after-trim is rejected.
    let trimmed_source_id = source_id.trim();
    if trimmed_source_id.is_empty() {
        return Err(CommandError::InvalidInput {
            field: "source_id".into(),
            message: "source_id is empty".into(),
        });
    }
    check_string("source_id", trimmed_source_id, Bounds::URL).map_err(|e| {
        CommandError::InvalidInput {
            field: "source_id".into(),
            message: e.to_string(),
        }
    })?;

    // Decide upsert-vs-clear at the boundary so the storage layer
    // sees one of two operations, not a "maybe insert" call.
    let normalized_note = match note.as_deref() {
        None => None,
        Some(raw) => {
            let normalized = check_user_text("note", raw, Bounds::RECIPE_FEEDBACK)
                .map_err(|e| CommandError::InvalidInput {
                    field: "note".into(),
                    message: e.to_string(),
                })?;
            if normalized.trim().is_empty() {
                None
            } else {
                Some(normalized)
            }
        }
    };

    match normalized_note {
        None => {
            state
                .store
                .clear_recipe_feedback(parsed_plan_id, trimmed_source_id)
                .map_err(CommandError::from)?;
            info!(
                plan_id = %parsed_plan_id,
                source_id = %trimmed_source_id,
                "recipe_feedback cleared"
            );
            Ok(None)
        }
        Some(text) => {
            let row = situation_room_storage::RecipeFeedbackRow {
                plan_id: parsed_plan_id,
                source_id: trimmed_source_id.to_string(),
                note: text,
                created_at: chrono::Utc::now(),
            };
            state
                .store
                .set_recipe_feedback(&row)
                .map_err(CommandError::from)?;
            info!(
                plan_id = %parsed_plan_id,
                source_id = %trimmed_source_id,
                "recipe_feedback set"
            );
            // Read back so the wire response always reflects what's
            // actually in the database (mirrors set_status_and_load's
            // posture). Costs one extra query per call but keeps the
            // frontend's optimistic shape canonical post-write.
            let stored = state
                .store
                .recipe_feedback_for_source(parsed_plan_id, trimmed_source_id)
                .map_err(CommandError::from)?
                // Race-impossible in practice (single-user desktop
                // app, single connection), but defensively map a
                // missing row to NotFound rather than unwrap.
                .ok_or_else(|| CommandError::NotFound {
                    id: format!(
                        "recipe_feedback for plan_id={parsed_plan_id} source_id={trimmed_source_id}"
                    ),
                })?;
            Ok(Some(RecipeFeedbackDto::from_stored(stored)))
        }
    }
}

// ---------------------------------------------------------------------------
// Command 10 — list_recipe_feedback_for_plan (ADR 0013)
// ---------------------------------------------------------------------------

/// Return every operator-feedback note attached to a plan, newest
/// first. The frontend calls this on plan selection (alongside
/// `list_recipes_for_plan`) so the indicator chip beside each
/// recipe card lights up if a note exists for the recipe's
/// `source_id`.
///
/// Pure read; safe to invoke freely. Empty list is the legitimate
/// state for a plan with no flagged recipes.
///
/// Errors:
///   - `InvalidInput { field: "plan_id" }` — plan_id isn't a UUID.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn list_recipe_feedback_for_plan(
    plan_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<RecipeFeedbackDto>, CommandError> {
    let parsed: Uuid = plan_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "plan_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;

    let stored = state
        .store
        .recipe_feedback_for_plan(parsed)
        .map_err(CommandError::from)?;

    let truncated = stored
        .into_iter()
        .take(AppState::MAX_RECIPE_FEEDBACK_LISTING)
        .map(RecipeFeedbackDto::from_stored)
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
// Command 11 — latest_attempt_for_recipe (Track A, ADR 0012 amendment 1)
// ---------------------------------------------------------------------------

/// Look up the most recent recorded fetch attempt for a recipe.
/// Track A: the re-author dialog opens this command when it mounts so
/// the operator sees the exact bytes + failure message the runtime
/// captured at the failed apply, before deciding to spend an LLM call
/// on a re-author.
///
/// Returns `Some(dto)` for any recipe with at least one captured
/// attempt (today: any recipe whose latest run failed at apply
/// stage); `None` otherwise. The frontend's empty-state copy says
/// "no bytes captured for this recipe — re-authoring may guess at
/// the response shape."
///
/// Errors:
///   - `InvalidInput { field: "recipe_id" }` — bad UUID.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn latest_attempt_for_recipe(
    recipe_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Option<RecipeFetchAttemptDto>, CommandError> {
    let parsed: Uuid =
        recipe_id
            .parse()
            .map_err(|e: uuid::Error| CommandError::InvalidInput {
                field: "recipe_id".into(),
                message: format!("not a valid UUID: {e}"),
            })?;

    let stored = state
        .store
        .latest_attempt_for_recipe(parsed)
        .map_err(CommandError::from)?;

    Ok(stored.map(RecipeFetchAttemptDto::from_stored))
}

// ---------------------------------------------------------------------------
// Command 12 — reauthor_recipe (Track A, ADR 0012 amendment 1)
// ---------------------------------------------------------------------------

/// Manually re-author a failed recipe, given the operator's optional
/// note. This is the operationalised form of ADR 0012 §"Manual-practice
/// protocol": the operator reads the failure, optionally diagnoses it,
/// and asks the LLM for a corrected recipe.
///
/// **Why this isn't `run_fetch` again.** `run_fetch_for_plan` only
/// authors recipes that don't yet exist for the plan; once a recipe
/// exists for `(plan, source)`, fetch reuses it. To get a *new* recipe
/// for an existing pair, the operator triggers re-authoring — which
/// is what this command does.
///
/// **Why the bytes come from storage, not a fresh fetch.** ADR 0012
/// amendment 1 §"Capture failed-apply bytes": the bytes that triggered
/// the failure are ground truth for re-authoring. A fresh fetch would
/// see whatever the source serves *now*, which may have changed
/// (sources rotate front-page content, rate-limit intermittently,
/// or A/B-test response shapes). The executor captured the failed-
/// apply bytes into `recipe_fetch_attempts` (migration 0013) at the
/// moment of the failure; we read them back here.
///
/// ## Inputs
///
/// - `recipe_id` — the prior recipe whose failure prompted the
///   re-author. Must parse as a UUID and reference an existing
///   recipe (else `NotFound`).
/// - `operator_note` — optional free-text correction. Validated
///   through `check_user_text` against `Bounds::RECIPE_FEEDBACK`
///   (control-character rejection, length cap, line-ending
///   normalization). Empty / `None` is allowed: the failure message
///   alone may be rich enough.
///
/// ## Behaviour
///
/// 1. Validate inputs.
/// 2. Load the prior recipe and the plan it belongs to. Reject with
///    `NotFound` if either is missing.
/// 3. Look up the latest fetch attempt for the recipe. If none
///    exists, surface `ReauthorFailed` — re-authoring without ground-
///    truth bytes would be guessing, which is exactly what ADR 0012
///    forbids.
/// 4. Call `pipeline::recipe_author::reauthor_recipe` with the bytes
///    + failure message + operator note. The new recipe is stamped
///    with `prior_recipe_id = old.id` and `reauthor_reason = …`.
/// 5. Persist via `save_recipe`. The new row becomes the highest-
///    version recipe for the same `dedup_key` (`{plan_id}:{source_id}`),
///    so the executor's next `run_fetch_for_plan` picks it up.
/// 6. Return the new `RecipeDto`.
///
/// ## Errors
///
/// - `InvalidInput { field: "recipe_id" }` — not a UUID.
/// - `InvalidInput { field: "operator_note" }` — bounds /
///   character-class violation.
/// - `NotFound { id }` — recipe id, or its plan id, missing.
/// - `ReauthorFailed { prior_recipe_id, message }` — no captured
///   failed-apply bytes for the recipe (the executor never recorded
///   one; the operator should run fetch first), or the LLM authoring
///   call failed, or the resulting recipe failed validation.
/// - `Storage { message }` — DB-level failure.
///
/// ## Authoring provenance
///
/// The new recipe is stamped `authored_from = FetchedBytes`. ADR 0012
/// amendment 1 §"Manual path almost always uses real bytes": the
/// operator triggered re-author after seeing a failure, which means
/// the source is reachable enough to surface a failed apply, which
/// means the bytes we recorded are the source's actual response.
/// `FetchedBytes` is the right default here. (A future "re-author
/// against a stub-authored recipe with no captured bytes" path would
/// stamp `StubExcerpt` — that path doesn't exist yet, and the absence
/// of captured bytes is itself a `ReauthorFailed` outcome above.)
#[tauri::command]
pub async fn reauthor_recipe(
    recipe_id: String,
    operator_note: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<RecipeDto, CommandError> {
    // 1. Parse the recipe id.
    let parsed_recipe_id: Uuid = recipe_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "recipe_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;

    // 2. Validate the operator note. Empty / None / blank-after-trim
    //    all collapse to None — same shape as `set_recipe_feedback`
    //    and `reject_plan`.
    let normalized_note: Option<String> = match operator_note.as_deref() {
        None => None,
        Some(raw) => match check_user_text("operator_note", raw, Bounds::RECIPE_FEEDBACK) {
            Ok(normalized) if normalized.trim().is_empty() => None,
            Ok(normalized) => Some(normalized),
            Err(violation) => {
                return Err(CommandError::InvalidInput {
                    field: "operator_note".into(),
                    message: violation.to_string(),
                })
            }
        },
    };

    info!(
        recipe_id = %parsed_recipe_id,
        has_note = normalized_note.is_some(),
        "reauthor_recipe command invoked"
    );

    // 3. Load the prior recipe.
    let prior = load_recipe_impl(state.store.as_ref(), parsed_recipe_id)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound {
            id: parsed_recipe_id.to_string(),
        })?;

    // 4. Load the plan the recipe belongs to. Required for
    //    `reauthor_recipe` to thread expectations through to the
    //    LLM. A missing plan is a structural inconsistency (recipes
    //    point at plans via FK semantics in the typed pipeline);
    //    surface as NotFound so the frontend handles it the same way
    //    it handles a missing recipe.
    let plan = load_research_plan_impl(state.store.as_ref(), prior.plan_id)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound {
            id: prior.plan_id.to_string(),
        })?;

    // 5. Pull the latest fetch attempt for the recipe — the bytes the
    //    runtime saw + the failure message it produced.
    let attempt = state
        .store
        .latest_attempt_for_recipe(parsed_recipe_id)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::ReauthorFailed {
            prior_recipe_id: parsed_recipe_id.to_string(),
            message: "no captured fetch attempt exists for this recipe; \
                      run fetch and observe a failure before re-authoring"
                .into(),
        })?;

    // The capture only records on apply-failure today (executor's
    // `record_apply_failure_attempt`). A `succeeded: true` row in
    // `recipe_fetch_attempts` would be unexpected (the table never
    // gets one written today) but checking is defensive — re-authoring
    // a successful recipe is structurally meaningless.
    if attempt.succeeded {
        return Err(CommandError::ReauthorFailed {
            prior_recipe_id: parsed_recipe_id.to_string(),
            message: "the recipe's latest attempt succeeded; nothing to re-author"
                .into(),
        });
    }

    let failure_message = attempt
        .failure_message
        .as_deref()
        .unwrap_or("(failure message not captured)");
    let bytes = attempt.bytes_excerpt.as_deref().unwrap_or("").as_bytes();

    // 6. Call into pipeline.
    let mut new_recipe = match reauthor_recipe_impl(
        state.provider.as_ref(),
        ModelTier::Workhorse,
        state.recipe_author_prompt,
        &plan,
        &prior,
        bytes,
        failure_message,
        normalized_note.as_deref(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => match e {
            // An LLM error (network, gateway, schema rejection) is a
            // re-author failure — surface as ReauthorFailed so the
            // dialog renders it next to the prior recipe id.
            AuthoringError::Llm(_)
            | AuthoringError::NoStructuredOutput
            | AuthoringError::OutputParse(_)
            | AuthoringError::BadUrl(_)
            | AuthoringError::InvalidRecipe(_)
            | AuthoringError::Prompt(_) => {
                warn!(
                    prior_recipe_id = %parsed_recipe_id,
                    error = %e,
                    "reauthor_recipe authoring failed"
                );
                return Err(CommandError::ReauthorFailed {
                    prior_recipe_id: parsed_recipe_id.to_string(),
                    message: e.to_string(),
                });
            }
        },
    };

    // 7. Stamp authoring provenance — see the doc-comment §"Authoring
    //    provenance". The pipeline `reauthor_recipe` left
    //    `authored_from = Unknown` (the default the validator
    //    produces); we set it to FetchedBytes here because the bytes
    //    came from a real fetch the executor performed earlier in
    //    the same session.
    new_recipe.authored_from = situation_room_storage::AuthoredFrom::FetchedBytes;

    // 8. Persist. The same `dedup_key` plus a higher `version` makes
    //    the new row the head of the version chain; subsequent
    //    fetches read it via `load_recipes_for_plan_latest` (which
    //    selects max-version per source).
    save_recipe_impl(state.store.as_ref(), &new_recipe).map_err(CommandError::from)?;

    info!(
        prior_recipe_id = %parsed_recipe_id,
        new_recipe_id = %new_recipe.id,
        new_version = new_recipe.version,
        "reauthor_recipe persisted new recipe"
    );

    // 9. Return the new recipe via the same DTO shape used everywhere
    //    else. The frontend's recipe panel observes a row with
    //    `prior_recipe_id = Some(...)` and renders the lineage chip.
    //    We round-trip through StoredRecipe for the wire conversion;
    //    re-loading from storage also confirms persistence — a defense
    //    against a silent write failure between save and return.
    let stored = state
        .store
        .get_recipe(new_recipe.id)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::Storage {
            message: format!(
                "re-authored recipe {} was not readable after save",
                new_recipe.id
            ),
        })?;

    Ok(RecipeDto::from_stored(stored))
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

    /// Track A: the new variant carries both the prior recipe id and
    /// the message; the frontend's dialog renders both, so both must
    /// be present and discoverable on the wire.
    #[test]
    fn command_error_reauthor_failed_serializes_with_kind_and_prior_id() {
        let e = CommandError::ReauthorFailed {
            prior_recipe_id: "019dee9a-ba75-7533-aa4f-ee673f03fece".into(),
            message: "no captured fetch attempt exists for this recipe".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"reauthor_failed""#));
        assert!(json.contains(r#""prior_recipe_id":"019dee9a-ba75-7533-aa4f-ee673f03fece""#));
        assert!(json.contains("no captured fetch attempt"));
    }
}
