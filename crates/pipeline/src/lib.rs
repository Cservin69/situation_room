// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-pipeline
//!
//! Pipeline orchestration. This is the only crate that depends on most of
//! the others; it's the place where wiring lives so that everywhere else
//! stays decoupled.
//!
//! ## Lifecycle of a record
//!
//! 1. [`ingest`] runs sources, writes raw records to storage.
//! 2. [`normalize`] applies units, currencies, entity resolution.
//! 3. [`extract`] hands documents to `llm::extraction` to produce Assertions.
//! 4. [`promote`] turns multi-source-agreed Assertions into Observations/Events.
//!
//! ## The research planner
//!
//! [`research`] is the entry point for "research topic X" — it decomposes a
//! free-text topic into a structured research plan, matches it against
//! registered sources, and triggers targeted ingestion. This is what makes
//! Stockpile general-purpose rather than commodity-only.
//!
//! [`recipes`] holds the Level-2 output types ([`recipes::FetchRecipe`] et
//! al.) — deterministic instructions produced by the LLM at authoring time
//! that the runtime applies without further LLM involvement. See
//! `docs/adr/0007-research-function.md`.
//!
//! ## Phase 1 status
//!
//! Module structure declared. Implementations land in Phase 3.

pub mod ingest;
pub mod normalize;
pub mod extract;
pub mod fetch_executor;
pub mod http_fetcher;
pub mod promote;
pub mod recipes;
pub mod recipe_author;
pub mod recipe_apply;
pub mod recipes_store;
pub mod research;
pub mod research_classifier;
pub mod research_plans_store;
