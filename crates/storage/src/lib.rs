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
pub mod connection;
pub mod documents;
pub mod entities;
pub mod envelope_io;
pub mod events;
pub mod fetch_runs;
pub mod migrate;
pub mod observations;
pub mod queries;
pub mod record_dispatch;
pub mod recipes;
pub mod relations;
pub mod research_plans;

pub use connection::Store;
pub use fetch_runs::{FetchRunRow, StoredFetchRun};
pub use queries::TopicUsage;
pub use recipes::{RecipeRow, StoredRecipe};
pub use research_plans::{PlanStatus, ResearchPlanRow, StoredResearchPlan};

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
