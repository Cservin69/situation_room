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
//! - **Retries once on response truncation** (Session 13 Improvement B).
//!   When a structured-output response parses as a top-level JSON string
//!   that ends mid-value (the gateway truncated the model's output before
//!   it could close a string literal), we retry the same request once
//!   with a larger `max_tokens` budget. Other JSON parse errors do not
//!   retry — bigger budget will not fix a malformed schema.
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
//! Three env vars (`XAI_FRONTIER_MODEL`, `XAI_WORKHORSE_MODEL`,
//! `XAI_CHEAP_MODEL`) override the per-tier defaults at startup. This
//! lets operators swap to a frontier model for a session — or pin to a
//! cheaper model — without recompiling. See [`XaiConfig::from_env`].
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
    ReasoningEffort,
};

/// Environment variable the provider reads its key from.
pub const XAI_API_KEY_ENV: &str = "XAI_API_KEY";

/// Environment variables for per-tier model overrides. Optional; if any
/// is unset (or set to an empty / whitespace-only string) the tier's
/// hardcoded default in [`XaiConfig::default`] is used. Added in
/// Session 13 to let operators swap to a frontier model for a session
/// (e.g. when a cheaper model truncates structured output too often)
/// without a code change.
pub const XAI_FRONTIER_MODEL_ENV: &str = "XAI_FRONTIER_MODEL";
pub const XAI_WORKHORSE_MODEL_ENV: &str = "XAI_WORKHORSE_MODEL";
pub const XAI_CHEAP_MODEL_ENV: &str = "XAI_CHEAP_MODEL";

/// Environment variables for per-tier reasoning-effort overrides.
/// Optional; if any is unset (or set to an empty / whitespace-only
/// string) the tier's hardcoded default in [`XaiConfig::default`] is
/// used. Same posture as the per-tier model overrides above. Added
/// in Session 43 to deliver cost-tier differentiation that the May
/// 2026 xAI lineup consolidation made unreachable through model
/// strings alone (Session 42 patch 4 — see comment on
/// [`XaiConfig::default`]).
///
/// Accepted values are `low`, `medium`, `high` (case-insensitive,
/// whitespace-trimmed). Anything else is treated as "absent" and the
/// default applies — a typo in shell scripting silently degrades to
/// the default rather than crashing the provider on a 400 from xAI.
pub const XAI_FRONTIER_EFFORT_ENV: &str = "XAI_FRONTIER_EFFORT";
pub const XAI_WORKHORSE_EFFORT_ENV: &str = "XAI_WORKHORSE_EFFORT";
pub const XAI_CHEAP_EFFORT_ENV: &str = "XAI_CHEAP_EFFORT";

/// xAI chat completions endpoint. Overridable (only) for tests.
const DEFAULT_ENDPOINT: &str = "https://api.x.ai/v1/chat/completions";

/// Token ceiling used by the truncation-retry path. When a structured-
/// output response truncates mid-string, the retry doubles the original
/// `max_tokens` and clamps to this value. Picked to be well above what
/// any of our prompts actually need (a fully-populated `ResearchPlan`
/// or `FetchRecipe` JSON object lands well under 16 KB of tokens) but
/// well below the gateway's hard ceiling, so the retry can never loop
/// forever even if the model genuinely can't finish.
const MAX_RETRY_TOKENS: u32 = 32_768;

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
    /// Per-tier reasoning intensity. xAI's chat/completions endpoint
    /// accepts a `reasoning_effort` parameter on each request; routing
    /// cheap-tier calls at low effort and frontier-tier calls at high
    /// effort delivers the cost differentiation a model-string swap
    /// can't on the post-consolidation catalog (Session 43,
    /// architectural follow-up to Session 42 patch 4).
    pub frontier_effort: ReasoningEffort,
    pub workhorse_effort: ReasoningEffort,
    pub cheap_effort: ReasoningEffort,
}

impl Default for XaiConfig {
    fn default() -> Self {
        // Defaults re-confirmed against the xAI catalog on 2026-05-08
        // (Session 42 item 7) and the reasoning-effort plumbing
        // landed in Session 43.
        //
        // xAI consolidated their lineup in May 2026: eight legacy
        // models — grok-4-fast, grok-4-0709, grok-3,
        // grok-code-fast-1, grok-imagine-image-pro, and the
        // grok-4-1-fast variants among others — retire on
        // 2026-05-15. The two models surviving the cutoff are:
        //
        // - `grok-4.3` — flagship, 1M context, $1.25/$2.50
        //   per-million tokens (doubling above 200 KiB). xAI's
        //   official recommendation for all callers.
        // - `grok-4.20` — long-context option, 2M context, $2/$6.
        //   *More* expensive than 4.3, not cheaper.
        //
        // There is no current xAI model string that cost-
        // differentiates a cheap tier from a frontier tier the way
        // our `ModelTier` enum implies. The cost lever xAI now
        // exposes is `grok-4.3`'s reasoning intensity (low/medium/
        // high) — a per-request parameter. Session 43 plumbs that
        // intensity through this struct (`*_effort`) and through
        // `CompletionRequest::reasoning_effort`; per-tier defaults
        // below give the cost differentiation that the model-string
        // map can't on this catalog.
        //
        // Defaults: frontier = High (authoring deserves the deep
        // think), workhorse = Medium (sane balance for everyday
        // extraction), cheap = Low (propose-URL and classification
        // run fast and cheap). All three model slots remain
        // `grok-4.3` because that is xAI's official recommendation;
        // the boot-log line
        //   `frontier=grok-4.3 workhorse=grok-4.3 cheap=grok-4.3`
        // is therefore correct, not config drift, and the per-tier
        // effort line beside it now carries the actual cost lever.
        //
        // The xAI catalog drifts; if a distinct cheap-tier model
        // appears (or the consolidation reverses), update these
        // strings — that is a config edit, not a code fix.
        // See https://docs.x.ai/developers/models for the live
        // catalog.
        Self {
            frontier_model: "grok-4.3".to_string(),
            workhorse_model: "grok-4.3".to_string(),
            cheap_model: "grok-4.3".to_string(),
            frontier_effort: ReasoningEffort::High,
            workhorse_effort: ReasoningEffort::Medium,
            cheap_effort: ReasoningEffort::Low,
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

    /// Per-tier reasoning intensity. Mirrors [`Self::model_for`] for
    /// the new cost-differentiation lever; see the [`XaiConfig`] doc
    /// comment for the architectural rationale.
    pub fn effort_for(&self, tier: ModelTier) -> ReasoningEffort {
        match tier {
            ModelTier::Frontier => self.frontier_effort,
            ModelTier::Workhorse => self.workhorse_effort,
            ModelTier::Cheap => self.cheap_effort,
        }
    }

    /// Build a config by reading the optional env vars and falling
    /// back to [`XaiConfig::default`] for any that are unset or
    /// empty/whitespace-only.
    ///
    /// Empty-string normalisation matches the `endpoint_hint` discipline
    /// from Session 10's TOML loaders: a blank string is "absent", not
    /// "use literal empty model name." A literal empty model name would
    /// be rejected by xAI with a 400, which is a worse failure mode than
    /// silently using the default.
    ///
    /// Effort values follow the same posture: an unrecognised string
    /// (typo, case-fold variant we don't accept, etc.) degrades to the
    /// default for that tier rather than crashing the provider on a
    /// 400. See [`parse_effort`] for the accepted forms.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            frontier_model: env_or(XAI_FRONTIER_MODEL_ENV, &defaults.frontier_model),
            workhorse_model: env_or(XAI_WORKHORSE_MODEL_ENV, &defaults.workhorse_model),
            cheap_model: env_or(XAI_CHEAP_MODEL_ENV, &defaults.cheap_model),
            frontier_effort: env_effort_or(
                XAI_FRONTIER_EFFORT_ENV,
                defaults.frontier_effort,
            ),
            workhorse_effort: env_effort_or(
                XAI_WORKHORSE_EFFORT_ENV,
                defaults.workhorse_effort,
            ),
            cheap_effort: env_effort_or(XAI_CHEAP_EFFORT_ENV, defaults.cheap_effort),
        }
    }
}

/// Read an env var, treating unset / empty / whitespace-only as
/// "use default."
fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

/// Parse a [`ReasoningEffort`] from a user-facing string. Accepts
/// `low`, `medium`, `high` case-insensitively with surrounding
/// whitespace trimmed. Any other input — typos, alternative spellings
/// like `lo` / `med`, or empty/whitespace strings — returns `None`,
/// which the env-reading helper translates to "use default" rather
/// than rejecting the boot.
///
/// We deliberately do *not* alias `none`/`off`/`disabled` to a
/// reasoning level today: xAI's `grok-4.3` cannot fully disable
/// reasoning (catalog notes promise a non-reasoning variant later),
/// and silently mapping `disabled` to `Low` would mask the user's
/// expectation that no reasoning would happen at all.
fn parse_effort(raw: &str) -> Option<ReasoningEffort> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        _ => None,
    }
}

/// Read an env var into a [`ReasoningEffort`], falling back to the
/// supplied default if the var is unset, empty, whitespace-only, or
/// holds an unrecognised value. Mirrors [`env_or`]'s posture: a
/// shell-script typo silently degrades to the default rather than
/// crashing the provider.
fn env_effort_or(name: &str, default: ReasoningEffort) -> ReasoningEffort {
    match std::env::var(name) {
        Ok(v) => parse_effort(&v).unwrap_or(default),
        Err(_) => default,
    }
}

/// Wire-format string xAI's chat/completions endpoint expects for the
/// `reasoning_effort` body field. Three accepted values; anything
/// else yields a 400 from the gateway.
///
/// xAI's Responses API (`/v1/responses`) uses a different shape for
/// the same parameter — a nested object `{"reasoning":{"effort":
/// "high"}}`. We post to chat/completions (OpenAI-compat), so the
/// flat string form is what we send. If xAI ever migrates the chat/
/// completions endpoint to the nested form, this helper plus its one
/// callsite in [`XaiProvider::build_body`] is the only thing that
/// needs to change.
fn effort_wire_str(e: ReasoningEffort) -> &'static str {
    match e {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
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
    ///
    /// Also reads the three optional `XAI_*_MODEL` env vars and applies
    /// them to the provider's config. Setting any of them lets an
    /// operator swap the per-tier model without a code change — useful
    /// for one-off frontier-model runs when the workhorse keeps
    /// truncating, or for pinning to a cheaper model in CI. See
    /// [`XaiConfig::from_env`] for the empty-string normalisation rules.
    pub fn from_env(http: SecureHttpClient) -> Option<Self> {
        ApiKey::from_env_optional(XAI_API_KEY_ENV).map(|k| {
            let mut p = Self::new(http, k);
            p.config = XaiConfig::from_env();
            // Log the resolved model identifiers and per-tier
            // reasoning-effort levels at INFO so operators who set the
            // env vars can confirm the override took effect. None of
            // these values is secret. The effort line is the actual
            // cost lever post-Session-43; the model line documents
            // that the catalog still recommends a single string.
            tracing::info!(
                frontier = %p.config.frontier_model,
                workhorse = %p.config.workhorse_model,
                cheap = %p.config.cheap_model,
                frontier_effort = ?p.config.frontier_effort,
                workhorse_effort = ?p.config.workhorse_effort,
                cheap_effort = ?p.config.cheap_effort,
                "xai: provider configured"
            );
            p
        })
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

        // Pick the reasoning effort per the documented precedence:
        // request-level override wins (rare; usually a test pinning
        // wire shape), otherwise fall back to the per-tier mapping
        // configured on the provider. Per-source rules belong
        // nowhere — see [`ReasoningEffort`] doc comment.
        let effort = req
            .reasoning_effort
            .unwrap_or_else(|| self.config.effort_for(tier));

        let mut body = json!({
            "model": self.config.model_for(tier),
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            // xAI's chat/completions endpoint accepts the flat
            // `reasoning_effort` field (OpenAI-compat shape). The
            // newer xAI Responses API (`/v1/responses`) uses a
            // nested object instead — `{"reasoning":{"effort":...}}`
            // — alongside an `input` field rather than `messages`.
            // We post to chat/completions, so the flat form is
            // correct here. If a live grok-4.3 run shows the legacy
            // endpoint silently ignoring this parameter, the
            // architectural follow-up is migrating to the Responses
            // API; that change is out of scope for Session 43 and
            // would be its own session because it touches the
            // endpoint, the body shape, and the response parser.
            "reasoning_effort": effort_wire_str(effort),
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

/// Heuristic for "this JSON parse error is a truncation, not a malformed
/// schema." Truncation messages from `serde_json` look like:
///
/// ```text
/// EOF while parsing a string at line 1 column 519
/// EOF while parsing an object at line 1 column 4096
/// unexpected end of input
/// ```
///
/// All three contain either "EOF" or "end of input". A schema-violation
/// error has the form `invalid type: ...` or `missing field ...` — those
/// will not match and will not retry. A bigger token budget cannot fix
/// a schema mismatch; only re-authoring the prompt can.
///
/// Returns false for any non-`JsonParse` variant. The retry path is
/// deliberately narrow.
fn looks_like_truncated_json(err: &LlmError) -> bool {
    match err {
        LlmError::JsonParse(msg) => {
            msg.contains("EOF") || msg.contains("end of input")
        }
        _ => false,
    }
}

fn map_http_err(e: HttpError) -> LlmError {
    match e {
        HttpError::Status(401) | HttpError::Status(403) => LlmError::Auth,
        HttpError::Status(429) => LlmError::RateLimited {
            // xAI returns retry-after in headers; the legacy
            // `Status(u16)` shape arrives via the body-only path
            // that drops them. Report 0 as "unknown" so the router
            // applies its own backoff. The `StatusWithHeaders` arm
            // below carries the real value when present.
            retry_after_seconds: 0,
        },
        HttpError::Status(code) => LlmError::Api(format!("http {code}")),
        // Track D, Session 25: when the LLM provider's HTTP call
        // surfaces with headers (e.g. 429 from the gateway), thread
        // the parsed `Retry-After` value through to the router so
        // its backoff is informed rather than guessed. Other status
        // codes collapse to the same shape as the body-only path.
        HttpError::StatusWithHeaders { status, headers } => match status {
            401 | 403 => LlmError::Auth,
            429 => LlmError::RateLimited {
                retry_after_seconds: headers.retry_after_seconds().unwrap_or(0),
            },
            code => LlmError::Api(format!("http {code}")),
        },
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

        // First attempt — exactly the original behaviour.
        let first = self.send_one(tier, &request, schema_requested).await;

        // Truncation-retry path (Session 13 Improvement B).
        //
        // Only retry when:
        //   - the request asked for structured output (schema_requested),
        //   - the parse error matches the truncation signature,
        //   - the original max_tokens was below the retry ceiling so
        //     doubling actually changes anything.
        //
        // One retry only. If the bigger budget also truncates, the
        // model genuinely cannot finish this request and the caller
        // should know — re-authoring the prompt or switching tiers is
        // the right next step, not burning more tokens.
        if !should_retry_truncation(&first, schema_requested, request.max_tokens) {
            return first;
        }

        // SAFETY: we just confirmed `first` is `Err(_)` matching the
        // truncation signature; the unwrap_err is total. Captured for
        // the retry-failed branch below where we surface the original.
        let original_err = first.expect_err("guarded by should_retry_truncation");

        let retry_max_tokens = request
            .max_tokens
            .saturating_mul(2)
            .min(MAX_RETRY_TOKENS);
        tracing::warn!(
            tier = ?tier,
            original_max_tokens = request.max_tokens,
            retry_max_tokens,
            error = %original_err,
            "xai: structured output truncated; retrying once with doubled max_tokens"
        );
        let retry_req = CompletionRequest {
            system: request.system.clone(),
            user: request.user.clone(),
            schema: request.schema.clone(),
            max_tokens: retry_max_tokens,
            temperature: request.temperature,
            // Preserve the original effort across the retry: doubling
            // the token budget is the only knob this path turns; the
            // reasoning intensity stays whatever the first attempt
            // resolved (request override or per-tier mapping).
            reasoning_effort: request.reasoning_effort,
        };
        match self.send_one(tier, &retry_req, schema_requested).await {
            Ok(r) => {
                tracing::info!(tier = ?tier, "xai: truncation retry succeeded");
                Ok(r)
            }
            Err(retry_err) => {
                tracing::warn!(
                    tier = ?tier,
                    error = %retry_err,
                    "xai: truncation retry also failed; surfacing original error"
                );
                // Surface the original error rather than the retry's,
                // on the principle that the first failure is what the
                // user reported and what the logs above this layer
                // will reference.
                Err(original_err)
            }
        }
    }
}

/// Predicate for the truncation-retry path. Pulled out so the borrow
/// checker doesn't have to reason about a guard that inspects `first`
/// while later arms move from it.
fn should_retry_truncation(
    first: &Result<CompletionResponse, LlmError>,
    schema_requested: bool,
    max_tokens: u32,
) -> bool {
    match first {
        Err(e) => {
            schema_requested
                && looks_like_truncated_json(e)
                && max_tokens < MAX_RETRY_TOKENS
        }
        Ok(_) => false,
    }
}

impl XaiProvider {
    /// One round-trip: build body, post, parse. Used by `complete` and
    /// by the truncation-retry path. Factored out so the retry path
    /// doesn't duplicate the bearer-construction + post + parse steps.
    async fn send_one(
        &self,
        tier: ModelTier,
        request: &CompletionRequest,
        schema_requested: bool,
    ) -> Result<CompletionResponse, LlmError> {
        let body = self.build_body(tier, request);

        let bearer = format!("Bearer {}", self.key.expose_secret());
        // Wrap the bearer in a SecretString so expose_secret is only
        // called once, inside SecureHttpClient::post_json_bytes.
        let bearer_secret = situation_room_secure::secrets::SecretString::new(bearer);

        tracing::debug!(
            tier = ?tier,
            model = %self.config.model_for(tier),
            structured = schema_requested,
            max_tokens = request.max_tokens,
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
            reasoning_effort: None,
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
            reasoning_effort: None,
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
            reasoning_effort: None,
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
            frontier_effort: ReasoningEffort::High,
            workhorse_effort: ReasoningEffort::Medium,
            cheap_effort: ReasoningEffort::Low,
        };
        assert_eq!(cfg.model_for(ModelTier::Frontier), "f");
        assert_eq!(cfg.model_for(ModelTier::Workhorse), "w");
        assert_eq!(cfg.model_for(ModelTier::Cheap), "c");
    }

    #[test]
    fn effort_for_maps_each_tier_to_its_configured_intensity() {
        // Mirror of `model_for_maps_each_tier_to_its_configured_name` for
        // the parallel cost-differentiation lever Session 43 added.
        // Intentionally pick a *different* High/Medium/Low arrangement
        // from the default so a regression that wires Frontier to
        // Low (the cheap default) would fail this test, not silently
        // pass on the same value.
        let cfg = XaiConfig {
            frontier_model: "f".into(),
            workhorse_model: "w".into(),
            cheap_model: "c".into(),
            frontier_effort: ReasoningEffort::Low,
            workhorse_effort: ReasoningEffort::High,
            cheap_effort: ReasoningEffort::Medium,
        };
        assert_eq!(cfg.effort_for(ModelTier::Frontier), ReasoningEffort::Low);
        assert_eq!(cfg.effort_for(ModelTier::Workhorse), ReasoningEffort::High);
        assert_eq!(cfg.effort_for(ModelTier::Cheap), ReasoningEffort::Medium);
    }

    #[test]
    fn xai_config_default_assigns_high_medium_low_per_tier() {
        // The defaults are the actual cost-differentiation policy
        // (Session 43): frontier authoring deserves the deep think,
        // cheap propose-URL/classification gets fast and cheap. Pin
        // the policy so a future "let's set everything to medium for
        // safety" silent edit fails this test.
        let cfg = XaiConfig::default();
        assert_eq!(cfg.frontier_effort, ReasoningEffort::High);
        assert_eq!(cfg.workhorse_effort, ReasoningEffort::Medium);
        assert_eq!(cfg.cheap_effort, ReasoningEffort::Low);
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

    // -----------------------------------------------------------------
    // Session 13 Improvement B — env-driven model overrides
    // -----------------------------------------------------------------

    /// Tests that mutate process-wide env vars must serialise. Tokio
    /// tests run in parallel; without this lock, two tests racing on
    /// `XAI_*_MODEL_ENV` would observe each other's writes. Using a
    /// `std::sync::Mutex` rather than the `serial_test` crate keeps
    /// the dependency footprint identical.
    ///
    /// `Mutex` poisoning isn't a real concern here — every guarded
    /// section is just a few env-var calls — but we use
    /// `lock().unwrap_or_else(|e| e.into_inner())` to keep going
    /// even if a previous test panicked while holding the lock.
    /// That gives clearer test output (one failed test, not a
    /// cascade of "lock poisoned" messages).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Test-only helper: clear the three model-override env vars so a
    /// test starts from a known clean state. Tests using these vars
    /// must call this both before *and* after they mutate. The
    /// `ENV_LOCK` guard above prevents parallel mutation; the explicit
    /// before-and-after clear keeps the env clean for the *next* test
    /// even if the assertion in this one fails.
    fn clear_model_envs() {
        std::env::remove_var(XAI_FRONTIER_MODEL_ENV);
        std::env::remove_var(XAI_WORKHORSE_MODEL_ENV);
        std::env::remove_var(XAI_CHEAP_MODEL_ENV);
        // Session 43 effort env vars share the same lock; clearing
        // them in the same helper keeps the model-env tests isolated
        // from the new effort tests if they ever land between runs.
        std::env::remove_var(XAI_FRONTIER_EFFORT_ENV);
        std::env::remove_var(XAI_WORKHORSE_EFFORT_ENV);
        std::env::remove_var(XAI_CHEAP_EFFORT_ENV);
    }

    #[test]
    fn xai_config_from_env_falls_back_to_default_when_vars_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        let cfg = XaiConfig::from_env();
        let defaults = XaiConfig::default();
        assert_eq!(cfg.frontier_model, defaults.frontier_model);
        assert_eq!(cfg.workhorse_model, defaults.workhorse_model);
        assert_eq!(cfg.cheap_model, defaults.cheap_model);
    }

    #[test]
    fn xai_config_from_env_picks_up_override_when_set() {
        // Use a synthetic model name no real xAI catalog would mistake
        // for a default — keeps the assertion unambiguous.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_FRONTIER_MODEL_ENV, "test-frontier-override");
        let cfg = XaiConfig::from_env();
        assert_eq!(cfg.frontier_model, "test-frontier-override");
        // The other two should still be defaults.
        let defaults = XaiConfig::default();
        assert_eq!(cfg.workhorse_model, defaults.workhorse_model);
        assert_eq!(cfg.cheap_model, defaults.cheap_model);
        clear_model_envs();
    }

    #[test]
    fn xai_config_from_env_treats_empty_string_as_unset() {
        // A literal empty model name would be rejected by xAI with a
        // 400; treating empty as "absent" makes the override safe to
        // wire through shell scripts that conditionally export.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_WORKHORSE_MODEL_ENV, "");
        let cfg = XaiConfig::from_env();
        assert_eq!(cfg.workhorse_model, XaiConfig::default().workhorse_model);
        clear_model_envs();
    }

    #[test]
    fn xai_config_from_env_treats_whitespace_only_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_CHEAP_MODEL_ENV, "   \t  ");
        let cfg = XaiConfig::from_env();
        assert_eq!(cfg.cheap_model, XaiConfig::default().cheap_model);
        clear_model_envs();
    }

    // -----------------------------------------------------------------
    // Session 43 — reasoning-effort wire mapping + per-tier env overrides
    //
    // These tests pin two contracts:
    //   1. The wire body carries `reasoning_effort` driven by the
    //      provider's per-tier mapping, with a request-level Some(_)
    //      taking precedence — that's the cost-tier-differentiation
    //      lever Session 42 patch 4 deferred and Session 43 lands.
    //   2. The three new env vars (`XAI_*_EFFORT`) follow the same
    //      empty/whitespace-as-unset posture as the existing model
    //      env vars; an unrecognised string degrades to default rather
    //      than crashing the provider.
    // -----------------------------------------------------------------

    #[test]
    fn build_body_emits_per_tier_reasoning_effort_from_default_config() {
        // The default mapping: frontier=High, workhorse=Medium,
        // cheap=Low. Verify each tier emits the corresponding wire
        // string, with no request-level override.
        let p = test_provider();
        let req = CompletionRequest {
            system: None,
            user: "ping".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
        };

        let frontier_body = p.build_body(ModelTier::Frontier, &req);
        assert_eq!(
            frontier_body["reasoning_effort"],
            json!("high"),
            "default frontier tier maps to 'high'"
        );

        let workhorse_body = p.build_body(ModelTier::Workhorse, &req);
        assert_eq!(
            workhorse_body["reasoning_effort"],
            json!("medium"),
            "default workhorse tier maps to 'medium'"
        );

        let cheap_body = p.build_body(ModelTier::Cheap, &req);
        assert_eq!(
            cheap_body["reasoning_effort"],
            json!("low"),
            "default cheap tier maps to 'low'"
        );
    }

    #[test]
    fn build_body_request_level_effort_overrides_tier_mapping() {
        // A request-level Some(_) wins over the tier mapping. This
        // is the per-call escape hatch for tests pinning wire shape
        // — *not* a per-source routing knob (see ReasoningEffort
        // doc comment).
        let p = test_provider();
        let req = CompletionRequest {
            system: None,
            user: "ping".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            // Request asks Low even though the Frontier tier would
            // otherwise emit High from default config. Override wins.
            reasoning_effort: Some(ReasoningEffort::Low),
        };
        let body = p.build_body(ModelTier::Frontier, &req);
        assert_eq!(body["reasoning_effort"], json!("low"));
    }

    #[test]
    fn build_body_uses_provider_config_effort_when_request_is_none() {
        // Mirror of the previous test, configured via a non-default
        // XaiConfig: Frontier configured at Low (artificial — this is
        // exactly the scenario an operator setting XAI_FRONTIER_EFFORT=
        // low for cost-savings would land in). Body should reflect it.
        let cfg = XaiConfig {
            frontier_model: "x".into(),
            workhorse_model: "x".into(),
            cheap_model: "x".into(),
            frontier_effort: ReasoningEffort::Low,
            workhorse_effort: ReasoningEffort::High,
            cheap_effort: ReasoningEffort::Medium,
        };
        let provider = test_provider().with_config(cfg);
        let req = CompletionRequest {
            system: None,
            user: "ping".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
        };
        assert_eq!(
            provider.build_body(ModelTier::Frontier, &req)["reasoning_effort"],
            json!("low")
        );
        assert_eq!(
            provider.build_body(ModelTier::Workhorse, &req)["reasoning_effort"],
            json!("high")
        );
        assert_eq!(
            provider.build_body(ModelTier::Cheap, &req)["reasoning_effort"],
            json!("medium")
        );
    }

    #[test]
    fn effort_wire_str_emits_low_medium_high_strings() {
        // The wire is sensitive to exact spelling; pin all three.
        // Anything else risks a 400 from xAI's gateway. If xAI ever
        // migrates chat/completions to the Responses-API nested form
        // (`reasoning: {effort: ...}`), this helper *and* its sole
        // caller in build_body change together — see the comment on
        // `effort_wire_str` and `build_body`.
        assert_eq!(effort_wire_str(ReasoningEffort::Low), "low");
        assert_eq!(effort_wire_str(ReasoningEffort::Medium), "medium");
        assert_eq!(effort_wire_str(ReasoningEffort::High), "high");
    }

    #[test]
    fn parse_effort_accepts_low_medium_high_case_insensitively() {
        // Shell scripts may export `LOW` or `Low`; trim and case-fold
        // so they all land at the same enum.
        assert_eq!(parse_effort("low"), Some(ReasoningEffort::Low));
        assert_eq!(parse_effort("LOW"), Some(ReasoningEffort::Low));
        assert_eq!(parse_effort("Medium"), Some(ReasoningEffort::Medium));
        assert_eq!(parse_effort("HIGH"), Some(ReasoningEffort::High));
        assert_eq!(parse_effort("  high  "), Some(ReasoningEffort::High));
    }

    #[test]
    fn parse_effort_rejects_unknown_values_returning_none() {
        // Anything not in the accepted set yields None; the env helper
        // translates None to "use default" rather than crashing the
        // provider on a 400.
        assert_eq!(parse_effort(""), None);
        assert_eq!(parse_effort("   "), None);
        assert_eq!(parse_effort("lo"), None);
        assert_eq!(parse_effort("med"), None);
        // Deliberately not aliased — see parse_effort doc comment.
        assert_eq!(parse_effort("none"), None);
        assert_eq!(parse_effort("disabled"), None);
        assert_eq!(parse_effort("extreme"), None);
    }

    #[test]
    fn xai_config_from_env_picks_up_effort_overrides() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_FRONTIER_EFFORT_ENV, "low");
        std::env::set_var(XAI_CHEAP_EFFORT_ENV, "HIGH");
        let cfg = XaiConfig::from_env();
        assert_eq!(cfg.frontier_effort, ReasoningEffort::Low);
        assert_eq!(cfg.cheap_effort, ReasoningEffort::High);
        // Unset workhorse stays at default.
        assert_eq!(
            cfg.workhorse_effort,
            XaiConfig::default().workhorse_effort
        );
        clear_model_envs();
    }

    #[test]
    fn xai_config_from_env_treats_empty_effort_string_as_unset() {
        // Same posture as the model-env tests — a blank export from a
        // shell conditional stays at default rather than crashing on a
        // 400.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_WORKHORSE_EFFORT_ENV, "");
        let cfg = XaiConfig::from_env();
        assert_eq!(
            cfg.workhorse_effort,
            XaiConfig::default().workhorse_effort
        );
        clear_model_envs();
    }

    #[test]
    fn xai_config_from_env_treats_whitespace_only_effort_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_CHEAP_EFFORT_ENV, "   \t  ");
        let cfg = XaiConfig::from_env();
        assert_eq!(cfg.cheap_effort, XaiConfig::default().cheap_effort);
        clear_model_envs();
    }

    #[test]
    fn xai_config_from_env_unrecognised_effort_value_falls_back_to_default() {
        // A typo in shell scripting ("highest") should not crash the
        // provider; it degrades to the tier's default.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(XAI_FRONTIER_EFFORT_ENV, "highest");
        let cfg = XaiConfig::from_env();
        assert_eq!(
            cfg.frontier_effort,
            XaiConfig::default().frontier_effort,
            "typo degrades to default rather than crashing the provider"
        );
        clear_model_envs();
    }

    // -----------------------------------------------------------------
    // Session 13 Improvement B — truncation-retry detection
    // -----------------------------------------------------------------

    #[test]
    fn looks_like_truncated_json_matches_eof_in_string() {
        // The exact wording from the Session 13 production failure on
        // eur_lex authoring. If serde_json's wording ever changes we
        // want a test that catches the regression at build time.
        let err = LlmError::JsonParse(
            "EOF while parsing a string at line 1 column 519".into(),
        );
        assert!(looks_like_truncated_json(&err));
    }

    #[test]
    fn looks_like_truncated_json_matches_eof_in_object() {
        let err = LlmError::JsonParse(
            "EOF while parsing an object at line 1 column 4096".into(),
        );
        assert!(looks_like_truncated_json(&err));
    }

    #[test]
    fn looks_like_truncated_json_matches_unexpected_end_of_input() {
        let err = LlmError::JsonParse("unexpected end of input".into());
        assert!(looks_like_truncated_json(&err));
    }

    #[test]
    fn looks_like_truncated_json_does_not_match_schema_violation() {
        // A schema-violation message that doesn't contain EOF or end of
        // input — these are not truncations and should not retry.
        let err = LlmError::JsonParse(
            "invalid type: string \"foo\", expected an integer at line 1 column 12".into(),
        );
        assert!(!looks_like_truncated_json(&err));
    }

    #[test]
    fn looks_like_truncated_json_does_not_match_other_error_kinds() {
        assert!(!looks_like_truncated_json(&LlmError::Auth));
        assert!(!looks_like_truncated_json(&LlmError::Api("boom".into())));
        assert!(!looks_like_truncated_json(&LlmError::RateLimited {
            retry_after_seconds: 30
        }));
        assert!(!looks_like_truncated_json(&LlmError::Network("dns".into())));
    }

    // -----------------------------------------------------------------
    // Session 13 Improvement B — should_retry_truncation predicate
    //
    // The predicate gates the retry path. Wrong gating either burns
    // budget on hopeless requests or silently swallows the legitimate
    // first-shot success. Test all four corner cases explicitly.
    // -----------------------------------------------------------------

    fn fake_completion() -> CompletionResponse {
        CompletionResponse {
            text: "ok".into(),
            structured: None,
            provider: "xai".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn should_retry_truncation_yes_when_all_conditions_match() {
        let err = LlmError::JsonParse(
            "EOF while parsing a string at line 1 column 519".into(),
        );
        let first: Result<CompletionResponse, LlmError> = Err(err);
        assert!(should_retry_truncation(&first, true, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_when_request_was_unstructured() {
        // Even if the body parse failed with a truncation signature,
        // an unstructured request can't have the schema-output mid-
        // string truncation we're guarding against; the failure must
        // be something else and shouldn't loop.
        let err = LlmError::JsonParse("EOF while parsing a string".into());
        let first: Result<CompletionResponse, LlmError> = Err(err);
        assert!(!should_retry_truncation(&first, false, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_when_already_at_ceiling() {
        // If max_tokens already equals the retry ceiling, doubling
        // can't change anything; retrying would just burn another
        // round-trip for the same outcome.
        let err = LlmError::JsonParse("EOF while parsing a string".into());
        let first: Result<CompletionResponse, LlmError> = Err(err);
        assert!(!should_retry_truncation(&first, true, MAX_RETRY_TOKENS));
    }

    #[test]
    fn should_retry_truncation_no_when_first_succeeded() {
        let first: Result<CompletionResponse, LlmError> = Ok(fake_completion());
        assert!(!should_retry_truncation(&first, true, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_for_non_truncation_errors() {
        let first: Result<CompletionResponse, LlmError> =
            Err(LlmError::Api("http 500".into()));
        assert!(!should_retry_truncation(&first, true, 8_000));
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
            reasoning_effort: None,
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
            reasoning_effort: None,
        };
        let resp = provider
            .complete(ModelTier::Workhorse, req)
            .await
            .expect("live xai structured completion should succeed");
        let structured = resp.structured.expect("structured field should be populated");
        assert!(structured.get("result").is_some(), "expected result field, got {structured}");
    }
}
