//! Error type for the core crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid commodity code: {0}")]
    InvalidCommodity(String),

    #[error("invalid country code: {0}")]
    InvalidCountry(String),

    #[error("invalid unit: {0}")]
    InvalidUnit(String),

    #[error("schema validation failed: {0}")]
    SchemaValidation(String),
}
