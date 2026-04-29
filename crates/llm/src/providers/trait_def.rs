//! The [`LlmProvider`] trait — every LLM provider conforms to this.
//!
//! The interface is deliberately minimal. We only ask providers to do one
//! thing well: generate a completion against a prompt, optionally constrained
//! to a JSON schema. Everything else (caching, retry, prompt versioning,
//! validation) happens above this layer.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Tier of model required for a given task.
///
/// `Frontier` — top-end model (Claude Opus, GPT-class flagship, Gemini Pro).
///   Used for hard extractions: long filings, novel topics, research planning.
/// `Workhorse` — mid-tier model (Claude Sonnet, GPT mid). Default for
///   most extraction tasks where quality matters but cost does too.
/// `Cheap` — small fast model (Haiku, GPT-mini, Flash). Used for high-volume
///   tagging, classification, and routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTier {
    Frontier,
    Workhorse,
    Cheap,
}

/// JSON schema constraint for structured output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredOutputSchema {
    pub name: String,
    pub schema: Value,
}

/// One completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// System / instruction prompt.
    pub system: Option<String>,
    /// User-turn content.
    pub user: String,
    /// If Some, the provider must return JSON conforming to this schema.
    pub schema: Option<StructuredOutputSchema>,
    /// Maximum output tokens (provider may clamp).
    pub max_tokens: u32,
    /// Sampling temperature, 0.0 – 1.0. Use low values for extraction.
    pub temperature: f32,
}

/// One completion response.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub text: String,
    /// If `request.schema` was set, this is the parsed JSON.
    pub structured: Option<Value>,
    /// Provider id and model id used.
    pub provider: String,
    pub model: String,
    /// Token usage (best effort — providers report differently).
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("rate limited; retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("response did not validate against schema: {0}")]
    SchemaValidation(String),
    #[error("response could not be parsed as JSON: {0}")]
    JsonParse(String),
    #[error("authentication failed: check your API key")]
    Auth,
    #[error("network error: {0}")]
    Network(String),
}

/// Contract every concrete provider implements.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider identifier — "anthropic", "openai", "grok", "gemini".
    fn id(&self) -> &'static str;

    /// Which tiers this provider can serve.
    fn supported_tiers(&self) -> &[ModelTier];

    /// Run one completion at the given tier.
    async fn complete(
        &self,
        tier: ModelTier,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError>;
}
