// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-storage
//!
//! DuckDB persistence for Stockpile records. See ADR 0005 for why DuckDB.
//!
//! ## Phase 1 status
//!
//! Module stubs only. Connection management, query builder, and the ring-buffer
//! cache logic land in Phase 2.

pub mod connection;
pub mod query;
pub mod cache;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("duckdb error: {0}")]
    DuckDb(#[from] duckdb::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("migration failed: {0}")]
    Migration(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
