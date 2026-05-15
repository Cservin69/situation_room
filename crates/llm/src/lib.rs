// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # situation_room-llm
//!
//! LLM provider abstraction, prompt management, and structured-output
//! extraction. The LLM is treated as a *normalizer*, not a data source —
//! it routes and shapes information that the deterministic layer fetches,
//! and it never invents numbers.
//!
//! ## Provider catalog
//!
//! - [`XaiProvider`] (id `"xai"`) — Grok, OpenAI-chat-compatible wire format.
//! - [`AnthropicProvider`] (id `"anthropic"`) — Claude, Messages API
//!   with structured output via forced tool use.
//! - OpenAI and Gemini providers remain stubs (carried forward).
//!
//! Both real providers honour ADR 0009 §"The rule": HTTP goes through
//! [`situation_room_secure::SecureHttpClient`], no fresh
//! `reqwest::Client::new()`. The composition root in each app picks
//! one provider via the `LLM_PROVIDER` env var (default `"xai"`).
//!
//! ## Phase 1 status
//!
//! Module structure and the [`LlmProvider`] trait are defined.
//! Concrete xAI + Anthropic providers shipped; OpenAI / Gemini land
//! as future work.

pub mod providers;
pub mod prompts;
pub mod extraction;
pub mod cache;
pub mod cost_ledger;

pub use providers::{LlmProvider, LlmError, ModelTier, ReasoningEffort, CompletionRequest, CompletionResponse};
pub use providers::{XaiProvider, XaiConfig, XAI_API_KEY_ENV};
pub use providers::{AnthropicProvider, AnthropicConfig, ANTHROPIC_API_KEY_ENV};
pub use cost_ledger::{CostLedger, LedgerEntry, MeteredProvider, Tally};
pub use extraction::{
    extract_assertions_from_document, extract_events_from_document,
    extract_observations_from_document, AssertionDraft, EventDraft, ExtractionConfig,
    ExtractionError, ObservationDraft, RawExtractedAssertion, RawExtractedAssertions,
    RawExtractedEvent, RawExtractedEvents, RawExtractedObservation, RawExtractedObservations,
};

pub type Result<T> = std::result::Result<T, LlmError>;
