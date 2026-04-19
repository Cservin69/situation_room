//! Structured-output extraction.
//!
//! Takes a [`Document`](stockpile_core::schema::records::document::Document) plus a
//! prompt, asks the LLM to extract structured [`Assertion`](stockpile_core::schema::records::assertion::Assertion)s,
//! validates the response against a JSON schema, retries on failure, returns
//! typed assertions.
//!
//! Phase 3 implements the full extraction loop. Phase 1 declares the contract.

use crate::providers::{LlmError, ModelTier};
use serde::{Deserialize, Serialize};

/// Configuration for an extraction run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    pub prompt_id: String,
    pub tier: ModelTier,
    pub max_retries: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("validation failed after {attempts} attempts: {last_error}")]
    ValidationExhausted { attempts: u32, last_error: String },
}
