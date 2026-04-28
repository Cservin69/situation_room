//! Tauri commands — actions the frontend triggers.
//!
//! ## The three commands (Session 6 baseline)
//!
//! Per `STOCKPILE_HANDOFF_SESSION5.md` Priority 4, the GUI's Tauri
//! command surface is exactly these three:
//!
//! - [`classify`] — run Level-1 classification on a topic, persist
//!   the resulting plan, return it.
//! - [`list_recent_plans`] — list the most recent persisted plans
//!   without making any LLM call.
//! - [`get_plan`] — fetch one plan by id.
//!
//! All three are thin wrappers over functions that already exist in
//! `pipeline::research_classifier` and `pipeline::research_plans_store`.
//! The `apps/situation_room` CLI already wires the same calls; this
//! module is the IPC surface for the same pipeline.
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
use stockpile_pipeline::research_classifier::{
    classify_topic, ClassificationContext, ClassificationError,
    SourceDescriptor as PipelineSourceDescriptor, TopicUsage as ClassifierTopicUsage,
};
use stockpile_pipeline::research_plans_store::{
    load_research_plan, save_research_plan, ResearchPlanStoreError,
};
use stockpile_secure::bounds::{check_string, Bounds};
use stockpile_storage::{Store, StorageError};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::types_export::{PlanSummary, ResearchPlanDto, SourceDescriptorDto};

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
/// - the classifier prompt template (loaded from the workspace at
///   compile time via `include_str!` in the binary, then handed in
///   here so this crate stays filesystem-agnostic),
/// - the registered source descriptors (loaded from
///   `config/sources.toml` in the binary).
///
/// Topic-injection limit is a constant here rather than configuration:
/// the same number the CLI uses (30 topics). If the GUI later needs
/// configurability, lift it to a field.
pub struct AppState {
    pub store: Arc<Store>,
    pub provider: Arc<XaiProvider>,
    pub classifier_prompt: &'static str,
    pub sources: Vec<PipelineSourceDescriptor>,
}

impl AppState {
    pub const TOPICS_INJECTION_LIMIT: usize = 30;

    pub fn new(
        store: Arc<Store>,
        provider: Arc<XaiProvider>,
        classifier_prompt: &'static str,
        sources: Vec<PipelineSourceDescriptor>,
    ) -> Self {
        Self {
            store,
            provider,
            classifier_prompt,
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

    // 5. Marshal to the wire shape.
    Ok(ResearchPlanDto::from(plan))
}

// ---------------------------------------------------------------------------
// Command 2 — list_recent_plans
// ---------------------------------------------------------------------------

/// List the most recent persisted plans. Pure read; no LLM call.
///
/// Returns lightweight [`PlanSummary`] rows (id, topic, created_at,
/// bucket counts). The frontend uses these to render the listing and
/// invokes [`get_plan`] when the user opens one.
///
/// `limit` is clamped to a sane range (1 to 200) to bound the IPC
/// payload regardless of frontend bugs.
#[tauri::command]
pub async fn list_recent_plans(
    limit: usize,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<PlanSummary>, CommandError> {
    let clamped = limit.clamp(1, 200);

    let stored = state
        .store
        .recent_research_plans(clamped)
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
#[tauri::command]
pub async fn get_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResearchPlanDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    let plan = load_research_plan(state.store.as_ref(), parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    Ok(ResearchPlanDto::from(plan))
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
}
