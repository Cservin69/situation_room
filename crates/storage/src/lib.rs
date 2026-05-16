//! # situation_room-storage
//!
//! DuckDB persistence for situation_room records. See ADR 0005 (DuckDB) and
//! ADR 0008 (offline / cache architecture).
//!
//! ## Phase 2e status
//!
//! Minimal end-to-end path: open a connection, apply migrations, insert
//! an Observation, query it back. Other record types and the cache /
//! archive distinction land in subsequent phases once the round-trip
//! shape is proven.

#![allow(dead_code)]

pub mod assertions;
pub mod authority_registry;
pub mod connection;
pub mod documents;
pub mod entities;
pub mod envelope_io;
pub mod events;
pub mod fetch_run_outcomes;
pub mod fetch_runs;
pub mod migrate;
pub mod observations;
pub mod promote_history;
pub mod queries;
pub mod recipe_feedback;
pub mod recipe_fetch_attempts;
pub mod record_dispatch;
pub mod recipes;
pub mod relations;
pub mod research_plans;
pub mod sources_memory;

pub use authority_registry::{AuthorityProvenance, AuthorityRegistryRow};
pub use connection::Store;
pub use fetch_run_outcomes::{
    ApplyFailureForProposer, FetchRunOutcomeKind, FetchRunOutcomeRow,
    RecipeOutcomeHistoryEntry, RecipeOutcomeHistoryRunRow, StoredFetchRunOutcome,
};
pub use fetch_runs::{FetchRunRow, StoredFetchRun};
pub use promote_history::{PromoteHistoryRow, TRIGGER_STRINGS as PROMOTE_HISTORY_TRIGGERS};
pub use queries::{RecordsByPlan, TopicUsage};
pub use recipe_feedback::{RecipeFeedbackRow, StoredRecipeFeedback};
pub use recipe_fetch_attempts::{
    truncate_excerpt, RecipeFetchAttemptRow, StoredRecipeFetchAttempt, MAX_EXCERPT_BYTES,
};
pub use recipes::{AuthoredFrom, RecipeRow, StoredRecipe, MAX_RECIPE_LINEAGE_DEPTH};
pub use research_plans::{PlanStatus, ResearchPlanRow, StoredResearchPlan};
pub use sources_memory::{MemorySource, SOURCES_MEMORY_LIMIT};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("duckdb error: {0}")]
    DuckDb(#[from] duckdb::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("record not found: {0}")]
    NotFound(String),

    #[error("storage error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
