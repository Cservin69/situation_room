//! Error type for the core crate.
//!
//! Consolidates validation errors from [`vocab`](crate::vocab) so consumers
//! can work with a single top-level error if they don't care about the
//! specific source of a validation failure.

use thiserror::Error;

use crate::vocab::{ConfidenceError, VocabError};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Vocab(#[from] VocabError),

    #[error(transparent)]
    Confidence(#[from] ConfidenceError),

    #[error("schema validation failed: {0}")]
    Schema(String),
}
