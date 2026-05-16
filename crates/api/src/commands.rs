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
use situation_room_llm::{CostLedger, LlmProvider, ModelTier};
use situation_room_pipeline::authoritative::AuthorityRegistry;
use situation_room_pipeline::fetch_backoff::{BackoffFetcher, HostBackoff};
use situation_room_pipeline::entity_synth::materialize_entity_exemplars;
use situation_room_pipeline::relation_synth::materialize_relation_exemplars;
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
    classify_topic, format_classifier_id, ClassificationContext, ClassificationError,
    SourceDescriptor as PipelineSourceDescriptor, TopicUsage as ClassifierTopicUsage,
    CLASSIFIER_PROMPT_VERSION,
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
    ExpectationCoverageRecipeDto, ExpectationCoverageRowDto, FetchReportDto, FetchRunSummaryDto,
    HostBackoffSnapshotDto, PlanStatusDto, PlanSummary, RecipeDto, RecipeFeedbackDto,
    RecipeFetchAttemptDto, RecipeOutcomesHistoryEntryDto, ResearchPlanDto, SourceDescriptorDto,
    SourcesMemoryEntryDto,
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
/// - the source descriptors loaded from `config/sources.toml`
///   (post-Session-37 this file holds only two demo fixtures —
///   `csv_demo` and `json_demo`; the slice is doc-narrowed and used
///   only by the executor's `#[ignore]` live tests against those
///   fixtures, see [`pipeline::research_classifier::SourceDescriptor`]).
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
    /// Tighter-timeout HTTP client used by the fetch executor's
    /// prefetch step. Session 50 (Class C).
    ///
    /// Distinct from `http` so a slow prefetch host fails fast inside
    /// the per-source authoring deadline (see
    /// `pipeline::fetch_executor::PER_SOURCE_DEADLINE_SECS`). The LLM
    /// provider client (`http`) keeps the default 300s ceiling for
    /// legitimately long completions; this client is built with a
    /// 60s `total_timeout` so a single prefetch attempt cannot
    /// consume the entire 240s deadline.
    ///
    /// Both clients share the per-host backoff state (`host_backoff`)
    /// so an observed 429 / Retry-After / timeout on a host during
    /// prefetch carries over to the runtime fetch and vice versa.
    /// The split is purely network-layer (timeout shape); nothing
    /// here mentions a host or scheme.
    pub prefetch_http: Arc<SecureHttpClient>,
    /// Per-host backoff state shared across every fetch the executor
    /// runs. Session 45 — see [`HostBackoff`]'s module-level rationale
    /// in `crates/pipeline/src/fetch_backoff.rs`. Lives in `AppState`
    /// (not built per-`run_fetch_for_plan` call) so observed signals
    /// like a 429 from a host during a prefetch carry over to the
    /// runtime fetch in the same session, and across sessions until
    /// the binary restarts.
    pub host_backoff: Arc<HostBackoff>,
    /// Session 75 — process-wide cost ledger. The provider is wrapped
    /// in `MeteredProvider` in the composition root; that wrap holds
    /// the same `Arc<CostLedger>` we keep here. Reading from the
    /// dashboard goes through `llm_cost_ledger` which calls
    /// `cost_ledger.snapshot()` directly — no provider round-trip.
    pub cost_ledger: Arc<CostLedger>,
    pub classifier_prompt: &'static str,
    pub recipe_author_prompt: &'static str,
    /// The Session-39 propose-URL prompt — consumed by the fetch
    /// executor's per-attempt URL-discovery step. Loaded the same way
    /// as the other prompts (binary `include_str!`).
    pub propose_url_prompt: &'static str,
    /// Session 77 — per-Document Assertion extraction prompt.
    /// Consumed by `pipeline::extract::extract_and_persist_assertions`,
    /// which the fetch executor calls once per article-kind Document
    /// (gated on MIME + non-empty body) right after the
    /// Session-69 `insert_fetch_document` hook. Loaded with the same
    /// `include_str!` pattern as the other prompts.
    pub document_assertions_prompt: &'static str,
    /// Session 78 — per-Document Event extraction prompt. Sibling of
    /// `document_assertions_prompt`. Consumed by
    /// `pipeline::extract::extract_and_persist_events`, called by
    /// each run_X_recipe runner immediately after the assertion
    /// extraction call. Same loading pattern (compile-time
    /// `include_str!`). Cost is gated upstream: plans with no
    /// declared `event_kinds` short-circuit before the workhorse-tier
    /// call.
    pub document_events_prompt: &'static str,
    /// Session 79 — per-Document Observation extraction prompt.
    /// Third sibling of the assertion + event extraction prompts.
    /// Consumed by `pipeline::extract::extract_and_persist_observations`,
    /// called by each run_X_recipe runner immediately after the
    /// event extraction call. Same loading pattern (compile-time
    /// `include_str!`). Cost is gated upstream: plans with no
    /// declared `observation_metrics` short-circuit before the
    /// workhorse-tier call.
    pub document_observations_prompt: &'static str,
    /// Session 80 — per-Document EntityAttribute extraction prompt.
    /// Fourth sibling of the three earlier extractor prompts. Consumed
    /// by `pipeline::extract::extract_and_persist_entity_attributes`,
    /// called by each run_X_recipe runner immediately after the
    /// observation extraction call. Same loading pattern (compile-time
    /// `include_str!`). v1 has no closed-vocab gate on attribute names
    /// — open-vocab matches the `EntityAttributeContent.key` schema.
    pub document_entity_attributes_prompt: &'static str,
    /// Doc-narrowed under ADR 0015 (Session 37). The classifier no
    /// longer consults this list; only the executor's `#[ignore]`
    /// live tests do (against `csv_demo` / `json_demo`). Production
    /// classification uses [`Store::sources_memory`] instead.
    pub sources: Vec<PipelineSourceDescriptor>,
    /// Session 82 — ADR 0004 pathway 1 registry. Loaded once at the
    /// composition root from
    /// `config/vocab/authoritative_sources.toml`. The
    /// `promote_consensus_for_plan` command clones into the per-call
    /// `PromoteConfig`; the auto-trigger in `run_fetch_for_plan` does
    /// the same. Wrapped in `Arc` so the auto-trigger doesn't pay the
    /// clone cost in the hot path (the per-Tauri-command path still
    /// clones; the registry is small).
    pub authoritative: Arc<AuthorityRegistry>,
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
    /// Session 46: how many runs the recipe-success heatmap can show
    /// in one column-axis. The heatmap renders runs left-to-right; at
    /// 50 columns the strip stays scannable on a 1280px-wide review
    /// pane. Anything beyond this would compress cells past
    /// usability; the operator can pick a tighter window if they want
    /// to focus on recent history.
    pub const MAX_OUTCOMES_HISTORY_RUNS: usize = 50;

    pub fn new(
        store: Arc<Store>,
        provider: Arc<dyn LlmProvider + Send + Sync>,
        http: Arc<SecureHttpClient>,
        prefetch_http: Arc<SecureHttpClient>,
        cost_ledger: Arc<CostLedger>,
        classifier_prompt: &'static str,
        recipe_author_prompt: &'static str,
        propose_url_prompt: &'static str,
        document_assertions_prompt: &'static str,
        document_events_prompt: &'static str,
        document_observations_prompt: &'static str,
        document_entity_attributes_prompt: &'static str,
        sources: Vec<PipelineSourceDescriptor>,
        authoritative: Arc<AuthorityRegistry>,
    ) -> Self {
        Self {
            store,
            provider,
            http,
            prefetch_http,
            // Session 45: a fresh empty `HostBackoff` per binary boot.
            // The composition root does not need to thread this in —
            // there are no per-deployment knobs (the policy is
            // uniform; runtime adapts on observed signals). Keeping
            // the field internal also means future tweaks to the
            // backoff schedule don't ripple through the binary
            // signatures.
            host_backoff: Arc::new(HostBackoff::new()),
            cost_ledger,
            classifier_prompt,
            recipe_author_prompt,
            propose_url_prompt,
            document_assertions_prompt,
            document_events_prompt,
            document_observations_prompt,
            document_entity_attributes_prompt,
            sources,
            authoritative,
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

    /// The manual re-author (Track A) completed an LLM round-trip,
    /// the response validated, and the LLM explicitly **declined** to
    /// author a corrected recipe — Track B's decline channel
    /// (ADR 0007 amendment 4) firing inside the re-author path.
    ///
    /// Architecturally distinct from `ReauthorFailed`: nothing broke.
    /// The LLM read the bytes + the prior recipe's selectors + the
    /// failure message and reached the honest conclusion that no
    /// recipe is possible under the closed extraction vocabulary.
    /// Surfacing this as a failure (the pre-Session-66 behavior, via
    /// `ReauthorFailed` with a `[declined]` prefix on the message)
    /// caused the dialog to stay open as if the IPC had errored,
    /// which Session 66's operator testing flagged as a UX bug — the
    /// dialog "reappeared with the same message" because no new
    /// recipe was persisted and no clear signal told the operator
    /// the LLM had declined.
    ///
    /// Wire shape mirrors `ReauthorFailed`: prior recipe id + the
    /// LLM's prose reason. The reason is bounded at the authoring
    /// step by [`Bounds::DECLINE_REASON`] (2 000 chars); it has
    /// already been validated by the LLM provider's structured-
    /// output enforcement by the time we get here.
    ///
    /// Frontend handles this distinctly: dialog closes, the failed-
    /// apply row in FetchReport / RecipesPanel surfaces the decline
    /// reason as a per-row badge so the operator sees what the LLM
    /// said without an error banner.
    ///
    /// The supersession path from `ReauthorFailed[declined]` to this
    /// variant is the Session-29 follow-up the original `Declined`-
    /// match-arm comment in `commands.rs::reauthor_recipe` flagged.
    #[error("re-author declined for recipe {prior_recipe_id}: {reason}")]
    ReauthorDeclined {
        prior_recipe_id: String,
        reason: String,
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

    // ADR 0015 / Session 37: pull the sources memory from the
    // recipes ⨝ recipe_fetch_attempts ⨝ research_plans join. The
    // pre-Session-37 path threaded `state.sources` (the static
    // `config/sources.toml` registry) here; that registry has been
    // retired (see ADR 0015 §"Configuration").
    let sources_memory = state
        .store
        .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
        .map_err(CommandError::from)?;

    let ctx = ClassificationContext {
        existing_topics,
        sources_memory,
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
    // Session 77 — stamp the prompt version alongside the provider id
    // (`"xai@2.2"`) so the per-plan dashboard can render a re-classify
    // banner when the persisted version trails the shipping prompt.
    // `format_classifier_id` is the single source of truth; the parser
    // sibling `parse_classifier_id` handles pre-Session-77 plans (bare
    // provider id, no `@`) and surfaces them as "stale version" too.
    let classifier_id = format_classifier_id(state.provider.id());
    if let Err(e) = save_research_plan(state.store.as_ref(), &plan, &classifier_id) {
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

    // Session 76 — on accept, promote each `entity_kinds[*].exemplars[*]`
    // to a persisted Entity row. Idempotent (the `entities.entity_id`
    // UNIQUE index plus an upfront existence check absorb repeats), no
    // LLM calls, never fails plan-accept: per-exemplar failures land in
    // the returned `MaterializationReport.errors` and are logged once
    // by the materialiser itself. We deliberately ignore the report
    // here — the operator-visible signal is the dashboard's Entities
    // panel lighting up, not an extra log line out of this command.
    if matches!(new_status, PlanStatus::Accepted) {
        match build_typed_plan_from_stored(&stored) {
            Ok(plan) => {
                let accepted_at = chrono::Utc::now();
                // Session 76 — entity exemplars to populate the
                // dashboard's Entities panel.
                let _ = materialize_entity_exemplars(
                    &plan,
                    state.store.as_ref(),
                    accepted_at,
                );
                // Session 77 — relation triples (sibling materialiser).
                // Same posture as the entity hook: we deliberately
                // ignore the returned `MaterializationReport` because
                // the operator-visible signal is the dashboard's
                // Relations panel lighting up, not an extra log line
                // out of this command. Per-triple failures are warn-
                // logged inside the materialiser itself.
                let _ = materialize_relation_exemplars(
                    &plan,
                    state.store.as_ref(),
                    accepted_at,
                );
            }
            Err(e) => {
                warn!(
                    plan_id = %parsed,
                    error = %e,
                    "accept_plan: entity/relation exemplar materialisation skipped — \
                     plan deserialisation failed; the per-plan dashboard \
                     entities + relations panels will stay empty for this plan"
                );
            }
        }
    }

    ResearchPlanDto::from_stored(stored).map_err(|e| CommandError::Storage {
        message: format!("plan deserialization: {e}"),
    })
}

/// Reassemble the pipeline-typed [`ResearchPlan`] from a
/// `StoredResearchPlan` row. Mirrors the JSON-decoding half of
/// [`ResearchPlanDto::from_stored`] but yields the typed
/// `pipeline::research::ResearchPlan` rather than the DTO — that's
/// the shape `entity_synth::materialize_entity_exemplars` accepts.
///
/// Kept here (private) rather than in `pipeline::research_plans_store`
/// because it's an `accept_plan`-local concern: every other caller
/// either already has the typed plan in hand (the classifier path)
/// or wants the DTO shape (every other command). Surfacing this in
/// `research_plans_store` would invite drift between two near-
/// identical builders.
fn build_typed_plan_from_stored(
    s: &situation_room_storage::research_plans::StoredResearchPlan,
) -> Result<situation_room_pipeline::research::ResearchPlan, serde_json::Error> {
    use situation_room_pipeline::research::{GeoScope, RecordExpectations, ResearchPlan};
    let topic_tags: Vec<situation_room_core::vocab::Topic> =
        serde_json::from_str(&s.topic_tags_json)?;
    let geographic_scope: Vec<GeoScope> = serde_json::from_str(&s.geographic_scope_json)?;
    let expectations: RecordExpectations = serde_json::from_str(&s.expectations_json)?;

    Ok(ResearchPlan {
        id: s.id,
        topic: s.topic.clone(),
        interpretation: s.interpretation.clone(),
        topic_tags,
        geographic_scope,
        historical_window_days: s.historical_window_days,
        expectations,
        created_at: s.created_at,
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

    // ADR 0015 / Session 37: same memory-derived context as `classify`.
    let sources_memory = state
        .store
        .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
        .map_err(CommandError::from)?;

    let ctx = ClassificationContext {
        existing_topics,
        sources_memory,
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
    // Session 77 — same provider-with-version stamping as `classify`.
    let classifier_id = format_classifier_id(state.provider.id());
    if let Err(e) = save_research_plan_with_lineage(
        state.store.as_ref(),
        &new_plan,
        &classifier_id,
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

    // Session 45: wrap the raw `SecureHttpClient` in the per-host
    // backoff decorator before handing it to the executor. The
    // wrapped fetcher applies pre-flight `next_allowed_at` waits and
    // records observed signals (429, `Retry-After`, timeouts) into
    // the shared `HostBackoff` state — see
    // `crates/pipeline/src/fetch_backoff.rs` for the policy. The
    // wrapper has scope-bound lifetime; `host_backoff` lives in
    // `AppState` so state survives across `run_fetch_for_plan` calls.
    let backoff_fetcher = BackoffFetcher::new(state.http.as_ref(), state.host_backoff.clone());
    // Session 50 (Class C): wrap the dedicated prefetch client in
    // its own backoff decorator. Both wrappers share the same
    // `host_backoff` state — observed signals (429 / Retry-After
    // / timeouts) on a host during prefetch carry over to the
    // runtime fetch's backoff decisions and vice versa, exactly as
    // the pre-Session-50 single-client flow already did. The
    // difference is that the prefetch client's tighter
    // `total_timeout` (60s vs. the 300s LLM ceiling) prevents a
    // slow host from eating the entire per-source authoring
    // deadline (PER_SOURCE_DEADLINE_SECS = 240s) on a single
    // attempt.
    let backoff_prefetch =
        BackoffFetcher::new(state.prefetch_http.as_ref(), state.host_backoff.clone());
    let ctx = ExecutorContext {
        store: state.store.as_ref(),
        http: &backoff_fetcher,
        prefetch_http: Some(&backoff_prefetch),
        provider: state.provider.as_ref(),
        recipe_author_prompt: state.recipe_author_prompt,
        propose_url_prompt: state.propose_url_prompt,
        // Session 77 — per-Document Assertion synthesis. Pass the
        // loaded prompt here; the executor will call into
        // `pipeline::extract::extract_and_persist_assertions` once
        // per article-kind Document. Test contexts that don't want
        // an LLM call per fetched URL pass `None` (see eval_harness
        // composition root for the rationale).
        document_assertions_prompt: Some(state.document_assertions_prompt),
        // Session 78 — per-Document Event extraction prompt. Same
        // posture as the assertion prompt above: production passes
        // `Some(_)`; the eval harness composition root passes
        // `None` to keep cost bounded for repeat trials.
        document_events_prompt: Some(state.document_events_prompt),
        // Session 79 — per-Document Observation extraction prompt.
        // Same posture as the assertion + event prompts above:
        // production passes `Some(_)`; the eval harness composition
        // root passes `None` to keep cost bounded for repeat trials.
        document_observations_prompt: Some(state.document_observations_prompt),
        // Session 80 — per-Document EntityAttribute extraction prompt.
        // Same posture as the three earlier extractor prompts.
        document_entity_attributes_prompt: Some(state.document_entity_attributes_prompt),
        // The same slice the classifier sees, threaded through to
        // the executor. Doc-narrowed under ADR 0015 (Session 37) and
        // further under Session 39: production authoring no longer
        // consults this slice. Pass-through preserved for the
        // `#[ignore]` live tests that author against the demo
        // fixtures.
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

    // Session 82 — auto-trigger promotion (ADR 0021 deferred half +
    // ADR 0004 pathway 1). Runs the authoritative pass first (if the
    // registry is non-empty) then the consensus pass over the
    // remaining Assertions. Scoped to the plan that just completed —
    // ADR 0021 flagged cross-plan dedup as the open trade-off; running
    // only against the just-completed plan limits surprises to that
    // plan's claim pile.
    //
    // Failure-mode discipline: a promote-stage error MUST NOT block
    // the FetchReport from reaching the operator. The fetch already
    // ran; whatever it produced should surface. We warn-log the
    // promote failure and continue.
    if let Err(e) = auto_promote_after_fetch(&state, parsed).await {
        warn!(
            plan_id = %parsed,
            error = %e,
            "auto-promote after fetch run failed — operator can still invoke manually"
        );
    }

    Ok(FetchReportDto::from_typed(report))
}

/// Session 82 — promote-after-fetch auto-trigger. Separated from
/// `run_fetch_for_plan` so the failure-isolation is explicit and the
/// promote-stage's `Result` doesn't pollute the executor's signature.
async fn auto_promote_after_fetch(
    state: &tauri::State<'_, AppState>,
    plan_id: Uuid,
) -> Result<(), CommandError> {
    let stored = state
        .store
        .get_research_plan(plan_id)
        .map_err(CommandError::from)?;
    let stored = match stored {
        Some(s) => s,
        // Plan vanished between fetch-run completion and now. Skip;
        // the warn-log on the caller side will surface the missing
        // plan id.
        None => return Ok(()),
    };

    // Only run against `Accepted` plans. Rejected plans don't get
    // auto-promoted — if an operator marks a plan rejected they likely
    // don't want consensus-derived rows landing under it.
    if !matches!(stored.status, PlanStatus::Accepted) {
        info!(
            plan_id = %plan_id,
            status = ?stored.status,
            "skipping auto-promote — plan not in Accepted status"
        );
        return Ok(());
    }

    let plan = match situation_room_pipeline::research_plans_store::load_research_plan(
        state.store.as_ref(),
        plan_id,
    )
    .map_err(|e| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("plan deserialization failed: {e}"),
    })? {
        Some(p) => p,
        None => return Ok(()),
    };

    let cfg = situation_room_pipeline::promote::PromoteConfig {
        min_independent_claimants: situation_room_pipeline::promote::PromoteConfig::default()
            .min_independent_claimants,
        authoritative: (*state.authoritative).clone(),
    };

    let report = situation_room_pipeline::promote::promote_consensus_for_plan(
        state.store.as_ref(),
        &plan,
        &cfg,
    )
    .map_err(|e| match e {
        situation_room_pipeline::promote::PromoteError::Storage(s) => CommandError::from(s),
    })?;

    info!(
        plan_id = %plan_id,
        considered = report.assertions_considered,
        authoritative = report.authoritative_promoted,
        consensus = report.groups_promoted,
        skipped = report.skipped_already_promoted,
        "auto-promote after fetch run complete"
    );

    Ok(())
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
/// - `failure_message_override` — **Session 68 follow-up**.
///   Optional. The failure message to present to the LLM when no
///   `recipe_fetch_attempts` row exists for this recipe. The
///   executor only captures rows on apply-stage failures; fetch-
///   stage failures (status 4xx/5xx, timeouts) have nothing in the
///   table, so the pre-Session-68 lookup-or-fail path bailed even
///   though re-authoring against the failure message alone is a
///   real product use case ("the URL is wrong; propose a different
///   one"). The frontend's FetchReport surface passes the outcome's
///   `message` field here. When the captured row exists (apply-
///   stage), the override is ignored and the row remains
///   authoritative.
///
/// ## Behaviour
///
/// 1. Validate inputs.
/// 2. Load the prior recipe and the plan it belongs to. Reject with
///    `NotFound` if either is missing.
/// 3. Look up the latest fetch attempt for the recipe.
///    - **Row exists** → use the captured bytes + failure message.
///    - **Row missing + override supplied** → use empty bytes +
///      override message (the fetch-stage path).
///    - **Row missing + no override** → surface `ReauthorFailed`.
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
/// - `InvalidInput { field: "failure_message_override" }` — same
///   bounds violation as `operator_note`.
/// - `NotFound { id }` — recipe id, or its plan id, missing.
/// - `ReauthorFailed { prior_recipe_id, message }` — no captured
///   failed-apply bytes AND no `failure_message_override` (the
///   operator should run fetch first, or the frontend should pass
///   the override), or the LLM authoring call failed, or the
///   resulting recipe failed validation.
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
    // Session 68 follow-up — see `## Inputs` above for the rationale.
    // (Doc comments aren't permitted on function parameters; the
    // load-bearing explanation lives in the function-level docstring.)
    failure_message_override: Option<String>,
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

    // Validate the override the same way — same character class +
    // length bounds. (The frontend supplies executor-emitted prose
    // verbatim; bounds-checking it makes the trust boundary
    // symmetric with operator_note above.)
    let normalized_override: Option<String> = match failure_message_override.as_deref() {
        None => None,
        Some(raw) => match check_user_text(
            "failure_message_override",
            raw,
            Bounds::RECIPE_FEEDBACK,
        ) {
            Ok(normalized) if normalized.trim().is_empty() => None,
            Ok(normalized) => Some(normalized),
            Err(violation) => {
                return Err(CommandError::InvalidInput {
                    field: "failure_message_override".into(),
                    message: violation.to_string(),
                })
            }
        },
    };

    info!(
        recipe_id = %parsed_recipe_id,
        has_note = normalized_note.is_some(),
        has_failure_override = normalized_override.is_some(),
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

    // 5. Pull the latest fetch attempt for the recipe — when present,
    //    the bytes the runtime saw + the failure message it produced
    //    are the load-bearing evidence for re-authoring.
    //
    //    **Session 68 follow-up:** when no row exists (the executor
    //    only captures apply-failure rows; fetch-stage failures like
    //    503s have nothing in the table), fall back to the frontend-
    //    supplied `failure_message_override` with empty bytes. The
    //    LLM gets "this recipe failed with `<msg>`; no body was
    //    captured" and authors against the prior recipe's selectors
    //    + the failure shape. Useful when the failure is "wrong URL"
    //    (404) or "wrong host shape" (403/WAF) where the LLM can
    //    propose a corrected URL without seeing bytes.
    let attempt_opt = state
        .store
        .latest_attempt_for_recipe(parsed_recipe_id)
        .map_err(CommandError::from)?;

    let (failure_message_owned, bytes_owned): (String, Vec<u8>) =
        match (attempt_opt, &normalized_override) {
            (Some(attempt), _) => {
                // Captured row exists — it's authoritative. The defensive
                // `succeeded: true` check still applies.
                if attempt.succeeded {
                    return Err(CommandError::ReauthorFailed {
                        prior_recipe_id: parsed_recipe_id.to_string(),
                        message: "the recipe's latest attempt succeeded; nothing to re-author"
                            .into(),
                    });
                }
                let msg = attempt
                    .failure_message
                    .clone()
                    .unwrap_or_else(|| "(failure message not captured)".into());
                let bytes = attempt
                    .bytes_excerpt
                    .as_deref()
                    .unwrap_or("")
                    .as_bytes()
                    .to_vec();
                (msg, bytes)
            }
            (None, Some(override_msg)) => {
                // Fetch-stage failure path: use the frontend's outcome
                // message verbatim, no bytes. The downstream pipeline
                // call already accepts an empty `bytes` slice — that's
                // the architectural signal the LLM gets that the
                // source returned the failure before any body could be
                // captured.
                (override_msg.clone(), Vec::new())
            }
            (None, None) => {
                return Err(CommandError::ReauthorFailed {
                    prior_recipe_id: parsed_recipe_id.to_string(),
                    message: "no captured fetch attempt exists for this recipe and \
                              no failure message was supplied; run fetch and observe \
                              a failure before re-authoring, or pass \
                              failure_message_override from the FetchReport outcome"
                        .into(),
                });
            }
        };
    let failure_message: &str = failure_message_owned.as_str();
    let bytes: &[u8] = bytes_owned.as_slice();

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
            // Track B (Session 28, ADR 0007 amendment 4): the LLM
            // exercised the decline channel during re-author. The
            // operator clicked "re-author" on a previously-failed
            // recipe and the LLM responded "I cannot author a recipe
            // for this source." Architecturally this is *not* a
            // re-author failure (the LLM call worked, the schema
            // validated, the answer was honest); architecturally it
            // is a *new authoring outcome*.
            //
            // **Session 66 landed the dedicated wire variant** —
            // [`CommandError::ReauthorDeclined`] — that the original
            // Session 29 follow-up note flagged. The pre-Session-66
            // shim squeezed declines through `ReauthorFailed` with a
            // `[declined]` prefix on the message; the frontend ate
            // the error, the dialog stayed open as if the IPC had
            // crashed, and the operator saw "the same message"
            // reappear with no clear signal that the LLM had
            // declined. Session 66's plan-review on a Fed re-author
            // surfaced this directly.
            //
            // With the dedicated variant, the frontend closes the
            // dialog cleanly and surfaces the LLM's prose reason as
            // a per-row decline badge on the failed-apply row in
            // FetchReport / RecipesPanel. The reason flows through
            // unmodified — no `[declined]` prefix; the wire variant
            // IS the discriminator.
            AuthoringError::Declined { reason } => {
                warn!(
                    prior_recipe_id = %parsed_recipe_id,
                    decline_reason = %reason,
                    "reauthor_recipe declined by LLM"
                );
                return Err(CommandError::ReauthorDeclined {
                    prior_recipe_id: parsed_recipe_id.to_string(),
                    reason,
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
// Command 13 — recipe_outcomes_history (Session 46)
// ---------------------------------------------------------------------------

/// Per-(recipe-or-source) outcome history across the plan's recent
/// runs. Pure read; no LLM call, no fetch.
///
/// The frontend's recipe-success heatmap calls this on plan
/// selection. Each returned entry is one row in the heatmap; each
/// `runs[i]` is one column-cell. Cells are ordered oldest-first so
/// the frontend renders runs left-to-right without sorting; entries
/// arrive in insertion order (the order they first appeared in the
/// run history) which keeps the recipe rows visually stable across
/// renders.
///
/// `run_limit` clamps the **runs** dimension — only outcomes from the
/// most recent N runs are returned. Older runs fall off the front of
/// the heatmap; recipes that only appear in older runs are dropped
/// from the result entirely. Defaults to a sensible ceiling
/// ([`AppState::MAX_OUTCOMES_HISTORY_RUNS`]).
///
/// Errors:
///   - `InvalidInput { field: "plan_id" }` — plan_id isn't a UUID.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn recipe_outcomes_history(
    plan_id: String,
    run_limit: usize,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<RecipeOutcomesHistoryEntryDto>, CommandError> {
    let parsed: Uuid = plan_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "plan_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;
    let clamped = run_limit.clamp(1, AppState::MAX_OUTCOMES_HISTORY_RUNS);

    let stored = state
        .store
        .recipe_outcomes_history_for_plan(parsed, clamped)
        .map_err(CommandError::from)?;

    Ok(stored
        .into_iter()
        .map(RecipeOutcomesHistoryEntryDto::from_stored)
        .collect())
}

// ---------------------------------------------------------------------------
// Command 14 — expectation_coverage (Session 46)
// ---------------------------------------------------------------------------

/// Plan-expectation coverage matrix: which recipes target which
/// expectations, plus an explicit row per uncovered expectation.
/// Pure read; no LLM call.
///
/// The recipe-author prompt's coverage discipline (v1.14
/// §"Coverage discipline — bindings vs expectations") deliberately
/// accepts narrow honest coverage — one recipe targeting one
/// expectation index when the source structurally yields one
/// scalar. This command surfaces that coverage so the operator sees
/// it instead of having to read recipe JSON.
///
/// Returns one row per (bucket, index) the plan declares, plus zero
/// or more recipes per row that bind to it. Uncovered rows surface
/// with `recipes` empty.
///
/// Buckets covered: `observation_metric`, `event_type`,
/// `entity_kind`, `relation_kind`. Document and Assertion
/// expectations are not addressed by recipe `produces` bindings —
/// they're surfaced through other surfaces and not part of this
/// matrix.
///
/// Errors:
///   - `InvalidInput { field: "plan_id" }` — plan_id isn't a UUID.
///   - `NotFound` — plan with this id isn't in the store.
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn expectation_coverage(
    plan_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ExpectationCoverageRowDto>, CommandError> {
    let parsed: Uuid = plan_id
        .parse()
        .map_err(|e: uuid::Error| CommandError::InvalidInput {
            field: "plan_id".into(),
            message: format!("not a valid UUID: {e}"),
        })?;

    // Load the plan so we know the full expectation matrix shape.
    // Without it we couldn't surface uncovered expectations.
    let stored_plan = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound {
            id: plan_id.clone(),
        })?;
    let plan = ResearchPlanDto::from_stored(stored_plan).map_err(|e| CommandError::Storage {
        message: format!("plan deserialization: {e}"),
    })?;

    // Load the recipes so we know which (bucket, index) pairs each
    // recipe binds to.
    let recipes = state
        .store
        .recipes_for_plan(parsed)
        .map_err(CommandError::from)?;

    Ok(build_expectation_coverage(&plan, &recipes))
}

/// Walk the plan's expectations and the recipes' `produces` bindings
/// to assemble the coverage matrix. Pure function; no I/O.
///
/// The function lives at module scope (rather than inside the
/// command) so it's testable against a synthetic plan + recipe pair
/// without spinning up a Store.
///
/// ## Bucket vocabulary
///
/// The `expectation.list` strings in `produces` bindings come from
/// the recipe-author prompt's closed vocabulary:
/// `observation_metric` | `event_type` | `entity_kind` |
/// `relation_kind`. Document and Assertion expectations don't have
/// bindings — they're carried by other surfaces.
fn build_expectation_coverage(
    plan: &ResearchPlanDto,
    recipes: &[situation_room_storage::StoredRecipe],
) -> Vec<ExpectationCoverageRowDto> {
    // 1. Index each recipe's bindings by (bucket, index).
    //
    //    The `produces` JSON shape (recipe_author.md §"What to
    //    produce") is an array of objects:
    //
    //        [{
    //          "record_type": "observation",
    //          "expectation": { "list": "observation_metric", "index": 0 },
    //          ...
    //        }, ...]
    //
    //    We parse it leniently — a recipe with a malformed produces
    //    column is still listed as "no coverage" rather than
    //    crashing the whole matrix. Same posture `RecipeDto`'s
    //    `from_stored` takes on parse failure.
    let mut by_key: std::collections::HashMap<(String, u32), Vec<ExpectationCoverageRecipeDto>> =
        std::collections::HashMap::new();

    for recipe in recipes {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&recipe.produces_json);
        let bindings = match parsed.as_ref().ok().and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => continue,
        };

        for binding in bindings {
            let bucket = binding
                .get("expectation")
                .and_then(|e| e.get("list"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let index = binding
                .get("expectation")
                .and_then(|e| e.get("index"))
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let record_type = binding
                .get("record_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let (Some(bucket), Some(index)) = (bucket, index) {
                by_key.entry((bucket, index)).or_default().push(
                    ExpectationCoverageRecipeDto {
                        recipe_id: recipe.id.to_string(),
                        source_id: recipe.source_id.clone(),
                        record_type,
                    },
                );
            }
        }
    }

    // 2. Walk the plan's four binding-addressable buckets in a
    //    stable order, emitting one row per (bucket, index) the plan
    //    declares.
    let mut rows: Vec<ExpectationCoverageRowDto> = Vec::new();

    for (i, m) in plan.expectations.observation_metrics.iter().enumerate() {
        let key = ("observation_metric".to_string(), i as u32);
        let recipes = by_key.remove(&key).unwrap_or_default();
        rows.push(ExpectationCoverageRowDto {
            bucket: key.0,
            index: key.1,
            label: m.name.clone(),
            recipes,
        });
    }
    for (i, e) in plan.expectations.event_types.iter().enumerate() {
        let key = ("event_type".to_string(), i as u32);
        let recipes = by_key.remove(&key).unwrap_or_default();
        rows.push(ExpectationCoverageRowDto {
            bucket: key.0,
            index: key.1,
            label: e.event_type.clone(),
            recipes,
        });
    }
    for (i, e) in plan.expectations.entity_kinds.iter().enumerate() {
        let key = ("entity_kind".to_string(), i as u32);
        let recipes = by_key.remove(&key).unwrap_or_default();
        rows.push(ExpectationCoverageRowDto {
            bucket: key.0,
            index: key.1,
            label: e.kind.clone(),
            recipes,
        });
    }
    for (i, r) in plan.expectations.relation_kinds.iter().enumerate() {
        let key = ("relation_kind".to_string(), i as u32);
        let recipes = by_key.remove(&key).unwrap_or_default();
        rows.push(ExpectationCoverageRowDto {
            bucket: key.0,
            index: key.1,
            label: r.kind.clone(),
            recipes,
        });
    }

    // 3. Any leftover bindings in `by_key` reference an
    //    (bucket, index) the plan no longer declares. This can
    //    happen if a plan was edited in storage between recipe
    //    authoring and this query, or if a recipe targets an index
    //    out of range. Surface them as orphan rows with `label = ""`
    //    so the operator sees the inconsistency rather than the
    //    matrix silently dropping the binding. Sort the leftovers by
    //    bucket then index for stable output.
    let mut orphans: Vec<((String, u32), Vec<ExpectationCoverageRecipeDto>)> =
        by_key.into_iter().collect();
    orphans.sort_by(|a, b| a.0.cmp(&b.0));
    for ((bucket, index), recipes) in orphans {
        rows.push(ExpectationCoverageRowDto {
            bucket,
            index,
            label: String::new(),
            recipes,
        });
    }

    rows
}

// ---------------------------------------------------------------------------
// Command 15 — host_backoff_state (Session 48, piece B)
// ---------------------------------------------------------------------------

/// Per-host backoff snapshot — what the network layer has observed
/// during this binary's session. Pure read; no LLM call, no fetch.
///
/// One entry per host the adaptation layer has ever recorded a signal
/// for. Hosts whose only history is success appear with
/// `consecutive_failures = 0` and `wait_seconds_remaining = 0` — the
/// row's existence is itself the signal that the host has been touched
/// at least once. The frontend distinguishes three states:
///
///   - `consecutive_failures = 0, wait_seconds_remaining = 0` → clean.
///     The host has succeeded at least once, no failure pressure.
///   - `consecutive_failures > 0, wait_seconds_remaining = 0` →
///     recovering. The schedule has expired so the next request fires
///     immediately, but the failure history is still in effect for the
///     next observed failure.
///   - `consecutive_failures > 0, wait_seconds_remaining > 0` →
///     blocked. The next request to this host will sleep at least the
///     remaining wait before firing.
///
/// **No source-specific routing.** The host string is a runtime key,
/// not a config knob — see the Session 45 `HostBackoff` module rationale
/// in `crates/pipeline/src/fetch_backoff.rs`. This command surfaces
/// what the adaptation layer has *observed*; it does not configure
/// behaviour.
///
/// Errors: none of the input-validation kinds (no inputs); pure read
/// over `state.host_backoff.snapshot()`. The accessor itself is
/// infallible.
#[tauri::command]
pub async fn host_backoff_state(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<HostBackoffSnapshotDto>, CommandError> {
    Ok(state
        .host_backoff
        .snapshot()
        .into_iter()
        .map(HostBackoffSnapshotDto::from_typed)
        .collect())
}

// ---------------------------------------------------------------------------
// Command 16 — sources_memory (Session 48, piece C)
// ---------------------------------------------------------------------------

/// Operator projection of the sources-memory listing — the same rows
/// the classifier consumes via `{{SOURCES_MEMORY}}`. Pure read; no LLM
/// call.
///
/// ## Why this surface earns its weight
///
/// The classifier prompt is taught to "stamp `known_id` when your
/// emitted URL corresponds to a memory entry" and to "fall back to
/// training knowledge when memory is empty." Before this command the
/// memory was invisible to the operator: a classifier that didn't pick
/// up an obvious-to-the-operator past success looked broken from
/// outside, but the underlying cause might have been a stale
/// `last_attempted_at` (the URL succeeded long ago but the recency-
/// sort dropped it past the top-30 cap) or a topic-tag mismatch.
/// Surfacing the memory makes both diagnosable.
///
/// ## What the operator sees vs. what the classifier sees
///
/// Identical row contents. The operator sees the rows in the same
/// recency-sorted order the classifier reads, and the surface
/// presents the same fields (URL, source_id, success count, last
/// success timestamp, associated topics). The cap matches
/// [`situation_room_storage::SOURCES_MEMORY_LIMIT`] so the operator
/// view doesn't drift from the classifier view.
///
/// **No source-specific routing.** ADR 0007 §"runtime path": the
/// memory is *summary of past successes*, not a registry; the surface
/// reads what storage holds, no filtering or curation in this command.
///
/// Errors:
///   - `Storage` — DB-level failure.
#[tauri::command]
pub async fn sources_memory(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SourcesMemoryEntryDto>, CommandError> {
    let typed = state
        .store
        .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
        .map_err(CommandError::from)?;
    Ok(typed
        .into_iter()
        .map(SourcesMemoryEntryDto::from_typed)
        .collect())
}

// ---------------------------------------------------------------------------
// Command 17 — llm_cost_ledger (Session 75)
// ---------------------------------------------------------------------------

/// Snapshot of the LLM cost ledger — one row per `(provider, tier)`
/// bucket the binary has seen completion responses for. Pure read; no
/// LLM call, no fetch.
///
/// The ledger lives in [`AppState::cost_ledger`] and is populated by
/// the `MeteredProvider` wrap installed in the desktop composition
/// root. Rows accumulate across the whole binary session and reset on
/// restart; persistence is intentionally out of scope (see the
/// cost_ledger module docs).
///
/// **No source-specific routing.** The ledger is keyed on
/// (provider_id, tier) — neither field carries host or model-name
/// detail; the closed-vocabulary discipline holds.
///
/// Errors: none. Lock poisoning recovers in-band; missing per-call
/// usage data surfaces as zeros without erroring (see Tally docs).
#[tauri::command]
pub async fn llm_cost_ledger(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<crate::types_export::LlmCostLedgerEntryDto>, CommandError> {
    Ok(state
        .cost_ledger
        .snapshot()
        .into_iter()
        .map(crate::types_export::LlmCostLedgerEntryDto::from_typed)
        .collect())
}

// ---------------------------------------------------------------------------
// Command 17b — llm_cost_timeline (Session 81)
// ---------------------------------------------------------------------------

/// Return the per-call timeline ring buffer (50 newest LLM
/// completions, oldest-first). Sibling to [`llm_cost_ledger`] —
/// that command surfaces cumulative bucket tallies, this one
/// surfaces *when* the calls happened so operators can spot cost
/// spikes in real time.
///
/// Pure read; no LLM call. Lock-poisoning recovers in-band the same
/// way `llm_cost_ledger` does.
#[tauri::command]
pub async fn llm_cost_timeline(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<crate::types_export::LlmCostTimelineEntryDto>, CommandError> {
    Ok(state
        .cost_ledger
        .timeline_snapshot()
        .into_iter()
        .map(crate::types_export::LlmCostTimelineEntryDto::from_typed)
        .collect())
}

// ---------------------------------------------------------------------------
// Command 18 — classifier_prompt_version (Session 77)
// ---------------------------------------------------------------------------

/// Return the version string of the classifier prompt currently
/// loaded in the binary. The frontend compares this against the
/// `@version` suffix parsed off each plan's `classified_by` field to
/// decide whether to render a "re-classify" banner.
///
/// Pure read of a compile-time constant; no LLM call, no fetch, no
/// DB. The constant
/// [`situation_room_pipeline::research_classifier::CLASSIFIER_PROMPT_VERSION`]
/// is the single source of truth — bumping it in pipeline cascades
/// to this command, to `format_classifier_id`, and (via the matching
/// `### Changelog` entry in `config/prompts/research_classifier.md`)
/// to the loaded prompt. The three move together; if they drift, the
/// banner fires on every plan or on no plan.
#[tauri::command]
pub async fn classifier_prompt_version(
) -> Result<crate::types_export::ClassifierPromptVersionDto, CommandError> {
    Ok(crate::types_export::ClassifierPromptVersionDto {
        current: CLASSIFIER_PROMPT_VERSION.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn classifier_prompt_version_matches_pipeline_constant() {
        // The wire surface must surface the same constant the pipeline
        // crate stamps onto `classified_by`. Drift between them is the
        // failure mode that would make every plan look "stale" (or
        // none of them, depending on the direction).
        let dto = classifier_prompt_version().await.unwrap();
        assert_eq!(dto.current, CLASSIFIER_PROMPT_VERSION);
    }

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

    /// Session 66: the new `ReauthorDeclined` variant. Mirrors the
    /// ReauthorFailed test on the real `CommandError` enum (the DTO
    /// shadow has its own test in types_export.rs). The frontend
    /// distinguishes the two via `kind`; this test pins the wire
    /// contract on the Rust side. The reason field name is
    /// `reason` (not `message`) because architecturally a decline
    /// is not a failure-message — the LLM gave a structured answer,
    /// which is the LLM's reason.
    #[test]
    fn command_error_reauthor_declined_serializes_with_kind_and_reason() {
        let e = CommandError::ReauthorDeclined {
            prior_recipe_id: "019e20b5-3881-7502-93fb-dcfdeb9c8b20".into(),
            reason: "the source's actual markup doesn't admit my prior \
                     recipe's iterator selectors; no css_select fix is \
                     possible against these bytes."
                .into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"reauthor_declined""#));
        assert!(json.contains(r#""prior_recipe_id":"019e20b5-3881-7502-93fb-dcfdeb9c8b20""#));
        assert!(json.contains(r#""reason":"#));
        // Distinguishing field name from ReauthorFailed — a frontend
        // match on `error.message` would silently miss this variant
        // if we'd reused the same key.
        assert!(!json.contains(r#""message":"#), "got {json}");
    }

    // -----------------------------------------------------------------
    // Session 46 — build_expectation_coverage
    // -----------------------------------------------------------------

    use crate::types_export::{
        DocumentSourceEntryDto, EntityKindExpectationDto, EventTypeExpectationDto,
        MetricExpectationDto, RecordExpectationsDto, RelationKindExpectationDto, ResearchPlanDto,
    };

    fn coverage_plan_with_obs_metrics(names: &[&str]) -> ResearchPlanDto {
        let observation_metrics = names
            .iter()
            .map(|n| MetricExpectationDto {
                name: (*n).to_string(),
                rationale: format!("test rationale for {n}"),
                unit_hint: "t".into(),
            })
            .collect::<Vec<_>>();
        ResearchPlanDto {
            id: "019e0b21-525e-7013-9dbe-ca5416ca014b".into(),
            topic: "lithium global supply chain".into(),
            interpretation: "test plan".into(),
            topic_tags: vec!["lithium".into()],
            geographic_scope: vec![],
            historical_window_days: 730,
            expectations: RecordExpectationsDto {
                observation_metrics,
                event_types: vec![EventTypeExpectationDto {
                    event_type: "mine_opened".into(),
                    rationale: "test".into(),
                }],
                entity_kinds: vec![EntityKindExpectationDto {
                    kind: "company".into(),
                    rationale: "test".into(),
                    exemplars: vec![],
                    attributes: vec![],
                }],
                relation_kinds: vec![RelationKindExpectationDto {
                    kind: "supplies_to".into(),
                    exemplar_triples: vec![],
                    rationale: "test".into(),
                }],
                document_sources: Vec::<DocumentSourceEntryDto>::new(),
                assertion_guidance: None,
            },
            status: PlanStatusDto::Accepted,
            created_at: chrono::Utc::now(),
            rejection_reason: String::new(),
            reclassified_from: String::new(),
            classified_by: String::new(),
        }
    }

    fn coverage_recipe_targeting(
        plan_id: uuid::Uuid,
        bucket: &str,
        index: u32,
        source_id: &str,
        record_type: &str,
    ) -> situation_room_storage::StoredRecipe {
        let produces = serde_json::json!([{
            "record_type": record_type,
            "expectation": { "list": bucket, "index": index },
            "field_mappings": [
                { "path": "value", "source": { "kind": "extracted" } },
            ],
        }]);
        situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some(format!("{plan_id}:{source_id}:{bucket}:{index}")),
            plan_id,
            source_id: source_id.into(),
            source_url: format!("https://{source_id}/x"),
            extraction_json: r#"{"mode":"json_path","path":"$.value"}"#.into(),
            produces_json: produces.to_string(),
            authored_at: chrono::Utc::now(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            iterator: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    /// Plan declares 4 obs metrics; one recipe binds to index 0 only.
    /// Three rows surface as uncovered.
    #[test]
    fn coverage_marks_unbound_expectations_uncovered_session_46() {
        let plan = coverage_plan_with_obs_metrics(&[
            "production",
            "reserves",
            "refining_capacity",
            "spot_price",
        ]);
        let plan_id: uuid::Uuid = plan.id.parse().unwrap();
        let recipes = vec![coverage_recipe_targeting(
            plan_id,
            "observation_metric",
            0,
            "pubs.usgs.gov",
            "observation",
        )];

        let rows = build_expectation_coverage(&plan, &recipes);

        // Four obs rows + one event + one entity + one relation = 7.
        let obs_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.bucket == "observation_metric")
            .collect();
        assert_eq!(obs_rows.len(), 4);
        assert_eq!(obs_rows[0].label, "production");
        assert_eq!(obs_rows[0].recipes.len(), 1);
        assert_eq!(obs_rows[0].recipes[0].source_id, "pubs.usgs.gov");
        assert_eq!(obs_rows[0].recipes[0].record_type, "observation");

        for unbound in &obs_rows[1..] {
            assert!(
                unbound.recipes.is_empty(),
                "expected uncovered: {} (label {})",
                unbound.index,
                unbound.label
            );
        }

        // The other buckets are also uncovered (no recipes target
        // them); the matrix surfaces each row regardless.
        assert!(rows.iter().any(|r| r.bucket == "event_type"));
        assert!(rows.iter().any(|r| r.bucket == "entity_kind"));
        assert!(rows.iter().any(|r| r.bucket == "relation_kind"));
    }

    /// Multiple recipes targeting the same expectation surface as
    /// multiple chips on the same row.
    #[test]
    fn coverage_groups_multiple_recipes_under_one_row_session_46() {
        let plan = coverage_plan_with_obs_metrics(&["production"]);
        let plan_id: uuid::Uuid = plan.id.parse().unwrap();
        let recipes = vec![
            coverage_recipe_targeting(
                plan_id,
                "observation_metric",
                0,
                "pubs.usgs.gov",
                "observation",
            ),
            coverage_recipe_targeting(
                plan_id,
                "observation_metric",
                0,
                "industry.gov.au",
                "observation",
            ),
        ];

        let rows = build_expectation_coverage(&plan, &recipes);
        let prod_row = rows
            .iter()
            .find(|r| r.bucket == "observation_metric" && r.index == 0)
            .expect("production row present");
        assert_eq!(prod_row.recipes.len(), 2);
        let sources: std::collections::HashSet<&str> = prod_row
            .recipes
            .iter()
            .map(|c| c.source_id.as_str())
            .collect();
        assert!(sources.contains("pubs.usgs.gov"));
        assert!(sources.contains("industry.gov.au"));
    }

    /// Recipes whose `expectation.index` references a position the
    /// plan no longer declares surface as orphan rows with empty
    /// `label`. The matrix still includes them so the operator sees
    /// the inconsistency.
    #[test]
    fn coverage_surfaces_orphan_bindings_with_empty_label_session_46() {
        // Plan declares one obs metric; recipe references index 9.
        let plan = coverage_plan_with_obs_metrics(&["production"]);
        let plan_id: uuid::Uuid = plan.id.parse().unwrap();
        let recipes = vec![coverage_recipe_targeting(
            plan_id,
            "observation_metric",
            9,
            "rogue.example.com",
            "observation",
        )];

        let rows = build_expectation_coverage(&plan, &recipes);
        let orphan = rows
            .iter()
            .find(|r| r.bucket == "observation_metric" && r.index == 9)
            .expect("orphan row present");
        assert_eq!(orphan.label, "", "orphan rows have empty label");
        assert_eq!(orphan.recipes.len(), 1);
        assert_eq!(orphan.recipes[0].source_id, "rogue.example.com");
    }

    /// Recipes whose `produces_json` is malformed don't crash the
    /// matrix — they simply contribute no chips. Matches the
    /// parse-on-error fallback `RecipeDto::from_stored` already
    /// uses for the same column.
    #[test]
    fn coverage_skips_recipes_with_malformed_produces_json_session_46() {
        let plan = coverage_plan_with_obs_metrics(&["production"]);
        let plan_id: uuid::Uuid = plan.id.parse().unwrap();
        let mut bad_recipe = coverage_recipe_targeting(
            plan_id,
            "observation_metric",
            0,
            "broken.example.com",
            "observation",
        );
        bad_recipe.produces_json = "not valid json".into();

        let good_recipe = coverage_recipe_targeting(
            plan_id,
            "observation_metric",
            0,
            "pubs.usgs.gov",
            "observation",
        );

        let rows = build_expectation_coverage(&plan, &[bad_recipe, good_recipe]);
        let prod = rows
            .iter()
            .find(|r| r.bucket == "observation_metric" && r.index == 0)
            .unwrap();
        // Only the good recipe contributed a chip.
        assert_eq!(prod.recipes.len(), 1);
        assert_eq!(prod.recipes[0].source_id, "pubs.usgs.gov");
    }

    // -----------------------------------------------------------------
    // Session 48 — host-backoff state + sources-memory wire mapping
    // -----------------------------------------------------------------
    //
    // The commands themselves take `tauri::State<'_, AppState>`, which
    // can't be cheaply constructed in a unit test without a Tauri
    // `mock_builder` runtime. The mapping logic is the part worth
    // pinning: each command lifts a typed value into a wire DTO with
    // no other transformation. These tests exercise the mapping using
    // the same accessor / Store the commands use, then assert the DTOs
    // come out correctly. If a future refactor adds filtering or
    // aggregation to the commands, the tests would catch it; today
    // they're a regression net for "the wire shape is what the operator
    // / classifier expects."

    #[test]
    fn host_backoff_state_maps_snapshot_into_dtos_session_48() {
        // Two hosts: one with a Retry-After-honoring 429, one with a
        // timeout. The mapping order isn't load-bearing (HashMap
        // iteration is unspecified) so the test asserts on a per-host
        // lookup rather than a positional shape.
        let backoff = situation_room_pipeline::fetch_backoff::HostBackoff::new();
        backoff.record_rate_limited(
            "throttled.example.com",
            Some(std::time::Duration::from_secs(15)),
        );
        backoff.record_timeout("slow.example.com");

        let dtos: Vec<HostBackoffSnapshotDto> = backoff
            .snapshot()
            .into_iter()
            .map(HostBackoffSnapshotDto::from_typed)
            .collect();
        assert_eq!(dtos.len(), 2);

        let throttled = dtos
            .iter()
            .find(|d| d.host == "throttled.example.com")
            .expect("throttled host present");
        assert_eq!(throttled.consecutive_failures, 1);
        assert!(
            (14..=15).contains(&throttled.wait_seconds_remaining),
            "Retry-After honored verbatim (got {}s)",
            throttled.wait_seconds_remaining
        );

        let slow = dtos
            .iter()
            .find(|d| d.host == "slow.example.com")
            .expect("slow host present");
        assert_eq!(slow.consecutive_failures, 1);
        // First timeout schedule is ~1s; collapse to whole seconds
        // gives 0 or 1 depending on jitter. Both are valid.
        assert!(
            slow.wait_seconds_remaining <= 1,
            "first timeout produces ~1s wait (got {}s)",
            slow.wait_seconds_remaining
        );
    }

    #[test]
    fn host_backoff_state_empty_snapshot_yields_empty_vec_session_48() {
        let backoff = situation_room_pipeline::fetch_backoff::HostBackoff::new();
        let dtos: Vec<HostBackoffSnapshotDto> = backoff
            .snapshot()
            .into_iter()
            .map(HostBackoffSnapshotDto::from_typed)
            .collect();
        assert!(
            dtos.is_empty(),
            "fresh-boot snapshot is empty until the first signal"
        );
    }

    #[test]
    fn sources_memory_command_maps_storage_rows_into_dtos_session_48() {
        // Round-trip a single (URL, source_id) pair through the same
        // path the command uses. Ensures the DTO carries the renamed
        // wire fields (`url`, `last_succeeded_at`) and the storage
        // query's filtering (only-successes, recency-sorted) reaches
        // the wire intact.
        use chrono::TimeZone;
        use uuid::Uuid;

        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrate");

        // Plan
        let plan_id = Uuid::now_v7();
        let plan_row = situation_room_storage::ResearchPlanRow {
            id: plan_id,
            topic: "lithium supply chain".into(),
            interpretation: "test".into(),
            topic_tags_json: serde_json::to_string(&["lithium"]).unwrap(),
            geographic_scope_json: "[]".into(),
            historical_window_days: 730,
            expectations_json: "{}".into(),
            classified_by: "test".into(),
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
            status: situation_room_storage::PlanStatus::Accepted,
            rejection_reason: None,
            reclassified_from: None,
        };
        store
            .insert_research_plan(&plan_row)
            .expect("insert plan");

        // Recipe
        let recipe_id = Uuid::now_v7();
        let recipe_row = situation_room_storage::RecipeRow {
            id: recipe_id,
            dedup_key: Some(format!("{plan_id}:wb")),
            plan_id,
            source_id: "world_bank_indicators".into(),
            source_url: "https://api.worldbank.org/v2/foo".into(),
            extraction_json: r#"{"mode":"json_path","path":"$.value"}"#.into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            iterator: None,
        };
        store.insert_recipe(&recipe_row).expect("insert recipe");

        // Successful attempt — without this the HAVING clause filters
        // the row out (only-successes contract).
        let attempt_row = situation_room_storage::RecipeFetchAttemptRow {
            id: Uuid::now_v7(),
            recipe_id,
            run_id: Uuid::now_v7(),
            attempted_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
            succeeded: true,
            failure_message: None,
            bytes_excerpt: None,
            response_content_type: None,
        };
        store
            .insert_recipe_fetch_attempt(&attempt_row)
            .expect("insert attempt");

        // The command body, minus the tauri State wrapper.
        let typed = store
            .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
            .expect("sources_memory");
        let dtos: Vec<SourcesMemoryEntryDto> = typed
            .into_iter()
            .map(SourcesMemoryEntryDto::from_typed)
            .collect();

        assert_eq!(dtos.len(), 1);
        assert_eq!(dtos[0].url, "https://api.worldbank.org/v2/foo");
        assert_eq!(dtos[0].source_id, "world_bank_indicators");
        assert_eq!(dtos[0].successful_attempts, 1);
        assert_eq!(
            dtos[0].last_succeeded_at,
            chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()
        );
        assert_eq!(dtos[0].associated_topics, vec!["lithium".to_string()]);
    }

    #[test]
    fn sources_memory_command_empty_store_yields_empty_vec_session_48() {
        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrate");

        let typed = store
            .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
            .expect("sources_memory on fresh store");
        let dtos: Vec<SourcesMemoryEntryDto> = typed
            .into_iter()
            .map(SourcesMemoryEntryDto::from_typed)
            .collect();
        assert!(
            dtos.is_empty(),
            "fresh installation has no successful sources to surface"
        );
    }

    #[test]
    fn sources_memory_command_filters_to_successes_only_session_48() {
        // Same posture as the storage layer's
        // `returns_only_sources_with_at_least_one_success` test, but
        // exercising the wire-DTO mapping the command performs. A
        // recipe with only failed attempts must not surface to the
        // operator panel — same contract the classifier sees.
        use chrono::TimeZone;
        use uuid::Uuid;

        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrate");

        let plan_id = Uuid::now_v7();
        store
            .insert_research_plan(&situation_room_storage::ResearchPlanRow {
                id: plan_id,
                topic: "test".into(),
                interpretation: "test".into(),
                topic_tags_json: "[\"t\"]".into(),
                geographic_scope_json: "[]".into(),
                historical_window_days: 365,
                expectations_json: "{}".into(),
                classified_by: "test".into(),
                created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
                status: situation_room_storage::PlanStatus::Accepted,
                rejection_reason: None,
                reclassified_from: None,
            })
            .unwrap();
        let recipe_id = Uuid::now_v7();
        store
            .insert_recipe(&situation_room_storage::RecipeRow {
                id: recipe_id,
                dedup_key: Some(format!("{plan_id}:fail")),
                plan_id,
                source_id: "always_fails".into(),
                source_url: "https://broken.example.com/x".into(),
                extraction_json: r#"{"mode":"json_path","path":"$.value"}"#.into(),
                produces_json: "[]".into(),
                authored_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
                authored_by: "test".into(),
                version: 1,
                static_payload: None,
                authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
                prior_recipe_id: None,
                reauthor_reason: None,
                iterator: None,
            })
            .unwrap();
        store
            .insert_recipe_fetch_attempt(
                &situation_room_storage::RecipeFetchAttemptRow {
                    id: Uuid::now_v7(),
                    recipe_id,
                    run_id: Uuid::now_v7(),
                    attempted_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 1, 0, 0).unwrap(),
                    succeeded: false,
                    failure_message: Some("404".into()),
                    bytes_excerpt: None,
                    response_content_type: None,
                },
            )
            .unwrap();

        let dtos: Vec<SourcesMemoryEntryDto> = store
            .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
            .unwrap()
            .into_iter()
            .map(SourcesMemoryEntryDto::from_typed)
            .collect();
        assert!(
            dtos.is_empty(),
            "failed-only sources must not surface to the operator panel"
        );
    }
}
