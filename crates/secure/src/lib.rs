//! # stockpile-secure
//!
//! Cross-cutting security primitives. Every other crate depends on this rather
//! than hand-rolling its own security-adjacent code.
//!
//! ## What's here and why
//!
//! - [`secrets`] — wrapping API keys in a type that scrubs from Debug,
//!   zeroizes on drop, and never `Serialize`s by accident.
//! - [`http`] — the one HTTP client Stockpile uses, with SSRF guardrails,
//!   strict TLS, bounded response sizes, timeouts, and redirect whitelisting.
//! - [`url_guard`] — URL validation that rejects private IP ranges, localhost,
//!   cloud metadata endpoints, and non-HTTP(S) schemes.
//! - [`logging`] — a tracing subscriber wrapper that scrubs known secret
//!   patterns from every log line before they hit stdout or disk.
//! - [`bounds`] — size limits for deserialization, strings, collections.
//!   Used by config loaders, source responses, and LLM outputs.
//! - [`fs_guard`] — filesystem helpers that reject path traversal and
//!   restrict writes to a designated workspace root.
//!
//! ## Threat model
//!
//! See `docs/security/threat_model.md`. In short: we defend against API-key
//! leakage, prompt injection (at the extraction layer, not here), SSRF via
//! user-supplied URLs, malicious content in ingested feeds, and supply-chain
//! tampering. We do *not* defend against a user-level malware infection on
//! the host (out of scope for a desktop app).

pub mod secrets;
pub mod http;
pub mod url_guard;
pub mod logging;
pub mod bounds;
pub mod fs_guard;

pub use secrets::{ApiKey, SecretString};
pub use http::{SecureHttpClient, SecureHttpConfig};
pub use url_guard::{UrlGuard, UrlViolation};
pub use bounds::{Bounds, BoundsViolation};
pub use fs_guard::{FsGuard, FsViolation};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("url rejected: {0}")]
    UrlRejected(#[from] UrlViolation),
    #[error("filesystem access rejected: {0}")]
    FsRejected(#[from] FsViolation),
    #[error("input exceeded bounds: {0}")]
    BoundsExceeded(#[from] BoundsViolation),
    #[error("http error: {0}")]
    Http(String),
    #[error("config error: {0}")]
    Config(String),
}
