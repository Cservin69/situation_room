//! Size bounds for untrusted input.
//!
//! Untrusted JSON/TOML/YAML from the network or user files must always be
//! length-bounded before parsing. `serde_json` on a deeply nested JSON
//! document with 10k levels of nesting will stack-overflow; a multi-GB
//! document will OOM.
//!
//! This module is a thin set of constants + helpers that the rest of the
//! codebase references. When we say "a source's response is at most 32MB,"
//! that's [`Bounds::SOURCE_RESPONSE`]. One place to find them. One place
//! to change them.

use thiserror::Error;

pub struct Bounds;

impl Bounds {
    /// A single source fetch response body (applied in SecureHttpClient).
    pub const SOURCE_RESPONSE: usize = 32 * 1024 * 1024;

    /// A single LLM completion response body.
    pub const LLM_RESPONSE: usize = 4 * 1024 * 1024;

    /// Maximum bytes of text we'll send to the LLM as document context.
    /// (LLMs have their own context window but we set a lower ceiling to
    /// control cost and avoid accidental prompt-injection from huge docs.)
    pub const LLM_PROMPT_BODY: usize = 256 * 1024;

    /// A single config file (TOML, JSON).
    pub const CONFIG_FILE: usize = 1024 * 1024;

    /// A single user-typed research topic.
    pub const RESEARCH_TOPIC: usize = 2_000;

    /// A single URL.
    pub const URL: usize = 2_048;

    /// Maximum entries in a deserialized collection (Vec, HashMap).
    pub const COLLECTION_ENTRIES: usize = 100_000;

    /// Maximum JSON nesting depth.
    pub const JSON_DEPTH: usize = 128;
}

#[derive(Debug, Error)]
pub enum BoundsViolation {
    #[error("input exceeded {kind} limit: {got} > {max}")]
    TooLarge {
        kind: &'static str,
        got: usize,
        max: usize,
    },
    #[error("collection too deeply nested: depth {depth} > {max}")]
    TooDeep { depth: usize, max: usize },
}

/// Assert that a byte slice is within a named limit.
pub fn check_size(kind: &'static str, bytes: &[u8], max: usize) -> Result<(), BoundsViolation> {
    if bytes.len() > max {
        return Err(BoundsViolation::TooLarge {
            kind,
            got: bytes.len(),
            max,
        });
    }
    Ok(())
}

/// Assert that a string is within a named limit.
pub fn check_string(kind: &'static str, s: &str, max: usize) -> Result<(), BoundsViolation> {
    if s.len() > max {
        return Err(BoundsViolation::TooLarge {
            kind,
            got: s.len(),
            max,
        });
    }
    Ok(())
}
