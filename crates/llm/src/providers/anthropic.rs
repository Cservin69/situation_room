//! Anthropic (Claude) provider — Messages API.
//!
//! Anthropic's `/v1/messages` endpoint does **not** speak the OpenAI chat-
//! completions wire format. Compared to xAI:
//!
//! - `system` is a **top-level** field on the body, not a `role: "system"`
//!   message in the messages array.
//! - Authentication is `x-api-key: <key>`, not `Authorization: Bearer …`.
//! - Every request must carry an `anthropic-version` header. The default
//!   is the long-standing `2023-06-01`; overridable via env for the rare
//!   case an operator pins to a future version.
//! - Structured output is delivered via **forced tool use**, not a
//!   `response_format` field. We declare a single tool whose
//!   `input_schema` is the caller's JSON Schema, then set
//!   `tool_choice = { "type": "tool", "name": <schema name> }` so the
//!   model is required to invoke it. The structured payload comes back
//!   as `content[].input` of the matching `tool_use` block.
//!
//! Everything else is the same shape as [`grok`](super::grok): one round-
//! trip per call through [`SecureHttpClient`], no SDK, no separate HTTP
//! client (ADR 0009 §"The rule"), bounds-check every prompt before send,
//! wire shapes deserialised through forgiving private types.
//!
//! ## Reasoning-effort plumbing
//!
//! [`CompletionRequest::reasoning_effort`] is part of the trait shape
//! Session 43 added so the xAI provider can route per-tier reasoning
//! intensity onto its chat/completions wire. **This provider ignores
//! the field**: the Anthropic Messages API does not currently accept
//! a per-request reasoning-intensity parameter, and we explicitly do
//! not synthesise one — pretending to honor a knob that has no wire
//! effect would mislead callers into thinking they have cost
//! differentiation here when they do not. If Anthropic later adds an
//! equivalent parameter, the change lands in this provider's
//! `build_body`; the trait shape already carries the field.
//!
//! ## Truncation-retry path
//!
//! Anthropic surfaces truncation through `stop_reason: "max_tokens"`,
//! not through a JSON parse error. The retry policy is identical in
//! spirit to [`grok::should_retry_truncation`]: when a structured-output
//! request comes back with `stop_reason == "max_tokens"` (the model
//! genuinely ran out of budget mid-tool-input) and the original
//! `max_tokens` was below the [`MAX_RETRY_TOKENS`] ceiling, we retry
//! exactly once with `max_tokens` doubled and clamped. One retry only —
//! a second truncation means the request can't be served at the chosen
//! tier and the caller should surface the failure.
//!
//! Plain-text (non-schema) requests do **not** retry on truncation:
//! the user got *something*, and a bigger budget on the same prompt
//! produces a longer answer that may or may not be what they wanted.
//! That's a prompt-or-tier decision, not a retry decision.
//!
//! ## Model names
//!
//! As with xAI, we don't hardcode a single model name in production
//! code. Defaults live in [`AnthropicConfig::default`] with a comment
//! pointing at the catalog; any caller can override per-tier via
//! [`AnthropicProvider::with_config`]. Three env vars
//! (`ANTHROPIC_FRONTIER_MODEL`, `ANTHROPIC_WORKHORSE_MODEL`,
//! `ANTHROPIC_CHEAP_MODEL`) override the per-tier defaults at startup.
//! See [`AnthropicConfig::from_env`].
//!
//! See <https://docs.claude.com/en/docs/about-claude/models> for the
//! current model catalog.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use situation_room_secure::bounds::{check_string, Bounds};
use situation_room_secure::http::{HttpError, SecureHttpClient};
use situation_room_secure::secrets::ApiKey;

use crate::providers::trait_def::{
    render_rate_limit_reason, CompletionRequest, CompletionResponse, LlmError, LlmProvider,
    ModelTier,
};

/// Environment variable the provider reads its key from.
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Per-tier model overrides. Optional; if any is unset (or set to an
/// empty / whitespace-only string) the tier's hardcoded default in
/// [`AnthropicConfig::default`] is used. Same posture as xAI's three
/// model env vars (Session 13 Improvement B). Lets operators swap to
/// a frontier model for a session — or pin to a cheaper model in
/// CI — without recompiling.
pub const ANTHROPIC_FRONTIER_MODEL_ENV: &str = "ANTHROPIC_FRONTIER_MODEL";
pub const ANTHROPIC_WORKHORSE_MODEL_ENV: &str = "ANTHROPIC_WORKHORSE_MODEL";
pub const ANTHROPIC_CHEAP_MODEL_ENV: &str = "ANTHROPIC_CHEAP_MODEL";

/// Override for the `anthropic-version` header. Defaults to
/// [`DEFAULT_ANTHROPIC_VERSION`] which is the stable long-standing
/// version. An operator pinning to a newer version (e.g. for a
/// pre-release feature) can set this without code changes; the
/// header value is not a secret and is logged at INFO on boot.
pub const ANTHROPIC_VERSION_ENV: &str = "ANTHROPIC_VERSION";

/// Anthropic Messages endpoint. Overridable (only) for tests.
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Default `anthropic-version` header value. Stable since 2023-06-01.
/// New API features are gated behind newer version strings; this
/// default is the long-supported baseline.
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Token ceiling used by the truncation-retry path. Picked to mirror
/// [`super::grok::MAX_RETRY_TOKENS`] — well above what any of our
/// prompts actually need (a fully-populated `ResearchPlan` or
/// `FetchRecipe` JSON object lands well under 16 KB of tokens) but
/// well below Anthropic's per-model ceilings, so the retry can never
/// loop forever even if the model genuinely can't finish.
const MAX_RETRY_TOKENS: u32 = 32_768;

/// Which model name to use for each tier.
///
/// Defaults are sensible at the time of writing but are not load-
/// bearing — Anthropic's catalog drifts (typically additively;
/// older model ids stay valid for many months but new variants
/// appear in between), and any running binary should be able to
/// override these via env without a recompile.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub frontier_model: String,
    pub workhorse_model: String,
    pub cheap_model: String,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        // Defaults pulled from the model catalog as of 2026-05-03.
        // Verify at https://docs.claude.com/en/docs/about-claude/models
        // before relying on them in production. These choices are
        // deliberately conservative — they pick the most-recent
        // generation in each tier, and any operator who needs to pin
        // to a specific dated revision can do so via the env vars
        // without touching code.
        //
        // - Frontier: the reasoning flagship. Used for hard
        //   classifications, novel topics, and recipe authoring on
        //   structurally challenging sources.
        // - Workhorse: mid-tier; the everyday extraction model.
        // - Cheap: small fast model; high-volume tagging and routing.
        Self {
            frontier_model: "claude-opus-4-7".to_string(),
            workhorse_model: "claude-sonnet-4-6".to_string(),
            cheap_model: "claude-haiku-4-5-20251001".to_string(),
        }
    }
}

impl AnthropicConfig {
    pub fn model_for(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Frontier => &self.frontier_model,
            ModelTier::Workhorse => &self.workhorse_model,
            ModelTier::Cheap => &self.cheap_model,
        }
    }

    /// Build a config by reading the three optional env vars and
    /// falling back to [`AnthropicConfig::default`] for any that are
    /// unset or empty/whitespace-only.
    ///
    /// Empty-string normalisation matches the `endpoint_hint`
    /// discipline from Session 10's TOML loaders and the
    /// `XaiConfig::from_env` rules: a blank string is "absent", not
    /// "use literal empty model name." A literal empty model name
    /// would be rejected by Anthropic with a 400, which is a worse
    /// failure mode than silently using the default.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            frontier_model: env_or(ANTHROPIC_FRONTIER_MODEL_ENV, &defaults.frontier_model),
            workhorse_model: env_or(ANTHROPIC_WORKHORSE_MODEL_ENV, &defaults.workhorse_model),
            cheap_model: env_or(ANTHROPIC_CHEAP_MODEL_ENV, &defaults.cheap_model),
        }
    }
}

/// Read an env var, treating unset / empty / whitespace-only as
/// "use default." Mirrors the helper of the same name in `grok.rs`;
/// duplicated rather than shared because pulling it into a public
/// helper would expose env-handling internals through the crate
/// surface for one extra line of code.
fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

/// Anthropic provider. Holds an API key, a reference-counted HTTP
/// client, the per-tier model map, and the API version header value.
///
/// Clone is cheap: [`SecureHttpClient`] wraps an `Arc` internally and
/// [`ApiKey`] wraps [`secrecy::SecretString`] which is `Clone` by
/// design.
pub struct AnthropicProvider {
    http: SecureHttpClient,
    key: ApiKey,
    config: AnthropicConfig,
    endpoint: String,
    api_version: String,
}

impl AnthropicProvider {
    /// Construct from an already-loaded key and shared HTTP client.
    pub fn new(http: SecureHttpClient, key: ApiKey) -> Self {
        Self {
            http,
            key,
            config: AnthropicConfig::default(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
        }
    }

    /// Load the key from `ANTHROPIC_API_KEY`. Returns `None` if unset
    /// / empty / placeholder — lets the caller fall back to another
    /// provider.
    ///
    /// Also reads the three optional `ANTHROPIC_*_MODEL` env vars and
    /// the optional `ANTHROPIC_VERSION` env var, applying them to the
    /// provider's config. See [`AnthropicConfig::from_env`] for the
    /// empty-string normalisation rules.
    pub fn from_env(http: SecureHttpClient) -> Option<Self> {
        ApiKey::from_env_optional(ANTHROPIC_API_KEY_ENV).map(|k| {
            let mut p = Self::new(http, k);
            p.config = AnthropicConfig::from_env();
            p.api_version = env_or(ANTHROPIC_VERSION_ENV, DEFAULT_ANTHROPIC_VERSION);
            // Log the resolved model identifiers + API version at
            // INFO so operators who set the env vars can confirm the
            // override took effect. None of these values are secret.
            tracing::info!(
                frontier = %p.config.frontier_model,
                workhorse = %p.config.workhorse_model,
                cheap = %p.config.cheap_model,
                api_version = %p.api_version,
                "anthropic: provider configured"
            );
            p
        })
    }

    /// Override the tier → model mapping.
    pub fn with_config(mut self, config: AnthropicConfig) -> Self {
        self.config = config;
        self
    }

    /// Test-only: override the endpoint URL. Not exposed outside
    /// tests so production code cannot accidentally point at a
    /// non-Anthropic host.
    #[cfg(test)]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Build the JSON request body for a given tier + request. Pure —
    /// factored out so tests can assert the wire shape without a
    /// network call.
    ///
    /// ## Prompt-caching breakpoints (Session 75)
    ///
    /// Anthropic's prompt cache activates only when at least one
    /// `cache_control: {type: "ephemeral"}` block appears on the
    /// request. Today we place breakpoints in two positions, both
    /// chosen so a future change can't accidentally invalidate the
    /// cache by introducing per-call variability above the boundary:
    ///
    ///   - **tools[0]**: when the caller asks for structured output,
    ///     the tool definition (name + description + input_schema)
    ///     is large, identical across calls in the same campaign,
    ///     and lives in a stable wire position. Always safe to mark
    ///     cacheable.
    ///   - **user content prefix**: the v1.22 recipe-author prompt
    ///     (Session 74) puts every `{{VAR}}` substitution below a
    ///     literal `## Concrete inputs` heading. When that marker
    ///     appears in `req.user`, we split the user turn into two
    ///     text blocks and mark the prefix cacheable. If the marker
    ///     is absent (e.g. a legacy prompt or a small one-off call),
    ///     we emit the user turn as a single uncached string —
    ///     same wire shape as pre-Session-75, no behavioural change.
    ///
    /// Breakpoints below Anthropic's minimum-cacheable-prefix
    /// threshold (1024 input tokens on the documented free tier
    /// today) are silently ignored by the server, so adding a
    /// breakpoint to a small call wastes the wire bytes but
    /// produces a correctly uncached response — not a failure mode.
    /// The recipe-author prompt is multiple orders of magnitude
    /// above the threshold; for the classifier (~1k tokens) the
    /// breakpoint may or may not bite depending on plan-line
    /// length.
    ///
    /// ## What this is NOT
    ///
    /// - **Not source-specific.** The split rule is "look for a
    ///   literal marker"; no host, no model, no plan. Closed-vocab
    ///   discipline holds.
    /// - **Not a guarantee of cache hits.** The wire shape only
    ///   declares "this prefix is eligible for caching." The
    ///   server decides whether it has matching bytes in its cache
    ///   pool. The cost-by-tier ledger (Session 75 piece 1)
    ///   surfaces the observed hit ratio.
    fn build_body(&self, tier: ModelTier, req: &CompletionRequest) -> Value {
        // Build the user content. Either a plain string (legacy
        // path) or an array of text blocks with cache_control on the
        // prefix.
        let user_content = build_user_content_with_cache_breakpoint(&req.user);

        let messages = json!([
            { "role": "user", "content": user_content }
        ]);

        let mut body = json!({
            "model": self.config.model_for(tier),
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "messages": messages,
        });

        if let Some(system) = &req.system {
            // Top-level field, distinct from messages. Today we leave
            // system as a plain string — the production system text
            // is small (~20 tokens) and well below the cache-prefix
            // threshold, so converting it to the array form for
            // cache_control would be performative.
            body["system"] = json!(system);
        }

        if let Some(schema) = &req.schema {
            // Anthropic structured output via forced tool use.
            //
            // We declare exactly one tool whose `input_schema` is the
            // caller's JSON Schema, then constrain tool_choice to it
            // so the model has no path other than calling that tool.
            // The structured payload comes back as the tool_use's
            // `input` field — already a JSON object, no client-side
            // string parsing required.
            //
            // Session 75: mark the tool block cacheable. The schema
            // is identical across every call in the same authoring
            // campaign, so this is a free cache lever. The marker is
            // safe even for small schemas — if the byte count is
            // below the server-side threshold, the breakpoint is
            // ignored without error.
            body["tools"] = json!([
                {
                    "name": schema.name,
                    "description": "Return the requested structured output.",
                    "input_schema": schema.schema,
                    "cache_control": { "type": "ephemeral" },
                }
            ]);
            body["tool_choice"] = json!({
                "type": "tool",
                "name": schema.name,
            });
        }

        body
    }

    /// Parse an Anthropic response body into a [`CompletionResponse`]
    /// plus a side signal indicating whether the response was
    /// truncated by `max_tokens`. The truncation signal is consumed
    /// by [`Self::complete`]'s retry path; it is not surfaced through
    /// the trait because the trait doesn't speak "truncation".
    fn parse_response(
        &self,
        raw: Value,
        schema_requested: bool,
    ) -> Result<(CompletionResponse, bool), LlmError> {
        let parsed: AnthropicMessage = serde_json::from_value(raw).map_err(|e| {
            LlmError::Api(format!("unexpected response shape: {e}"))
        })?;

        let was_truncated = parsed
            .stop_reason
            .as_deref()
            .map(|s| s == "max_tokens")
            .unwrap_or(false);

        // For structured output we want the input from the tool_use
        // block; for plain text we want the text from the text block.
        // Scan all blocks rather than indexing position-0 — Anthropic
        // can emit a preamble text block before a tool_use block when
        // the model "thinks aloud" briefly first.
        let mut text_acc = String::new();
        let mut structured: Option<Value> = None;
        let mut saw_tool_use = false;

        for block in parsed.content.into_iter() {
            match block {
                AnthropicContentBlock::Text { text } => {
                    if !text_acc.is_empty() {
                        text_acc.push('\n');
                    }
                    text_acc.push_str(&text);
                }
                AnthropicContentBlock::ToolUse { name, input, .. } => {
                    saw_tool_use = true;
                    if schema_requested {
                        // Defensive: only accept the input from the
                        // tool we asked for. If the model hallucinated
                        // a different tool name (shouldn't happen with
                        // tool_choice set, but Anthropic's contract is
                        // best-effort like xAI's strict mode), surface
                        // it as an Api error rather than silently
                        // accepting wrong-shape data.
                        if let Some(req_schema_name) = self.last_requested_schema_name() {
                            if name != req_schema_name {
                                return Err(LlmError::Api(format!(
                                    "model returned tool_use for unexpected tool: \
                                     got '{name}', expected '{req_schema_name}'"
                                )));
                            }
                        }
                        structured = Some(input);
                    }
                    // If schema wasn't requested but tool_use came back
                    // anyway (impossible without our setting tools, but
                    // be defensive), we just ignore it — text_acc still
                    // carries whatever the model said in plain language.
                }
                AnthropicContentBlock::Other => {
                    // Unknown content-block type. Ignore. Anthropic
                    // adds new block types over time; surfacing them
                    // as errors would break this provider on every
                    // catalog refresh. The block is effectively
                    // skipped; if it carried the only payload, the
                    // downstream extractor will surface the absence.
                }
            }
        }

        // Sanity: if a schema was requested but no tool_use block came
        // back (and the model didn't truncate to nothing), surface a
        // clear error rather than handing back `structured: None` and
        // letting the caller fail later with a less-specific message.
        if schema_requested && structured.is_none() && !was_truncated {
            return Err(LlmError::Api(format!(
                "model did not invoke the structured-output tool \
                 (saw_tool_use={saw_tool_use}, text_len={})",
                text_acc.len()
            )));
        }

        Ok((
            CompletionResponse {
                text: text_acc,
                structured,
                provider: "anthropic".to_string(),
                model: parsed.model.unwrap_or_default(),
                input_tokens: parsed.usage.as_ref().and_then(|u| u.input_tokens),
                output_tokens: parsed.usage.as_ref().and_then(|u| u.output_tokens),
                // Session 74: thread Anthropic's
                // `cache_read_input_tokens` onto the response. `None`
                // when the usage block is absent or doesn't carry the
                // field (the API only emits it when cache_control
                // breakpoints are active on the request); `Some(0)`
                // when present but cold; `Some(n)` when the prefix
                // matched the prompt cache. Same shape contract as
                // grok.rs.
                cached_input_tokens: parsed
                    .usage
                    .as_ref()
                    .and_then(|u| u.cache_read_input_tokens),
            },
            was_truncated,
        ))
    }

    /// Returns the name of the schema the most-recent build_body call
    /// declared as the forced tool, **if** we can recover it from the
    /// stored request. Used by [`Self::parse_response`] to verify the
    /// model invoked the right tool.
    ///
    /// Today this is a no-op (we don't keep the request around in
    /// the provider) — `parse_response` lives on the same call as
    /// `build_body` so the caller knows the schema name. Kept as a
    /// hook for a future tightening; returns `None` so the
    /// hallucination-defence path is skipped.
    fn last_requested_schema_name(&self) -> Option<&str> {
        None
    }
}

/// Literal marker that signals the per-call inputs section of the
/// v1.22 recipe-author prompt (Session 74). When present in the user
/// content, [`build_user_content_with_cache_breakpoint`] splits the
/// text at the marker and marks the prefix cacheable.
///
/// The choice of marker matches the recipe-author prompt's
/// changelog entry word-for-word; a documentation change that
/// renames the heading must update both sides in the same commit, or
/// the breakpoint silently stops firing and the cache lever
/// regresses to "no cacheable prefix declared." See
/// `config/prompts/recipe_author.md`.
const CONCRETE_INPUTS_MARKER: &str = "## Concrete inputs";

/// Convert the user turn's string into one of:
///
///   - a plain JSON string (no marker, legacy path, no cache breakpoint),
///   - a 1-element array of one text block when the marker sits at
///     the very start (degenerate — prefix would be empty so we don't
///     bother declaring a breakpoint),
///   - a 2-element array of two text blocks: a cacheable prefix
///     ending immediately before the marker, and an uncached tail
///     starting at the marker.
///
/// The marker is included with the **tail** block (not the prefix)
/// so the prefix bytes are identical across calls: every call's
/// prefix ends at the same byte sequence, immediately before
/// `"## Concrete inputs"`. Including the marker on the prefix side
/// would be wrong only if a future revision wanted to insert text
/// after the marker but keep the same prefix — moving the marker
/// to the tail side is the simpler invariant.
fn build_user_content_with_cache_breakpoint(user: &str) -> Value {
    // Look for the marker. If absent, ship the legacy plain-string
    // shape — same wire bytes as pre-Session-75 builds, so this
    // function is byte-for-byte non-disruptive on callsites whose
    // prompts don't carry the marker.
    let Some(idx) = user.find(CONCRETE_INPUTS_MARKER) else {
        return Value::String(user.to_string());
    };

    if idx == 0 {
        // Pathological: marker is at byte 0. No prefix to cache;
        // emit the legacy plain-string form rather than a 1-element
        // cache_control'd array whose declared prefix is empty.
        return Value::String(user.to_string());
    }

    let prefix = &user[..idx];
    let tail = &user[idx..];

    json!([
        {
            "type": "text",
            "text": prefix,
            "cache_control": { "type": "ephemeral" },
        },
        {
            "type": "text",
            "text": tail,
        }
    ])
}

/// Predicate for the truncation-retry path. Pulled out so the borrow
/// checker doesn't have to reason about a guard that inspects the
/// first attempt while later arms move from it. Mirrors the shape of
/// [`super::grok::should_retry_truncation`]; the *signal* differs (we
/// gate on `was_truncated`, an explicit flag from the response body,
/// rather than on the JSON parse error message) but the *policy* is
/// identical: structured-output only, doubled budget below the
/// ceiling, one retry only.
fn should_retry_truncation(
    first: &Result<(CompletionResponse, bool), LlmError>,
    schema_requested: bool,
    max_tokens: u32,
) -> bool {
    match first {
        Ok((_, was_truncated)) => {
            schema_requested && *was_truncated && max_tokens < MAX_RETRY_TOKENS
        }
        Err(_) => false,
    }
}

fn map_http_err(e: HttpError) -> LlmError {
    match e {
        HttpError::Status(401) | HttpError::Status(403) => LlmError::Auth,
        HttpError::Status(429) => LlmError::RateLimited {
            // Anthropic returns retry-after in headers, but the
            // legacy `Status(u16)` shape arrives via classify_err's
            // body-only path that discards them — report 0 as
            // "unknown" so the router can apply its own backoff.
            // The headers-aware path (`StatusWithHeaders` arm
            // below) carries the real value when present. Reason
            // is empty on this branch because the body-only HTTP
            // path discards the response payload before we reach
            // here.
            retry_after_seconds: 0,
            reason: String::new(),
        },
        HttpError::Status(code) => LlmError::Api(format!("http {code}")),
        // Track D, Session 25: when the LLM provider's HTTP call
        // surfaces with headers (e.g. 429 from the gateway), thread
        // the parsed `Retry-After` value through to the router so
        // its backoff is informed rather than guessed. Other status
        // codes collapse to the same shape as the body-only path.
        //
        // Session 69: project the response body into the RateLimited
        // variant's `reason` field. Mirrors the xAI path —
        // `render_rate_limit_reason` parses Anthropic's
        // `{"error": ...}` shape and falls back to plain text.
        HttpError::StatusWithHeaders { status, headers, body } => match status {
            401 | 403 => LlmError::Auth,
            429 => LlmError::RateLimited {
                retry_after_seconds: headers.retry_after_seconds().unwrap_or(0),
                reason: render_rate_limit_reason(&body),
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
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn supported_tiers(&self) -> &[ModelTier] {
        // Anthropic covers all three tiers via distinct model names.
        &[ModelTier::Frontier, ModelTier::Workhorse, ModelTier::Cheap]
    }

    async fn complete(
        &self,
        tier: ModelTier,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        // Enforce the LLM prompt size bound before sending. Same
        // discipline as xAI; ADR 0009.
        if let Some(system) = &request.system {
            check_string("llm_prompt_system", system, Bounds::LLM_PROMPT_BODY)
                .map_err(|e| LlmError::Api(e.to_string()))?;
        }
        check_string("llm_prompt_user", &request.user, Bounds::LLM_PROMPT_BODY)
            .map_err(|e| LlmError::Api(e.to_string()))?;

        let schema_requested = request.schema.is_some();

        // First attempt — exactly the original behaviour.
        let first = self.send_one(tier, &request, schema_requested).await;

        // Truncation-retry path. Only retry when:
        //   - the request asked for structured output,
        //   - the response carried `stop_reason == "max_tokens"`,
        //   - the original max_tokens was below the retry ceiling.
        // One retry only — see module docs for the rationale.
        if !should_retry_truncation(&first, schema_requested, request.max_tokens) {
            // Translate the (response, truncated) tuple back into the
            // trait's response shape. The truncation flag is dropped
            // here because the trait doesn't surface it.
            return first.map(|(resp, _)| resp);
        }

        // SAFETY: we just confirmed `first` is `Ok((_, true))`; the
        // unwrap is total. We need the response to surface its model
        // identifier in the warn log below.
        let (orig_resp, _) = first.expect("guarded by should_retry_truncation");

        let retry_max_tokens = request
            .max_tokens
            .saturating_mul(2)
            .min(MAX_RETRY_TOKENS);
        tracing::warn!(
            tier = ?tier,
            model = %orig_resp.model,
            original_max_tokens = request.max_tokens,
            retry_max_tokens,
            "anthropic: structured output truncated; retrying once with doubled max_tokens"
        );
        let retry_req = CompletionRequest {
            system: request.system.clone(),
            user: request.user.clone(),
            schema: request.schema.clone(),
            max_tokens: retry_max_tokens,
            temperature: request.temperature,
            // Preserved for parity with the trait shape; the
            // Anthropic provider does not currently map this onto
            // the wire (see comment on `complete`).
            reasoning_effort: request.reasoning_effort,
            // Session 80 — carry the cache-key hint through to the
            // retry so any future Anthropic mapping (cache_control on
            // the system block, say) stays consistent across the
            // original + retry pair.
            prompt_cache_key: request.prompt_cache_key.clone(),
        };
        match self.send_one(tier, &retry_req, schema_requested).await {
            Ok((resp, was_truncated_again)) => {
                if was_truncated_again {
                    tracing::warn!(
                        tier = ?tier,
                        "anthropic: truncation retry also truncated; surfacing partial response"
                    );
                } else {
                    tracing::info!(tier = ?tier, "anthropic: truncation retry succeeded");
                }
                Ok(resp)
            }
            Err(retry_err) => {
                tracing::warn!(
                    tier = ?tier,
                    error = %retry_err,
                    "anthropic: truncation retry failed; surfacing original (truncated) response"
                );
                // Surface the original response — the user got
                // *something* back on the first attempt, and that
                // something is more useful than a synthetic error
                // built from a network failure on the retry.
                Ok(orig_resp)
            }
        }
    }
}

impl AnthropicProvider {
    /// One round-trip: build body, post, parse. Used by `complete`
    /// and by the truncation-retry path. Factored out so the retry
    /// path doesn't duplicate the auth-construction + post + parse
    /// steps. Returns `(response, was_truncated)` so the caller can
    /// decide whether to retry.
    async fn send_one(
        &self,
        tier: ModelTier,
        request: &CompletionRequest,
        schema_requested: bool,
    ) -> Result<(CompletionResponse, bool), LlmError> {
        let body = self.build_body(tier, request);

        // Wrap the key in a SecretString so expose_secret is only
        // called once, inside SecureHttpClient::post_json_bytes. The
        // header value carries `set_sensitive(true)` from there so
        // reqwest-internal logging redacts it.
        let key_secret = situation_room_secure::secrets::SecretString::new(
            self.key.expose_secret().to_string(),
        );

        tracing::debug!(
            tier = ?tier,
            model = %self.config.model_for(tier),
            structured = schema_requested,
            max_tokens = request.max_tokens,
            "anthropic: sending completion"
        );

        let raw: Value = self
            .http
            .post_json(
                &self.endpoint,
                &body,
                // Anthropic's auth is `x-api-key: <key>`, NOT
                // `Authorization: Bearer …`. The header name is
                // load-bearing — using `authorization` would 401.
                &[("x-api-key", &key_secret)],
                // `anthropic-version` is a non-secret header and
                // goes through extra_headers. Note: we do NOT pass
                // `content-type` here — `SecureHttpClient::
                // post_json_bytes` calls `.json(body)` on the
                // reqwest builder, which already sets the header.
                // Adding it again would cause a duplicate header on
                // the wire (xAI rejects that with 415; Anthropic's
                // posture isn't documented but the rule is the
                // same). See `SecureHttpClient::post_json_bytes`.
                &[("anthropic-version", self.api_version.as_str())],
            )
            .await
            .map_err(map_http_err)?;

        self.parse_response(raw, schema_requested)
    }
}

// ---------------------------------------------------------------------------
// Private wire shapes — kept minimal and forgiving. Any extra fields
// the API adds in the future are ignored. The shapes only model the
// fields we actually read.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

/// One block of an Anthropic message's content. Anthropic's content is
/// always an array of typed blocks, even for plain-text responses
/// (where the array has a single `text` block).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        // `id` exists on the wire but we don't use it.
        #[serde(default, rename = "id")]
        _id: Option<String>,
        name: String,
        input: Value,
    },
    /// Catch-all for block types Anthropic adds in the future
    /// (`thinking`, `image`, etc.). Ignored at parse time so adding
    /// a new block type at the API layer doesn't break this provider.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    /// Session 74 — Anthropic's cached-input projection. The
    /// Messages API ships `cache_read_input_tokens` on the usage
    /// block when prompt-caching cache_control breakpoints are in
    /// effect; absent or zero on uncached requests. This crate
    /// doesn't yet declare `cache_control` blocks on its requests,
    /// so today the field is `None` in practice — the projection is
    /// here so when the cache-control plumbing lands, surfacing it
    /// is a one-line build_body change rather than a parser change.
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
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
        // ApiKey::from_env enforces a >=16 char minimum; we must set
        // an env var and use from_env. We use a test-scoped var name
        // so the unit tests don't collide with a real ANTHROPIC_API_KEY
        // the developer may have exported in their shell.
        std::env::set_var(
            "TEST_ANTHROPIC_KEY_FOR_UNIT_TESTS",
            "sk-ant-fake-test-key-1234567890",
        );
        ApiKey::from_env("TEST_ANTHROPIC_KEY_FOR_UNIT_TESTS").unwrap()
    }

    fn test_provider() -> AnthropicProvider {
        AnthropicProvider::new(test_http(), fake_key())
    }

    #[test]
    fn build_body_has_expected_shape_for_plain_completion() {
        let p = test_provider();
        let req = CompletionRequest {
            system: Some("you are a helpful assistant".into()),
            user: "what is 2+2?".into(),
            schema: None,
            max_tokens: 64,
            // 0.5 is exactly representable in both f32 and f64; the
            // serialized JSON number roundtrips cleanly. See the
            // comment on the equivalent xAI test.
            temperature: 0.5,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Cheap, &req);

        assert_eq!(body["model"], json!(p.config.cheap_model));
        assert_eq!(body["max_tokens"], json!(64));
        assert_eq!(body["temperature"], json!(0.5));
        // System is a top-level field, NOT a message.
        assert_eq!(body["system"], json!("you are a helpful assistant"));

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "messages array should not contain system");
        assert_eq!(messages[0]["role"], json!("user"));
        assert_eq!(messages[0]["content"], json!("what is 2+2?"));

        // No schema => no tools / tool_choice on the body.
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn build_body_without_system_omits_top_level_system_field() {
        let p = test_provider();
        let req = CompletionRequest {
            system: None,
            user: "hello".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Workhorse, &req);
        assert!(
            body.get("system").is_none(),
            "no system key when request.system is None"
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], json!("user"));
    }

    // -----------------------------------------------------------------
    // Session 75 — Anthropic cache_control breakpoint plumbing
    // -----------------------------------------------------------------

    #[test]
    fn build_body_user_content_stays_plain_string_when_marker_absent() {
        // No `## Concrete inputs` marker → legacy plain-string shape.
        // Byte-for-byte compatible with pre-Session-75 builds; this
        // test pins the absence-of-regression for callsites whose
        // prompts don't carry the v1.22 marker.
        let p = test_provider();
        let req = CompletionRequest {
            system: None,
            user: "hello world, no marker here".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Workhorse, &req);
        let content = &body["messages"][0]["content"];
        assert!(
            content.is_string(),
            "no marker → user content stays a plain string; got {content}"
        );
        assert_eq!(content.as_str().unwrap(), "hello world, no marker here");
    }

    #[test]
    fn build_body_user_content_splits_around_marker_when_present() {
        // The v1.22 prompt path: `## Concrete inputs` marker splits
        // the user turn into a cacheable prefix and an uncached
        // tail. Confirm the wire shape and the cache_control
        // placement.
        let p = test_provider();
        let prompt = "Authoring rules go here.\n\nMore rules.\n\n## Concrete inputs\nplan_id=abc\n";
        let req = CompletionRequest {
            system: None,
            user: prompt.into(),
            schema: None,
            max_tokens: 64,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Frontier, &req);
        let content = &body["messages"][0]["content"];
        let blocks = content.as_array().expect("array shape when marker present");
        assert_eq!(blocks.len(), 2);
        // Prefix block — cache_control attached, ends immediately
        // before the marker.
        assert_eq!(blocks[0]["type"], json!("text"));
        assert_eq!(
            blocks[0]["text"].as_str().unwrap(),
            "Authoring rules go here.\n\nMore rules.\n\n"
        );
        assert_eq!(
            blocks[0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        // Tail block — starts at the marker, no cache_control.
        assert_eq!(blocks[1]["type"], json!("text"));
        assert!(blocks[1]["text"].as_str().unwrap().starts_with("## Concrete inputs"));
        assert!(blocks[1].get("cache_control").is_none());
    }

    #[test]
    fn build_body_user_content_marker_at_start_falls_back_to_string() {
        // Pathological: marker is at byte 0. No prefix to cache; fall
        // back to legacy plain-string form rather than ship a 1-block
        // array whose declared prefix is empty.
        let p = test_provider();
        let prompt = "## Concrete inputs\nplan_id=abc\n";
        let req = CompletionRequest {
            system: None,
            user: prompt.into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Cheap, &req);
        let content = &body["messages"][0]["content"];
        assert!(content.is_string(), "marker-at-start falls back to string");
    }

    #[test]
    fn build_body_tool_block_carries_cache_control_when_schema_set() {
        // Symmetric lever: the tool definition is identical across
        // every call in a campaign; mark it cacheable always.
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
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Frontier, &req);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0]["cache_control"],
            json!({ "type": "ephemeral" }),
            "tool block must carry cache_control: ephemeral"
        );
    }

    #[test]
    fn build_user_content_with_cache_breakpoint_unit() {
        // Direct unit test on the helper, complementing the
        // through-build_body integration tests above.
        let v = build_user_content_with_cache_breakpoint("just text");
        assert!(v.is_string());

        let v = build_user_content_with_cache_breakpoint(
            "PREFIX\n## Concrete inputs\nTAIL",
        );
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"].as_str().unwrap(), "PREFIX\n");
        assert_eq!(arr[0]["cache_control"], json!({ "type": "ephemeral" }));
        assert!(arr[1]["text"].as_str().unwrap().starts_with("## Concrete inputs"));
    }

    #[test]
    fn build_body_emits_forced_tool_use_when_schema_set() {
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
            prompt_cache_key: None,
        };
        let body = p.build_body(ModelTier::Frontier, &req);

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "exactly one tool declared");
        assert_eq!(tools[0]["name"], json!("Answer"));
        assert_eq!(
            tools[0]["input_schema"]["properties"]["answer"]["type"],
            json!("string")
        );

        let tc = &body["tool_choice"];
        assert_eq!(tc["type"], json!("tool"));
        assert_eq!(
            tc["name"],
            json!("Answer"),
            "tool_choice forces the named tool"
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
        let cfg = AnthropicConfig {
            frontier_model: "f".into(),
            workhorse_model: "w".into(),
            cheap_model: "c".into(),
        };
        assert_eq!(cfg.model_for(ModelTier::Frontier), "f");
        assert_eq!(cfg.model_for(ModelTier::Workhorse), "w");
        assert_eq!(cfg.model_for(ModelTier::Cheap), "c");
    }

    #[test]
    fn parse_response_extracts_text_from_text_block() {
        let p = test_provider();
        let raw = json!({
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "model": "claude-haiku-4-5-20251001",
            "content": [
                { "type": "text", "text": "4" }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "output_tokens": 1 }
        });
        let (resp, truncated) = p.parse_response(raw, false).unwrap();
        assert_eq!(resp.text, "4");
        assert!(resp.structured.is_none());
        assert_eq!(resp.provider, "anthropic");
        assert_eq!(resp.model, "claude-haiku-4-5-20251001");
        assert_eq!(resp.input_tokens, Some(10));
        assert_eq!(resp.output_tokens, Some(1));
        assert!(!truncated, "end_turn means not truncated");
    }

    #[test]
    fn parse_response_returns_structured_from_tool_use_block() {
        let p = test_provider();
        let raw = json!({
            "id": "msg_def",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_xyz",
                    "name": "Answer",
                    "input": { "answer": "four" }
                }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 12, "output_tokens": 8 }
        });
        let (resp, truncated) = p.parse_response(raw, true).unwrap();
        assert_eq!(resp.text, "");
        let s = resp.structured.unwrap();
        assert_eq!(s["answer"], json!("four"));
        assert!(!truncated);
    }

    #[test]
    fn parse_response_handles_text_then_tool_use_preamble() {
        // Anthropic can emit a brief text block before a tool_use
        // when the model "reasons aloud" first. Confirm we still
        // extract the structured payload from the tool block rather
        // than failing on the unexpected preamble.
        let p = test_provider();
        let raw = json!({
            "model": "claude-opus-4-7",
            "content": [
                { "type": "text", "text": "Reasoning: 2+2 is four." },
                {
                    "type": "tool_use",
                    "id": "toolu_xyz",
                    "name": "Answer",
                    "input": { "answer": "four" }
                }
            ],
            "stop_reason": "tool_use"
        });
        let (resp, _) = p.parse_response(raw, true).unwrap();
        assert_eq!(resp.text, "Reasoning: 2+2 is four.");
        assert_eq!(resp.structured.unwrap()["answer"], json!("four"));
    }

    #[test]
    fn parse_response_skips_unknown_block_types() {
        // Anthropic adds new block types over time (e.g. `thinking`).
        // Confirm we ignore them without erroring.
        let p = test_provider();
        let raw = json!({
            "model": "claude-opus-4-7",
            "content": [
                { "type": "thinking", "thinking": "hmm" },
                { "type": "text", "text": "result" }
            ],
            "stop_reason": "end_turn"
        });
        let (resp, _) = p.parse_response(raw, false).unwrap();
        assert_eq!(resp.text, "result");
    }

    #[test]
    fn parse_response_signals_truncation_when_stop_reason_is_max_tokens() {
        let p = test_provider();
        let raw = json!({
            "model": "claude-sonnet-4-6",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_xyz",
                    "name": "Answer",
                    "input": { "partial": "field" }
                }
            ],
            "stop_reason": "max_tokens",
            "usage": { "input_tokens": 100, "output_tokens": 8000 }
        });
        let (_resp, truncated) = p.parse_response(raw, true).unwrap();
        assert!(truncated, "max_tokens stop_reason flips the truncated bit");
    }

    #[test]
    fn parse_response_errors_when_schema_requested_but_no_tool_use_came_back() {
        let p = test_provider();
        let raw = json!({
            "model": "claude-sonnet-4-6",
            "content": [
                { "type": "text", "text": "I refuse to call the tool." }
            ],
            "stop_reason": "end_turn"
        });
        let err = p.parse_response(raw, true).unwrap_err();
        // The check uses Api, not SchemaValidation, because
        // SchemaValidation in the trait is for shape mismatches we
        // can't detect at this layer; "wrong block type entirely" is
        // a higher-order Api-level surprise.
        assert!(matches!(err, LlmError::Api(_)), "got {err:?}");
    }

    #[test]
    fn parse_response_does_not_error_on_truncated_schema_request_with_no_tool_use() {
        // Distinct corner from the previous test: a max_tokens cut-off
        // before the model emitted any tool_use. We surface the empty
        // response and let the retry path decide what to do, rather
        // than masking the truncation with a "tool not called" error.
        let p = test_provider();
        let raw = json!({
            "model": "claude-sonnet-4-6",
            "content": [],
            "stop_reason": "max_tokens"
        });
        let (resp, truncated) = p.parse_response(raw, true).unwrap();
        assert!(resp.structured.is_none());
        assert!(truncated);
    }

    #[test]
    fn provider_id_is_stable() {
        assert_eq!(test_provider().id(), "anthropic");
    }

    // -----------------------------------------------------------------
    // env-driven model overrides — same coverage as the xAI tests
    // -----------------------------------------------------------------

    /// Tests that mutate process-wide env vars must serialise. Tokio
    /// tests run in parallel; without this lock, two tests racing on
    /// `ANTHROPIC_*_MODEL_ENV` would observe each other's writes.
    /// Mirrors the lock pattern in the xAI test module.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_model_envs() {
        std::env::remove_var(ANTHROPIC_FRONTIER_MODEL_ENV);
        std::env::remove_var(ANTHROPIC_WORKHORSE_MODEL_ENV);
        std::env::remove_var(ANTHROPIC_CHEAP_MODEL_ENV);
        std::env::remove_var(ANTHROPIC_VERSION_ENV);
    }

    #[test]
    fn anthropic_config_from_env_falls_back_to_default_when_vars_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        let cfg = AnthropicConfig::from_env();
        let defaults = AnthropicConfig::default();
        assert_eq!(cfg.frontier_model, defaults.frontier_model);
        assert_eq!(cfg.workhorse_model, defaults.workhorse_model);
        assert_eq!(cfg.cheap_model, defaults.cheap_model);
    }

    #[test]
    fn anthropic_config_from_env_picks_up_override_when_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(ANTHROPIC_FRONTIER_MODEL_ENV, "test-frontier-override");
        let cfg = AnthropicConfig::from_env();
        assert_eq!(cfg.frontier_model, "test-frontier-override");
        let defaults = AnthropicConfig::default();
        assert_eq!(cfg.workhorse_model, defaults.workhorse_model);
        assert_eq!(cfg.cheap_model, defaults.cheap_model);
        clear_model_envs();
    }

    #[test]
    fn anthropic_config_from_env_treats_empty_string_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(ANTHROPIC_WORKHORSE_MODEL_ENV, "");
        let cfg = AnthropicConfig::from_env();
        assert_eq!(
            cfg.workhorse_model,
            AnthropicConfig::default().workhorse_model
        );
        clear_model_envs();
    }

    #[test]
    fn anthropic_config_from_env_treats_whitespace_only_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_model_envs();
        std::env::set_var(ANTHROPIC_CHEAP_MODEL_ENV, "   \t  ");
        let cfg = AnthropicConfig::from_env();
        assert_eq!(cfg.cheap_model, AnthropicConfig::default().cheap_model);
        clear_model_envs();
    }

    // -----------------------------------------------------------------
    // Truncation-retry predicate — corner-case coverage
    // -----------------------------------------------------------------

    fn fake_completion(text: &str) -> CompletionResponse {
        CompletionResponse {
            text: text.to_string(),
            structured: None,
            provider: "anthropic".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        }
    }

    #[test]
    fn should_retry_truncation_yes_when_all_conditions_match() {
        let first: Result<(CompletionResponse, bool), LlmError> =
            Ok((fake_completion(""), true));
        assert!(should_retry_truncation(&first, true, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_when_request_was_unstructured() {
        // Even when the response truncated, an unstructured request
        // doesn't retry — the user got *something* back, and a bigger
        // budget on the same prompt produces a longer answer that may
        // or may not be wanted. That's a prompt-or-tier decision.
        let first: Result<(CompletionResponse, bool), LlmError> =
            Ok((fake_completion(""), true));
        assert!(!should_retry_truncation(&first, false, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_when_already_at_ceiling() {
        // If max_tokens already equals the retry ceiling, doubling
        // can't change anything; retrying would just burn another
        // round-trip for the same outcome.
        let first: Result<(CompletionResponse, bool), LlmError> =
            Ok((fake_completion(""), true));
        assert!(!should_retry_truncation(&first, true, MAX_RETRY_TOKENS));
    }

    #[test]
    fn should_retry_truncation_no_when_first_succeeded_without_truncation() {
        let first: Result<(CompletionResponse, bool), LlmError> =
            Ok((fake_completion("ok"), false));
        assert!(!should_retry_truncation(&first, true, 8_000));
    }

    #[test]
    fn should_retry_truncation_no_for_errors() {
        let first: Result<(CompletionResponse, bool), LlmError> =
            Err(LlmError::Api("http 500".into()));
        assert!(!should_retry_truncation(&first, true, 8_000));
    }

    // -----------------------------------------------------------------
    // map_http_err — same coverage discipline as xAI's mapping
    // -----------------------------------------------------------------

    #[test]
    fn map_http_err_401_becomes_auth() {
        assert!(matches!(map_http_err(HttpError::Status(401)), LlmError::Auth));
    }

    #[test]
    fn map_http_err_403_becomes_auth() {
        assert!(matches!(map_http_err(HttpError::Status(403)), LlmError::Auth));
    }

    #[test]
    fn map_http_err_429_becomes_rate_limited_with_zero_retry_after() {
        // Until SecureHttpClient surfaces response headers, we report
        // 0 — "unknown". The router can apply its own backoff. When
        // the headers-followup lands, this changes; the test keeps us
        // honest about the current limitation.
        let err = map_http_err(HttpError::Status(429));
        assert!(matches!(
            err,
            LlmError::RateLimited {
                retry_after_seconds: 0,
                ..
            }
        ));
    }

    #[test]
    fn map_http_err_other_5xx_becomes_api() {
        let err = map_http_err(HttpError::Status(503));
        match err {
            LlmError::Api(s) => assert!(s.contains("503"), "got {s}"),
            other => panic!("expected Api, got {other:?}"),
        }
    }

    // Live test — hits real Anthropic. Ignored by default. Run with:
    //   cargo test -p situation_room-llm --ignored live_anthropic
    //
    // `.env` is loaded automatically: put `ANTHROPIC_API_KEY=...` in a
    // `.env` file at the workspace root and it'll be picked up. The
    // key never appears anywhere in this file or in any log line —
    // SecureHttpClient marks the header value sensitive, the
    // logging subscriber scrubs known patterns, and the trace
    // statements above only log the model id and the api version.
    #[tokio::test]
    #[ignore]
    async fn live_anthropic_returns_nonempty_completion() {
        let _ = dotenvy::dotenv();
        let http = test_http();
        let Some(provider) = AnthropicProvider::from_env(http) else {
            panic!(
                "ANTHROPIC_API_KEY not set, empty, placeholder, or too short in env or .env — cannot run live test"
            );
        };
        let req = CompletionRequest {
            system: Some("Reply with a single digit only.".into()),
            user: "What is 2+2?".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let resp = provider
            .complete(ModelTier::Cheap, req)
            .await
            .expect("live anthropic completion should succeed");
        assert!(!resp.text.is_empty(), "response text should not be empty");
        assert!(!resp.model.is_empty(), "response should name the model used");
        assert_eq!(resp.provider, "anthropic");
    }

    #[tokio::test]
    #[ignore]
    async fn live_anthropic_returns_structured_json_when_schema_requested() {
        let _ = dotenvy::dotenv();
        let http = test_http();
        let Some(provider) = AnthropicProvider::from_env(http) else {
            panic!(
                "ANTHROPIC_API_KEY not set in environment or .env — cannot run live test"
            );
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
            system: Some("Answer using the provided structured-output tool.".into()),
            user: "What is 2+2?".into(),
            schema: Some(schema),
            max_tokens: 256,
            temperature: 0.0,
            reasoning_effort: None,
            prompt_cache_key: None,
        };
        let resp = provider
            .complete(ModelTier::Workhorse, req)
            .await
            .expect("live anthropic structured completion should succeed");
        let structured = resp
            .structured
            .expect("structured field should be populated");
        assert!(
            structured.get("result").is_some(),
            "expected result field, got {structured}"
        );
    }
}
