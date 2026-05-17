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
// Session 69 — per-fetch Document synthesis. `recipe_apply::build_record`
// rejects `RecordType::Document` by design (Documents aren't field-
// mapped extractions; they're the raw fetched page). This module fills
// the Documents bucket from the executor side by synthesising one
// Document record per successful fetch — kind from MIME, body
// preview-capped, provenance routed so `records_for_plan` picks it up
// on the per-plan dashboard. Called from each `run_X_recipe` after
// `fetch_recipe_bytes` returns Ok.
pub mod document_synth;
// Session 76 — plan-accept-time Entity exemplar materialisation.
// The classifier already attaches `exemplars: Vec<EntityId>` to each
// `EntityKindExpectation` (concrete actors the topic revolves around).
// Pre-Session-76 those exemplars were dead weight — the recipe author
// prompt explicitly blocks `entity_kind` production bindings, pointing
// at a "registry lookup" path that never existed. This module promotes
// each exemplar to an `Entity` row at plan-accept time so the
// dashboard's Entities panel lights up before any fetching runs.
// Closed-vocabulary, no LLM calls, idempotent via the
// `entities.entity_id` UNIQUE constraint.
pub mod entity_synth;
// Session 77 — plan-accept-time Relation triple materialisation.
// Sibling of `entity_synth`. The classifier now emits optional
// `exemplar_triples: Vec<(EntityId, EntityId)>` on each
// `RelationKindExpectation`; this module promotes each
// `(plan, kind, from, to)` to a `Relation` row at plan-accept time so
// the dashboard's Relations panel lights up before any fetching runs.
// Closed-vocabulary, no LLM calls, idempotent via a deterministic
// `dedup_key` derived from `(plan_id, kind, from, to)`.
pub mod relation_synth;
// Session 70 / ADR 0009 amendment 2 — host-class-aware User-Agent
// policy. A closed `UaPolicy` enum mirrors the `FetchOutcomeClass`
// vocabulary in `fetch_classes`; each variant maps to one UA string
// constant. The policy boundary is structural — no host strings
// appear in this module, only in `fetch_classes::HOST_CLASS_OVERRIDES`
// which is empty until probe evidence justifies entries. Wires into
// `SecureHttpClient::get_with_headers_ua`, the per-request override
// path added in the same session.
pub mod ua_policies;
pub mod research_classifier;
pub mod research_plans_store;
// Session 82 — ADR 0004 pathway 1. The authoritative-source registry
// is the configuration-driven half of the assertion-promotion model
// (the consensus half lives in `promote.rs` since Session 81). When
// an Assertion's claimant matches an entry in
// `config/vocab/authoritative_sources.toml` for the right metric +
// topic, `promote.rs` fast-tracks it at N=1 instead of waiting for
// quorum.
pub mod authoritative;
// Session 84 — hot-reload wrapper around `AuthorityRegistry`. Watches
// `config/vocab/authoritative_sources.toml` for mtime changes and
// swaps the in-memory `Arc<AuthorityRegistry>` atomically. ADR 0021
// amendment: operator can tune `consensus_quorum` without restarting
// the desktop binary.
pub mod authoritative_live;
// Session 92 — operator-triggered re-extraction of relation Assertions
// from Documents already on disk. Lights up ADR 0023's multi-claimant
// prompt (v1.2) for the pre-Sn-91 article-Document pile that the
// executor's per-Document hook only touched once under v1.1 or earlier.
// Cost-bounded by article-kind Document count per plan; per-plan UI
// trigger (Session 92 Option 2 chose per-plan over per-Document).
pub mod reextract;
// Session 93 — apply-time index-page detector. Pre-apply check on
// HTML payloads: when bytes look like a topic/category listing
// rather than article prose, short-circuit selector evaluation and
// stamp the outcome with `FetchOutcomeClass::IndexPageDetected` so
// the proposer's next attempt routes through the v1.24 "follow-the-
// link" path. Closed-vocabulary throughout — no host strings;
// structural signals only.
pub mod index_page_detector;
// Session 93 — operator-triggered cull pass for boilerplate-shaped
// Assertions. Read-only sample listing + bounded DELETE pass, gated
// on a per-plan caller. Pairs with `index_page_detector`: an
// Assertion whose source Document scored Index is exactly the
// boilerplate the cull is designed to remove.
pub mod cull;
// Session 93 — relation-predicate vocabulary enforcement at extract
// time. Closes the cross-plan spillover gap (Tesla/Meta predicates
// in fed plan). The plan's classifier-declared `relation_kinds` set
// is the closed vocabulary for that plan's relations; anything
// outside the set is dropped at LLM-output validation with a
// warn-log so the operator sees the bound rather than silent loss.
pub mod relation_vocab;
