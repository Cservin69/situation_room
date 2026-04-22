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

pub use trait_def::{LlmProvider, LlmError, ModelTier, CompletionRequest, CompletionResponse, StructuredOutputSchema};
pub use router::ProviderRouter;
pub use grok::{XaiProvider, XaiConfig, XAI_API_KEY_ENV};
