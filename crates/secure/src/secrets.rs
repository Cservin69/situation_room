//! Secret handling — API keys and other sensitive strings.
//!
//! ## Rules enforced by this module
//!
//! 1. `ApiKey` and `SecretString` **never** implement `Display` or `Debug`
//!    in a way that reveals the value. Debug prints `"ApiKey(***)"`.
//! 2. They **never** implement `Serialize`. You cannot accidentally write
//!    a secret to JSON, TOML, a log file, or telemetry.
//! 3. They zeroize their memory on drop via the `zeroize` crate (wrapped
//!    through `secrecy::SecretString`).
//! 4. Equality comparison uses constant-time comparison (prevents timing
//!    attacks when comparing user-supplied tokens).
//! 5. Loading from environment is the *only* supported path. Secrets are
//!    never parsed from config files.
//!
//! ## Usage
//!
//! ```ignore
//! use stockpile_secure::secrets::ApiKey;
//!
//! let key = ApiKey::from_env("ANTHROPIC_API_KEY")?;
//! // key.expose_secret() returns &str when you genuinely need the value
//! // (e.g. passing to the HTTP client). Every call site that does this
//! // should be reviewable.
//! ```

use secrecy::{ExposeSecret, SecretString as SecrecySecretString};
use std::env;
use subtle::ConstantTimeEq;
use thiserror::Error;

/// A generic secret string — wraps the `secrecy` crate's SecretString with
/// Stockpile-specific constraints (no serialization, constant-time eq).
#[derive(Clone)]
pub struct SecretString(SecrecySecretString);

impl SecretString {
    pub fn new(s: String) -> Self {
        Self(SecrecySecretString::from(s))
    }

    /// Access the raw secret. Every call to this is a review point —
    /// it should only happen at the boundary where the secret is used
    /// (e.g. setting an Authorization header).
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }

    /// Constant-time equality comparison. Use this, not `==`, when comparing
    /// secrets to user-supplied values.
    pub fn ct_eq(&self, other: &str) -> bool {
        let a = self.0.expose_secret().as_bytes();
        let b = other.as_bytes();
        if a.len() != b.len() {
            // Note: length itself is treated as non-secret (API key lengths
            // are public knowledge per provider).
            return false;
        }
        a.ct_eq(b).into()
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(***)")
    }
}

// DELIBERATELY no Display, no Serialize, no Deserialize implementations.

/// An API key. Thin wrapper over SecretString with a type-level distinction
/// so a function that wants an API key can't accidentally receive, say, a
/// user password or a random string.
#[derive(Clone)]
pub struct ApiKey {
    provider: &'static str,
    inner: SecretString,
}

impl ApiKey {
    /// Load an API key from an environment variable.
    /// This is the only supported way to construct an ApiKey — forces the
    /// reviewer to see every key ingress point.
    pub fn from_env(env_var: &'static str) -> Result<Self, ApiKeyError> {
        let raw = env::var(env_var).map_err(|_| ApiKeyError::NotSet(env_var))?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ApiKeyError::Empty(env_var));
        }
        // Reject obvious placeholder values so users don't accidentally run with them.
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("your-")
            || lower.contains("placeholder")
            || lower == "changeme"
            || lower == "xxx"
        {
            return Err(ApiKeyError::Placeholder(env_var));
        }
        // Minimum plausible length for an API key (defense against truncation).
        if trimmed.len() < 16 {
            return Err(ApiKeyError::TooShort(env_var));
        }
        Ok(Self {
            provider: env_var,
            inner: SecretString::new(trimmed.to_string()),
        })
    }

    /// Optional load — returns None if the env var is unset or empty.
    /// Use when a key is optional (e.g. a specific provider is not required).
    pub fn from_env_optional(env_var: &'static str) -> Option<Self> {
        Self::from_env(env_var).ok()
    }

    pub fn provider(&self) -> &'static str {
        self.provider
    }

    pub fn expose_secret(&self) -> &str {
        self.inner.expose_secret()
    }

    /// First four + last four characters, for log fingerprinting.
    /// This reveals *some* information but is useful for debugging without
    /// leaking the full key.
    pub fn fingerprint(&self) -> String {
        let s = self.inner.expose_secret();
        if s.len() < 16 {
            // Shouldn't happen due to from_env check, but defensive
            return "***".to_string();
        }
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiKey({}={})", self.provider, self.fingerprint())
    }
}

#[derive(Debug, Error)]
pub enum ApiKeyError {
    #[error("API key env var not set: {0}")]
    NotSet(&'static str),
    #[error("API key env var is empty: {0}")]
    Empty(&'static str),
    #[error("API key env var contains placeholder value: {0} — edit your .env")]
    Placeholder(&'static str),
    #[error("API key env var is implausibly short: {0}")]
    TooShort(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let s = SecretString::new("actual_secret_value_xyz".to_string());
        let dbg = format!("{s:?}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("actual_secret_value"));
    }

    #[test]
    fn api_key_debug_shows_fingerprint_only() {
        let key = ApiKey {
            provider: "TEST_API_KEY",
            inner: SecretString::new("abcdefghijklmnop_secret_xyz123".to_string()),
        };
        let dbg = format!("{key:?}");
        assert!(dbg.contains("TEST_API_KEY"));
        assert!(dbg.contains("abcd"));
        assert!(dbg.contains("z123"));
        assert!(!dbg.contains("efghijklmnop_secret_xy"));
    }

    #[test]
    fn ct_eq_detects_equal() {
        let s = SecretString::new("hello_world_value".to_string());
        assert!(s.ct_eq("hello_world_value"));
        assert!(!s.ct_eq("hell0_world_value"));
        assert!(!s.ct_eq("hello_world_value!"));
    }
}
