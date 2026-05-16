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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelTier {
    Frontier,
    Workhorse,
    Cheap,
}

/// Reasoning intensity for models that expose it as a request parameter
/// (xAI's `grok-4.3`, OpenAI's o-series, …).
///
/// `Low` returns faster and burns fewer reasoning tokens; `High` lets
/// the model think longer for harder problems. The cost lever xAI now
/// exposes after the May 2026 lineup consolidation is exactly this
/// parameter — model-string swaps no longer differentiate cheap from
/// frontier on xAI's catalog (Session 42 patch 4).
///
/// **Per-tier defaults live on the provider config**, e.g.
/// [`crate::providers::grok::XaiConfig`]'s `frontier_effort` /
/// `workhorse_effort` / `cheap_effort` fields. Callers normally leave
/// [`CompletionRequest::reasoning_effort`] as `None` and let the
/// provider pick from its tier mapping. Setting it explicitly on a
/// [`CompletionRequest`] is for the rare callsite that needs to pin a
/// specific value across tiers (e.g. a test asserting wire shape).
///
/// **Do not encode source-specific routing here.** A rule like "if
/// URL host is X, use High" is the failure mode the operator has
/// caught more than once across sessions; per-tier mapping is fine,
/// per-source is not. The LLM is the only specialist that decides
/// what each source needs — we only decide what the *tier* should
/// cost-budget for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
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
    /// Optional per-call override of the provider's per-tier reasoning-
    /// intensity default. `None` (the normal path) means "use whatever
    /// the provider's per-tier mapping says for this tier" —
    /// configured on the provider, not the call site. `Some(e)` pins
    /// the request body's effort to `e` regardless of tier.
    ///
    /// Providers that don't currently expose a reasoning-intensity
    /// parameter on the wire (Anthropic Messages today, OpenAI for
    /// non-reasoning models, all current stubs) ignore this field
    /// rather than pretend to honor it. The xAI provider is the only
    /// one that maps it onto the wire today; see
    /// [`crate::providers::grok`] for the per-tier mapping and the
    /// body-field shape used.
    ///
    /// See [`ReasoningEffort`] for the principle on why per-source
    /// rules belong nowhere.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Session 80 — optional per-call hint for the provider's prompt
    /// cache. `Some(key)` routes the request to a cache shard distinct
    /// from the provider's default routing; `None` keeps the default.
    ///
    /// Use case: per-Document extraction calls (assertion / event /
    /// observation) carry different prompt templates from the
    /// classifier / recipe-author calls, so routing them to a separate
    /// cache shard improves hit rate on the extraction-only prefix
    /// without polluting the authoring shard. The extraction module
    /// sets this to its prompt id (e.g. `"document_assertions"`).
    ///
    /// xAI maps this to the `x-grok-conv-id` header, overriding the
    /// per-process conv-id from `XAI_CONV_ID`. Anthropic and the stub
    /// providers currently ignore the hint — they're free to map it
    /// onto provider-native cache controls in future sessions.
    #[doc(alias = "x-grok-conv-id")]
    pub prompt_cache_key: Option<String>,
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
    /// How many of the `input_tokens` were served from the
    /// provider's prompt cache. Session 74 surface — `Some(n)` when
    /// the provider reports a cached-token count on the completion
    /// usage block (xAI's `prompt_tokens_details.cached_tokens`,
    /// Anthropic's `usage.cache_read_input_tokens`); `None` when the
    /// provider doesn't report cache hits or didn't return a usage
    /// block.
    ///
    /// `0` means "the provider reported the field and it was zero"
    /// (cold prefix, no cache hit) — distinct from `None` ("we
    /// don't know"). The cost-by-tier dashboard tile and the
    /// eval-harness cache-hit ratio both depend on the
    /// `Some(0)` / `None` distinction; collapsing them would falsely
    /// inflate the "no cache support" denominator.
    pub cached_input_tokens: Option<u32>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("api error: {0}")]
    Api(String),
    /// Session 69 — `reason` projects the upstream API's structured
    /// 429 message ("daily quota exceeded", "model not in your tier",
    /// etc.) when the provider could extract one from the response
    /// body. The field is built by the constructor with the
    /// rendering prefix baked in: either an empty string (legacy
    /// path, no reason available, or 429 without a parseable JSON
    /// body) or a `": <message>"` prefix-and-message that Display
    /// can concatenate verbatim. Keeping the prefix in the data
    /// rather than the format string lets thiserror's `#[error]`
    /// stay boring (no conditional expressions) while preserving the
    /// pre-Session-69 wire form when reason is absent:
    /// `"rate limited; retry after 30s"` vs.
    /// `"rate limited; retry after 30s: daily quota exceeded"`.
    /// Constructors in providers should use
    /// [`render_rate_limit_reason`] to build the field consistently.
    #[error("rate limited; retry after {retry_after_seconds}s{reason}")]
    RateLimited {
        retry_after_seconds: u64,
        reason: String,
    },
    #[error("response did not validate against schema: {0}")]
    SchemaValidation(String),
    #[error("response could not be parsed as JSON: {0}")]
    JsonParse(String),
    #[error("authentication failed: check your API key")]
    Auth,
    #[error("network error: {0}")]
    Network(String),
}

/// Build the `reason` field for [`LlmError::RateLimited`] from a
/// 429 response body. Session 69.
///
/// Returns either an empty string (no usable reason — body empty,
/// not UTF-8, or unparseable) or a `": <message>"` form where the
/// leading `": "` is the in-data prefix that
/// [`LlmError::RateLimited`]'s Display concatenates verbatim. See
/// the field's doc comment for why the prefix lives in the data
/// rather than in the format string.
///
/// Extraction strategy, deliberately ordered cheap → expensive:
///
/// 1. Try `serde_json::from_slice` on the body. If it deserializes
///    to an object with `error.message` (OpenAI / xAI shape) or
///    `error` (Anthropic shape), use that field's string value.
/// 2. Fall through to a UTF-8 lossy projection of the body's first
///    `MAX_REASON_LEN_BYTES` bytes, with trailing whitespace
///    trimmed. Catches plain-text 429 bodies and edge cases where
///    the JSON parser doesn't find the expected shape but the body
///    still carries human-readable text.
/// 3. If after both attempts the result is empty, return the empty
///    string so Display falls back to the pre-Session-69 form.
///
/// The cap on extracted-reason length (`MAX_REASON_LEN_CHARS`)
/// keeps a runaway server response (e.g. a 16 KiB HTML 429 page
/// from a misbehaving proxy) from blowing up the error message
/// for the operator's terminal / toast.
pub fn render_rate_limit_reason(body: &[u8]) -> String {
    /// Body bytes to consider; aligns with the secure HTTP layer's
    /// per-response cap. Anything larger gets sliced; we don't try
    /// to parse a 16 KiB HTML error page as JSON. The final reason
    /// length is capped separately inside `format_reason` (see that
    /// helper's `MAX_REASON_LEN_CHARS`).
    const MAX_BODY_BYTES: usize = 4096;

    let view = if body.len() > MAX_BODY_BYTES {
        &body[..MAX_BODY_BYTES]
    } else {
        body
    };

    // 1. Try JSON projection. The OpenAI / xAI / Anthropic catalogs
    //    all use one of these shapes:
    //      {"error": {"message": "...", "code": "..."}}        // xAI / OpenAI
    //      {"error": "..."}                                    // legacy / Anthropic
    //      {"message": "..."}                                  // some proxies
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(view) {
        let candidate = v
            .get("error")
            .and_then(|e| e.get("message").and_then(|m| m.as_str()).or_else(|| e.as_str()))
            .or_else(|| v.get("message").and_then(|m| m.as_str()));
        if let Some(s) = candidate {
            return format_reason(s);
        }
    }

    // 2. Plain-text fallback. UTF-8-lossy so a misbehaving proxy
    //    that ships non-UTF-8 bytes still produces a readable line
    //    (with U+FFFD substitutions) rather than nothing.
    let lossy = String::from_utf8_lossy(view);
    let trimmed = lossy.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format_reason(trimmed)
}

/// Truncate to a sensible visual length, attach the `": "` prefix
/// so Display can concatenate. Caller has already determined the
/// reason is non-empty.
fn format_reason(raw: &str) -> String {
    /// Same cap as in `render_rate_limit_reason`. Keeping it here
    /// rather than threading through avoids a non-pub re-export
    /// just for this private helper.
    const MAX_REASON_LEN_CHARS: usize = 240;

    let mut out = String::with_capacity(raw.len().min(MAX_REASON_LEN_CHARS) + 2);
    out.push_str(": ");
    if raw.chars().count() <= MAX_REASON_LEN_CHARS {
        out.push_str(raw);
    } else {
        // Char-boundary-safe truncation so we never split a
        // multi-byte codepoint.
        for (i, c) in raw.chars().enumerate() {
            if i >= MAX_REASON_LEN_CHARS {
                break;
            }
            out.push(c);
        }
        out.push('…');
    }
    out
}

#[cfg(test)]
mod render_rate_limit_reason_tests {
    use super::*;

    #[test]
    fn empty_body_returns_empty_reason() {
        assert_eq!(render_rate_limit_reason(b""), "");
    }

    #[test]
    fn whitespace_only_body_returns_empty_reason() {
        assert_eq!(render_rate_limit_reason(b"   \n\t\n"), "");
    }

    #[test]
    fn xai_shape_picks_error_message() {
        let body = br#"{"error":{"message":"daily quota exceeded","code":"rate_limit_exceeded"}}"#;
        assert_eq!(render_rate_limit_reason(body), ": daily quota exceeded");
    }

    #[test]
    fn anthropic_shape_picks_error_string() {
        // Anthropic sometimes returns `{"error": "..."}` rather than
        // a nested object. Both shapes work.
        let body = br#"{"error":"overloaded; try a different region"}"#;
        assert_eq!(render_rate_limit_reason(body), ": overloaded; try a different region");
    }

    #[test]
    fn flat_message_shape_works() {
        let body = br#"{"message":"too many requests"}"#;
        assert_eq!(render_rate_limit_reason(body), ": too many requests");
    }

    #[test]
    fn plain_text_body_falls_through_to_lossy() {
        let body = b"rate limit exceeded; daily token budget for free tier";
        assert_eq!(
            render_rate_limit_reason(body),
            ": rate limit exceeded; daily token budget for free tier"
        );
    }

    #[test]
    fn unparseable_json_falls_back_to_text() {
        // Body looks JSON-ish but isn't valid; we still surface what
        // text we can.
        let body = b"{\"oops not really json";
        let out = render_rate_limit_reason(body);
        assert!(out.starts_with(": "), "should have the leading prefix: {out:?}");
        assert!(out.contains("oops"));
    }

    #[test]
    fn long_reason_is_truncated_with_ellipsis() {
        let long = "x".repeat(500);
        let body = format!(r#"{{"error":{{"message":"{long}"}}}}"#);
        let out = render_rate_limit_reason(body.as_bytes());
        assert!(out.ends_with('…'), "expected ellipsis suffix, got: {out:?}");
        // ": " + 240 chars + '…' = 243 chars worst-case
        assert!(out.chars().count() <= 243);
    }

    #[test]
    fn multibyte_truncation_does_not_split_codepoint() {
        // 500 copies of 'é' (U+00E9, 2 bytes). Truncation at 240
        // chars must land at a codepoint boundary; the U+FFFD
        // replacement char must NOT appear.
        let many = "é".repeat(500);
        let body = format!(r#"{{"error":{{"message":"{many}"}}}}"#);
        let out = render_rate_limit_reason(body.as_bytes());
        assert!(!out.contains('\u{FFFD}'), "truncation corrupted a codepoint: {out:?}");
    }

    #[test]
    fn renders_into_full_error_string() {
        // End-to-end: the rendered reason flows through Display.
        let body = br#"{"error":{"message":"daily quota exceeded"}}"#;
        let reason = render_rate_limit_reason(body);
        let err = LlmError::RateLimited {
            retry_after_seconds: 0,
            reason,
        };
        assert_eq!(
            err.to_string(),
            "rate limited; retry after 0s: daily quota exceeded"
        );
    }

    #[test]
    fn empty_reason_preserves_legacy_display_form() {
        let err = LlmError::RateLimited {
            retry_after_seconds: 30,
            reason: String::new(),
        };
        assert_eq!(err.to_string(), "rate limited; retry after 30s");
    }
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
