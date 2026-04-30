//! xAI (Grok) provider — OpenAI-chat-compatible.
//!
//! xAI's public API exposes a `chat/completions` endpoint that mirrors the
//! OpenAI wire format. This adapter speaks that wire format directly
//! through [`situation_room_secure::SecureHttpClient`] rather than pulling in
//! an SDK, per ADR 0009 (one HTTP client, one set of guards).
//!
//! ## What this provider does
//!
//! - Maps [`CompletionRequest`] → xAI JSON body (`model`, `messages`,
//!   `temperature`, `max_tokens`, optional `response_format`).
//! - Maps the structured-output schema (a JSON Schema `Value`) into xAI's
//!   `response_format: { type: "json_schema", json_schema: { name, strict,
//!   schema } }`.
//! - Parses the response, pulls out the assistant text and — if a schema
//!   was requested — the text parsed as JSON.
//!
//! ## What this provider does NOT do
//!
//! - Validate the response against the provided schema. That is the
//!   extraction layer's job (Phase 3c.2+). We only surface parse errors.
//! - Retry on 429. The router / extraction layer decides retry policy.
//! - Cache. The `cache` module is the right home; this provider is
//!   stateless on purpose.
//!
//! ## Model names
//!
//! xAI has rotated model identifiers before and will again. We do not
//! hardcode a single model name in this file — defaults live in
//! [`XaiConfig`] with a comment pointing at the docs, and any caller can
//! override per-tier via [`XaiProvider::with_config`]. A wrong default is
//! a config edit, not a code fix.
//!
//! See <https://docs.x.ai/api> for the current model catalog.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use situation_room_secure::bounds::{check_string, Bounds};
use situation_room_secure::http::{HttpError, SecureHttpClient};
use situation_room_secure::secrets::ApiKey;

use crate::providers::trait_def::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ModelTier,
};

/// Environment variable the provider reads its key from.
pub const XAI_API_KEY_ENV: &str = "XAI_API_KEY";

/// xAI chat completions endpoint. Overridable (only) for tests.
const DEFAULT_ENDPOINT: &str = "https://api.x.ai/v1/chat/completions";

/// Which model name to use for each tier.
///
/// Defaults are sensible at the time of writing but are not load-bearing
/// — the xAI catalog drifts, and any running binary should be able to
/// override these via config without a recompile.
#[derive(Debug, Clone)]
pub struct XaiConfig {
    pub frontier_model: String,
    pub workhorse_model: String,
    pub cheap_model: String,
}

impl Default for XaiConfig {
    fn default() -> Self {
        // Defaults confirmed against console.x.ai on 2026-04-22.
        // Frontier: the reasoning flagship. Cheap (and current workhorse
        // placeholder): the fast reasoning model — the default everyday
        // tier. The workhorse slot intentionally mirrors cheap until a
        // distinct mid-tier model is named; do not guess at one.
        // See https://docs.x.ai/api for the live catalog.
        Self {
            frontier_model: "grok-4.20-0309-reasoning".to_string(),
            workhorse_model: "grok-4-1-fast-reasoning".to_string(),
            cheap_model: "grok-4-1-fast-reasoning".to_string(),
        }
    }
}

impl XaiConfig {
    pub fn model_for(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Frontier => &self.frontier_model,
            ModelTier::Workhorse => &self.workhorse_model,
            ModelTier::Cheap => &self.cheap_model,
        }
    }
}

/// xAI provider. Holds an API key and a reference-counted HTTP client.
///
/// Clone is cheap: [`SecureHttpClient`] wraps an `Arc` internally, and
/// [`ApiKey`] wraps [`secrecy::SecretString`] which is `Clone` by design.
pub struct XaiProvider {
    http: SecureHttpClient,
    key: ApiKey,
    config: XaiConfig,
    endpoint: String,
}

impl XaiProvider {
    /// Construct from an already-loaded key and shared HTTP client.
    pub fn new(http: SecureHttpClient, key: ApiKey) -> Self {
        Self {
            http,
            key,
            config: XaiConfig::default(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
        }
    }

    /// Load the key from `XAI_API_KEY`. Returns `None` if unset / empty /
    /// placeholder — lets the caller fall back to another provider.
    pub fn from_env(http: SecureHttpClient) -> Option<Self> {
        ApiKey::from_env_optional(XAI_API_KEY_ENV).map(|k| Self::new(http, k))
    }

    /// Override the tier → model mapping.
    pub fn with_config(mut self, config: XaiConfig) -> Self {
        self.config = config;
        self
    }

    /// Test-only: override the endpoint URL. Not exposed outside tests so
    /// production code cannot accidentally point at a non-xAI host.
    #[cfg(test)]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Build the JSON request body for a given tier + request. Pure —
    /// factored out so tests can assert the wire shape without a network.
    fn build_body(&self, tier: ModelTier, req: &CompletionRequest) -> Value {
        let mut messages: Vec<Value> = Vec::with_capacity(2);
        if let Some(system) = &req.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.push(json!({ "role": "user", "content": req.user }));

        let mut body = json!({
            "model": self.config.model_for(tier),
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });

        if let Some(schema) = &req.schema {
            // xAI / OpenAI-compatible structured output.
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": schema.name,
                    "strict": true,
                    "schema": schema.schema,
                }
            });
        }

        body
    }

    /// Parse an xAI response body into a [`CompletionResponse`].
    fn parse_response(
        &self,
        raw: Value,
        schema_requested: bool,
    ) -> Result<CompletionResponse, LlmError> {
        // Deserialize via a private shape type so we get clear errors
        // rather than index-chasing on Value.
        let parsed: XaiChatResponse = serde_json::from_value(raw)
            .map_err(|e| LlmError::Api(format!("unexpected response shape: {e}")))?;

        let choice = parsed.choices.into_iter().next().ok_or_else(|| {
            LlmError::Api("response contained no choices".to_string())
        })?;

        let text = choice.message.content.unwrap_or_default();

        let structured = if schema_requested {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => Some(v),
                Err(e) => return Err(LlmError::JsonParse(e.to_string())),
            }
        } else {
            None
        };

        Ok(CompletionResponse {
            text,
            structured,
            provider: "xai".to_string(),
            model: parsed.model.unwrap_or_default(),
            input_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
            output_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
        })
    }
}

#[async_trait]
impl LlmProvider for XaiProvider {
    fn id(&self) -> &'static str {
        "xai"
    }

    fn supported_tiers(&self) -> &[ModelTier] {
        // xAI covers all three tiers via distinct model names.
        &[ModelTier::Frontier, ModelTier::Workhorse, ModelTier::Cheap]
    }

    async fn complete(
        &self,
        tier: ModelTier,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        // Enforce the LLM prompt size bound before sending.
        if let Some(system) = &request.system {
            check_string("llm_prompt_system", system, Bounds::LLM_PROMPT_BODY)
                .map_err(|e| LlmError::Api(e.to_string()))?;
        }
        check_string("llm_prompt_user", &request.user, Bounds::LLM_PROMPT_BODY)
            .map_err(|e| LlmError::Api(e.to_string()))?;

        let schema_requested = request.schema.is_some();
        let body = self.build_body(tier, &request);

        let bearer = format!("Bearer {}", self.key.expose_secret());
        // Wrap the bearer in a SecretString so expose_secret is only
        // called once, inside SecureHttpClient::post_json_bytes.
        let bearer_secret = situation_room_secure::secrets::SecretString::new(bearer);

        tracing::debug!(
            tier = ?tier,
            model = %self.config.model_for(tier),
            structured = schema_requested,
            "xai: sending completion"
        );

        let raw: Value = self
            .http
            .post_json(
                &self.endpoint,
                &body,
                &[("authorization", &bearer_secret)],
                // No `content-type` here — `SecureHttpClient::post_json_bytes`
                // calls `.json(body)` on the reqwest builder, which already
                // sets `Content-Type: application/json`. Adding it again as
                // an extra header makes reqwest *append* a second
                // Content-Type to the wire, and xAI's API gateway returns
                // `415 Unsupported Media Type` when it sees two of them.
                // See `SecureHttpClient::post_json_bytes` for the rule.
                &[],
            )
            .await
            .map_err(map_http_err)?;

        self.parse_response(raw, schema_requested)
    }
}

fn map_http_err(e: HttpError) -> LlmError {
    match e {
        HttpError::Status(401) | HttpError::Status(403) => LlmError::Auth,
        HttpError::Status(429) => LlmError::RateLimited {
            // xAI returns retry-after in headers; SecureHttpClient doesn't
            // surface response headers today. Report 0 as "unknown" — the
            // router can apply its own backoff.
            retry_after_seconds: 0,
        },
        HttpError::Status(code) => LlmError::Api(format!("http {code}")),
        HttpError::Timeout(d) => LlmError::Network(format!("timeout after {d:?}")),
        HttpError::ResponseTooLarge { max, got } => {
            LlmError::Api(format!("response exceeded bound: {got} > {max}"))
        }
        HttpError::Request(m) | HttpError::Tls(m) | HttpError::RedirectRejected(m) => {
            LlmError::Network(m)
        }
        HttpError::UrlRejected(v) => LlmError::Network(v.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Private wire shapes — kept minimal and forgiving. Any extra fields the
// API adds in the future are ignored.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct XaiChatResponse {
    #[serde(default)]
    model: Option<String>,
    choices: Vec<XaiChatChoice>,
    #[serde(default)]
    usage: Option<XaiUsage>,
}

#[derive(Debug, Deserialize)]
struct XaiChatChoice {
    message: XaiChatMessage,
}

#[derive(Debug, Deserialize)]
struct XaiChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XaiUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::trait_def::StructuredOutputSchema;
    use situation_room_secure::http::SecureHttpConfig;

    fn test_http() -> SecureHttpClient {
        SecureHttpClient::new(SecureHttpConfig::default()).unwrap()
    }

    fn fake_key() -> ApiKey {
        // ApiKey::from_env enforces a >=16 char minimum; we must set an
        // env var and use from_env. We use a test-scoped var name.
        std::env::set_var("TEST_XAI_KEY_FOR_UNIT_TESTS", "sk-fake-test-key-1234567890");
        ApiKey::from_env("TEST_XAI_KEY_FOR_UNIT_TESTS").unwrap()
    }

    fn test_provider() -> XaiProvider {
        XaiProvider::new(test_http(), fake_key())
    }

    #[test]
    fn build_body_has_expected_shape_for_plain_completion() {
        let p = test_provider();
        let req = CompletionRequest {
            system: Some("you are a helpful assistant".into()),
            user: "what is 2+2?".into(),
            schema: None,
            max_tokens: 64,
            // 0.5 is exactly representable in both f32 and f64, so the
            // serialized JSON number roundtrips cleanly. Fractional
            // values like 0.1 or 0.3 differ between f32 and f64
            // representations and would cause a spurious mismatch.
            temperature: 0.5,
        };
        let body = p.build_body(ModelTier::Cheap, &req);

        assert_eq!(body["model"], json!(p.config.cheap_model));
        assert_eq!(body["max_tokens"], json!(64));
        assert_eq!(body["temperature"], json!(0.5));

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[0]["content"], json!("you are a helpful assistant"));
        assert_eq!(messages[1]["role"], json!("user"));
        assert_eq!(messages[1]["content"], json!("what is 2+2?"));

        // No schema => no response_format on the body.
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn build_body_without_system_omits_system_message() {
        let p = test_provider();
        let req = CompletionRequest {
            system: None,
            user: "hello".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
        };
        let body = p.build_body(ModelTier::Workhorse, &req);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], json!("user"));
    }

    #[test]
    fn build_body_emits_structured_output_block_when_schema_set() {
        let p = test_provider();
        let schema = StructuredOutputSchema {
            name: "Answer".into(),
            schema: json!({
                "type": "object",
                "properties": { "answer": { "type": "string" } },
                "required": ["answer"],
                "additionalProperties": false,
            }),
        };
        let req = CompletionRequest {
            system: None,
            user: "give me a json answer".into(),
            schema: Some(schema),
            max_tokens: 128,
            temperature: 0.0,
        };
        let body = p.build_body(ModelTier::Frontier, &req);

        let rf = &body["response_format"];
        assert_eq!(rf["type"], json!("json_schema"));
        assert_eq!(rf["json_schema"]["name"], json!("Answer"));
        assert_eq!(rf["json_schema"]["strict"], json!(true));
        // The actual schema Value is passed through unchanged.
        assert_eq!(
            rf["json_schema"]["schema"]["properties"]["answer"]["type"],
            json!("string")
        );
    }

    #[test]
    fn supported_tiers_covers_all_three() {
        let p = test_provider();
        let tiers = p.supported_tiers();
        assert!(tiers.contains(&ModelTier::Frontier));
        assert!(tiers.contains(&ModelTier::Workhorse));
        assert!(tiers.contains(&ModelTier::Cheap));
    }

    #[test]
    fn model_for_maps_each_tier_to_its_configured_name() {
        let cfg = XaiConfig {
            frontier_model: "f".into(),
            workhorse_model: "w".into(),
            cheap_model: "c".into(),
        };
        assert_eq!(cfg.model_for(ModelTier::Frontier), "f");
        assert_eq!(cfg.model_for(ModelTier::Workhorse), "w");
        assert_eq!(cfg.model_for(ModelTier::Cheap), "c");
    }

    #[test]
    fn parse_response_extracts_text_and_usage() {
        let p = test_provider();
        // A response the shape of a real xAI chat completion.
        let raw = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "grok-4-1-fast-reasoning",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "4" }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 1,
                "total_tokens": 11
            }
        });
        let resp = p.parse_response(raw, false).unwrap();
        assert_eq!(resp.text, "4");
        assert!(resp.structured.is_none());
        assert_eq!(resp.provider, "xai");
        assert_eq!(resp.model, "grok-4-1-fast-reasoning");
        assert_eq!(resp.input_tokens, Some(10));
        assert_eq!(resp.output_tokens, Some(1));
    }

    #[test]
    fn parse_response_returns_structured_when_schema_was_requested() {
        let p = test_provider();
        let raw = json!({
            "model": "grok-4-1-fast-reasoning",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "{\"answer\":\"four\"}"
                }
            }]
        });
        let resp = p.parse_response(raw, true).unwrap();
        assert_eq!(resp.text, "{\"answer\":\"four\"}");
        let s = resp.structured.unwrap();
        assert_eq!(s["answer"], json!("four"));
    }

    #[test]
    fn parse_response_errors_when_structured_requested_but_content_not_json() {
        let p = test_provider();
        let raw = json!({
            "model": "grok-4-1-fast-reasoning",
            "choices": [{
                "message": { "role": "assistant", "content": "not json at all" }
            }]
        });
        let err = p.parse_response(raw, true).unwrap_err();
        assert!(matches!(err, LlmError::JsonParse(_)), "got {err:?}");
    }

    #[test]
    fn parse_response_errors_when_no_choices() {
        let p = test_provider();
        let raw = json!({ "model": "grok-4-1-fast-reasoning", "choices": [] });
        let err = p.parse_response(raw, false).unwrap_err();
        assert!(matches!(err, LlmError::Api(_)), "got {err:?}");
    }

    #[test]
    fn provider_id_is_stable() {
        assert_eq!(test_provider().id(), "xai");
    }

    // Live test — hits real xAI. Ignored by default. Run with:
    //   cargo test -p situation_room-llm --ignored live_xai
    //
    // `.env` is loaded automatically: put `XAI_API_KEY=...` in a
    // `.env` file at the workspace root and it'll be picked up.
    // (You should never have to paste the key anywhere.)
    #[tokio::test]
    #[ignore]
    async fn live_xai_returns_nonempty_completion() {
        let _ = dotenvy::dotenv();
        let http = test_http();
        let Some(provider) = XaiProvider::from_env(http) else {
            panic!(
                "XAI_API_KEY not set, empty, placeholder, or too short in env or .env — cannot run live test"
            );
        };
        let req = CompletionRequest {
            system: Some("Reply with a single digit only.".into()),
            user: "What is 2+2?".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
        };
        let resp = provider
            .complete(ModelTier::Cheap, req)
            .await
            .expect("live xai completion should succeed");
        assert!(!resp.text.is_empty(), "response text should not be empty");
        assert!(!resp.model.is_empty(), "response should name the model used");
    }

    #[tokio::test]
    #[ignore]
    async fn live_xai_returns_structured_json_when_schema_requested() {
        let _ = dotenvy::dotenv();
        let http = test_http();
        let Some(provider) = XaiProvider::from_env(http) else {
            panic!("XAI_API_KEY not set in environment or .env — cannot run live test");
        };
        let schema = StructuredOutputSchema {
            name: "Addition".into(),
            schema: json!({
                "type": "object",
                "properties": { "result": { "type": "integer" } },
                "required": ["result"],
                "additionalProperties": false,
            }),
        };
        let req = CompletionRequest {
            system: Some("Answer using the provided JSON schema.".into()),
            user: "What is 2+2?".into(),
            schema: Some(schema),
            max_tokens: 64,
            temperature: 0.0,
        };
        let resp = provider
            .complete(ModelTier::Workhorse, req)
            .await
            .expect("live xai structured completion should succeed");
        let structured = resp.structured.expect("structured field should be populated");
        assert!(structured.get("result").is_some(), "expected result field, got {structured}");
    }
}
