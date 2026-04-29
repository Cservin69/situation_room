// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-llm
//!
//! LLM provider abstraction, prompt management, and structured-output
//! extraction. The LLM is treated as a *normalizer*, not a data source —
//! it routes and shapes information that the deterministic layer fetches,
//! and it never invents numbers.
//!
//! ## Phase 1 status
//!
//! Module structure and the [`LlmProvider`] trait are defined. Concrete
//! providers and the extraction pipeline land in Phase 3.

pub mod providers;
pub mod prompts;
pub mod extraction;
pub mod cache;

pub use providers::{LlmProvider, LlmError, ModelTier, CompletionRequest, CompletionResponse};
pub use providers::{XaiProvider, XaiConfig, XAI_API_KEY_ENV};

pub type Result<T> = std::result::Result<T, LlmError>;
