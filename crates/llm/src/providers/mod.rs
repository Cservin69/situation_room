//! LLM providers. Each concrete provider implements [`LlmProvider`].
//!
//! Routing happens in [`router`] — the runtime selects a provider based on
//! the task's required `ModelTier` and which providers are configured.

pub mod trait_def;
pub mod router;
pub mod anthropic;
pub mod openai;
pub mod grok;
pub mod gemini;

pub use trait_def::{LlmProvider, LlmError, ModelTier, ReasoningEffort, CompletionRequest, CompletionResponse, StructuredOutputSchema};
pub use router::ProviderRouter;
pub use grok::{XaiProvider, XaiConfig, XAI_API_KEY_ENV};
// Session 23 — Anthropic provider promoted from stub to real
// implementation. Re-exported alongside xAI so the apps' composition
// roots can pick either at boot via the `LLM_PROVIDER` env var.
pub use anthropic::{AnthropicProvider, AnthropicConfig, ANTHROPIC_API_KEY_ENV};
