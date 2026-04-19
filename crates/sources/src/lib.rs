// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-sources
//!
//! Data source adapters. Each source implements the [`Source`] trait and
//! lives in its own folder under `adapters/`.
//!
//! See `docs/sources/adding_a_source.md` for how to add a new one.
//!
//! ## Phase 1 status
//!
//! The `Source` trait is defined. Adapter folders exist as stubs.

pub mod traits;
pub mod scheduler;
pub mod registry;
pub mod adapters;

pub use traits::{Source, SourceError, SourceMetadata, FetchOutcome};

pub type Result<T> = std::result::Result<T, SourceError>;
