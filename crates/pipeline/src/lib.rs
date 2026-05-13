// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # situation_room-pipeline
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
//! situation_room general-purpose rather than commodity-only.
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
pub mod fetch_backoff;
// Session 57 / ADR 0017 Piece A — class-level vocabulary the URL
// proposer can reason about ("host_blocked_by_waf",
// "url_shape_mismatch", …) sitting one layer above
// `http_fetcher::FetchError`'s wire-shape vocabulary. Read-only:
// the module classifies, callers react. The host-override map
// inside the module is the single bake-in of host-specific
// knowledge in the codebase per ADR 0007's closed-vocabulary
// discipline; it stays empty until probe evidence justifies
// entries.
pub mod fetch_classes;
pub mod fetch_executor;
pub mod http_fetcher;
pub mod promote;
pub mod propose_source_url;
pub mod recipes;
pub mod recipe_author;
pub mod recipe_apply;
pub mod recipes_store;
pub mod research;
// Session 68 — runtime URL normalisation for OData-shaped paginated
// endpoints. Sits between the recipe's `source_url` and the HTTP
// fetch in `fetch_executor::fetch_recipe_bytes`, capping `$top` at
// the runtime record cap. Closes the FEMA-style "default page is
// 1000, cap is 500" failure shape Session 67's hunt surfaced.
pub mod url_pagination;
pub mod research_classifier;
pub mod research_plans_store;
