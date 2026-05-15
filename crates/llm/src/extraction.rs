//! Structured-output extraction.
//!
//! Phase-3 minimal landing (Session 77). Per-Document extraction:
//! one LLM call per persisted Document → list of relation-shaped
//! `AssertionDraft`s the pipeline orchestrator persists as
//! [`Assertion`](situation_room_core::Assertion) rows.
//!
//! ## v1 scope (Session 77)
//!
//! The Phase-1 declaration carried [`ExtractionConfig`] and
//! [`ExtractionError`] only. v1 ships the runtime path: build a
//! prompt that embeds the Document body, call the provider with a
//! schema-constrained completion, parse the structured output,
//! validate each emitted item against the closed [`Stance`]
//! vocabulary and the [`EntityId`] newtype, return a Vec of
//! [`AssertionDraft`]s.
//!
//! **Only Relation-shaped assertions in v1.** The handoff sketch
//! named `{claimant, stance, subject, predicate, object,
//! confidence}` — a Subject-Predicate-Object triple shape that maps
//! cleanly to `AssertedContent::Relation`. Observation /
//! EntityAttribute / Event variants can land in a future session
//! once the operator decides whether the dashboard wants them as
//! separate panels or unified under the Assertions count. Keeping
//! v1 to one variant keeps the prompt simple and the validator
//! single-purpose.
//!
//! ## What this module does NOT do
//!
//! - Persist [`Assertion`] rows. That's the pipeline orchestrator's
//!   job (`pipeline::extract::extract_and_persist_assertions`),
//!   which knows the plan + recipe + Document context needed to
//!   build envelopes.
//! - Retry on validation failure. Today we drop invalid items and
//!   return the valid ones. A future session may add a re-prompt
//!   loop using the existing [`ExtractionError::ValidationExhausted`]
//!   variant; today the v1 contract is "single shot, lenient parse,
//!   drop garbage."
//! - Decide which Documents to extract from. The fetch executor
//!   gates on MIME / kind (article-kind, non-empty body) before
//!   calling this module — this keeps the LLM-cost surface bounded
//!   without dragging routing rules into the extractor.

use crate::providers::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ModelTier,
    StructuredOutputSchema,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

/// Configuration for an extraction run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    pub prompt_id: String,
    pub tier: ModelTier,
    pub max_retries: u32,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            prompt_id: "document_assertions".to_string(),
            // Workhorse: extraction is interpretive but mostly
            // mechanical (read the prose, project to schema). Cheap
            // is too small; Frontier costs more than the marginal
            // value per Document.
            tier: ModelTier::Workhorse,
            // v1 doesn't retry — see module docs.
            max_retries: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExtractionError {
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("response did not include structured output")]
    NoStructuredOutput,
    #[error("response failed to parse as the extraction schema: {0}")]
    OutputParse(String),
    #[error("validation failed after {attempts} attempts: {last_error}")]
    ValidationExhausted { attempts: u32, last_error: String },
}

/// Wire shape the LLM emits, before validation. Each item is
/// loosely-typed — we project to typed [`AssertionDraft`] in
/// [`validate_one`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedAssertion {
    /// Who is making the claim. `prefix:slug` shape consumed by
    /// [`EntityId::new`]. The Document publisher / agency is the
    /// usual choice (`agency:reuters`, `agency:sec`,
    /// `company:tsla`).
    pub claimant: String,
    /// Free-form stance. Validated against the closed [`Stance`]
    /// vocabulary in [`validate_one`]; unknown values fail validation
    /// and the item is dropped from the returned Vec.
    pub stance: String,
    /// Source end of the relation triple.
    pub subject: String,
    /// Predicate — becomes `RelationContent::kind`. Free-form
    /// lowercase snake_case (`supplies_to`, `subsidiary_of`,
    /// `subject_to_sanction`). Not validated against a closed
    /// vocab — the Relation schema is open by design (see
    /// `RelationContent::kind` docs).
    pub predicate: String,
    /// Target end of the relation triple.
    pub object: String,
    /// 0.0..=1.0 confidence. Clamped to range in
    /// [`validate_one`] — out-of-range emissions are clipped
    /// rather than dropped (a low-confidence assertion is still an
    /// assertion).
    pub confidence: f64,
}

/// LLM wire envelope for the extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedAssertions {
    #[serde(default)]
    pub assertions: Vec<RawExtractedAssertion>,
}

/// Typed projection of one extracted assertion, ready for the
/// pipeline orchestrator to wrap in an [`Assertion`] envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct AssertionDraft {
    pub claimant: situation_room_core::vocab::EntityId,
    pub stance: situation_room_core::vocab::Stance,
    pub kind: String,
    pub from: situation_room_core::vocab::EntityId,
    pub to: situation_room_core::vocab::EntityId,
    pub confidence: situation_room_core::vocab::Confidence,
}

/// Run one extraction pass against a single Document body.
///
/// `prompt_template` is the contents of
/// `config/prompts/document_assertions.md` — the call site (today:
/// `pipeline::extract::extract_and_persist_assertions`) is
/// responsible for loading it. `body` is the Document's preview
/// body (HTML-stripped per Session 70, ~32 KiB). `source_url` and
/// `mime` give the LLM grounding context.
///
/// The placeholders the prompt template can carry:
///   - `{{SOURCE_URL}}` — the Document's `source_url`.
///   - `{{MIME}}` — the normalised MIME type.
///   - `{{BODY}}` — the body preview.
///   - `{{TOPIC}}` — the plan's topic string (for grounding).
///
/// Returns the **valid** drafts only. Invalid items (bad `EntityId`,
/// unknown stance, empty fields) are warn-logged and dropped. An
/// empty Vec is a legal outcome: the Document had no claims worth
/// extracting (or the LLM emitted nothing).
pub async fn extract_assertions_from_document(
    provider: &dyn LlmProvider,
    cfg: &ExtractionConfig,
    prompt_template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
) -> Result<Vec<AssertionDraft>, ExtractionError> {
    let user = build_extraction_prompt(prompt_template, topic, source_url, mime, body);
    let schema = extraction_schema_value();

    let req = CompletionRequest {
        system: Some(
            "You are the situation_room document-extraction layer. \
             Read the supplied document body and emit only \
             relation-shaped assertions present in the text. \
             Output only JSON conforming to the provided schema. \
             No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(StructuredOutputSchema {
            name: "DocumentAssertions".to_string(),
            schema,
        }),
        // Output is a small JSON list; 2048 covers ~20 assertions
        // comfortably and keeps cost bounded if the LLM gets verbose.
        max_tokens: 2048,
        // Extraction is mechanical: low temperature, deterministic.
        temperature: 0.0,
        // Tier mapping picks the per-tier default; no per-call override.
        reasoning_effort: None,
    };

    let resp = provider.complete(cfg.tier, req).await?;
    let drafts = parse_response(&resp)?;
    Ok(drafts)
}

/// Pure helper: render the prompt template against the call inputs.
/// Lifted out of [`extract_assertions_from_document`] so tests can
/// assert the rendered text contains the substitutions without
/// hitting a network.
pub fn build_extraction_prompt(
    template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
) -> String {
    template
        .replace("{{TOPIC}}", topic)
        .replace("{{SOURCE_URL}}", source_url)
        .replace("{{MIME}}", mime)
        .replace("{{BODY}}", body)
}

/// Parse a [`CompletionResponse`] into validated drafts. Split out
/// for testability: synthetic responses exercise the validation
/// branches without standing up a provider.
pub fn parse_response(resp: &CompletionResponse) -> Result<Vec<AssertionDraft>, ExtractionError> {
    let raw_value = resp
        .structured
        .as_ref()
        .ok_or(ExtractionError::NoStructuredOutput)?;

    let parsed: RawExtractedAssertions = serde_json::from_value(raw_value.clone())
        .map_err(|e| ExtractionError::OutputParse(e.to_string()))?;

    let mut drafts = Vec::with_capacity(parsed.assertions.len());
    for raw in parsed.assertions {
        match validate_one(raw) {
            Ok(draft) => drafts.push(draft),
            Err(reason) => {
                warn!(
                    reason = %reason,
                    "document extractor dropped malformed assertion"
                );
            }
        }
    }
    Ok(drafts)
}

/// Project one [`RawExtractedAssertion`] to [`AssertionDraft`].
/// Drops any item where:
///   - `claimant`, `subject`, `object`, or `predicate` fail to
///     validate / are empty
///   - `stance` doesn't map to the closed [`Stance`] vocabulary
///
/// Confidence outside `0.0..=1.0` is clamped to the nearest edge
/// (matching the [`Confidence`] newtype's `new` semantics for
/// in-range values; out-of-range values would otherwise fail the
/// constructor and drop the whole assertion).
fn validate_one(raw: RawExtractedAssertion) -> Result<AssertionDraft, String> {
    use situation_room_core::vocab::{Confidence, EntityId, Stance};

    let claimant_s = raw.claimant.trim();
    if claimant_s.is_empty() {
        return Err("empty claimant".into());
    }
    let claimant = EntityId::new(claimant_s)
        .map_err(|e| format!("invalid claimant entity id `{claimant_s}`: {e}"))?;

    let stance = parse_stance(&raw.stance)
        .ok_or_else(|| format!("unknown stance `{}`", raw.stance))?;

    let subject_s = raw.subject.trim();
    let object_s = raw.object.trim();
    let predicate_s = raw.predicate.trim();
    if subject_s.is_empty() || object_s.is_empty() || predicate_s.is_empty() {
        return Err("empty subject/predicate/object".into());
    }

    let from = EntityId::new(subject_s)
        .map_err(|e| format!("invalid subject entity id `{subject_s}`: {e}"))?;
    let to = EntityId::new(object_s)
        .map_err(|e| format!("invalid object entity id `{object_s}`: {e}"))?;

    // Confidence::clamp accepts NaN (maps to 0.0) and clamps to
    // [0.0, 1.0]. We prefer it over `Confidence::new` here so a
    // single out-of-range emission doesn't drop the whole
    // assertion (a low-confidence assertion is still an assertion).
    let confidence = Confidence::clamp(raw.confidence as f32);

    Ok(AssertionDraft {
        claimant,
        stance,
        kind: predicate_s.to_string(),
        from,
        to,
        confidence,
    })
}

/// Closed-vocabulary parser for the LLM's `stance` string. Lowercase
/// match; the Display impl on [`Stance`] is the source of truth for
/// wire forms.
fn parse_stance(raw: &str) -> Option<situation_room_core::vocab::Stance> {
    use situation_room_core::vocab::Stance;
    match raw.trim().to_ascii_lowercase().as_str() {
        "asserted" => Some(Stance::Asserted),
        "hedged" => Some(Stance::Hedged),
        "denied" => Some(Stance::Denied),
        "reported" => Some(Stance::Reported),
        "predicted" => Some(Stance::Predicted),
        "speculated" => Some(Stance::Speculated),
        _ => None,
    }
}

/// The JSON Schema constraint we hand to the provider. Kept as a
/// `serde_json::Value` so it can be embedded directly into
/// `StructuredOutputSchema.schema`.
///
/// Hand-written rather than schemars-derived because the wire shape
/// uses plain strings for `claimant` / `subject` / `object` (the
/// `EntityId` newtype isn't `JsonSchema` at the wire level — it's
/// validated server-side in [`validate_one`]). A schemars-derived
/// schema would require a `JsonSchema` impl chain through several
/// types that don't have one today.
fn extraction_schema_value() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "assertions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "claimant": { "type": "string" },
                        "stance": {
                            "type": "string",
                            "enum": [
                                "asserted",
                                "hedged",
                                "denied",
                                "reported",
                                "predicted",
                                "speculated"
                            ]
                        },
                        "subject": { "type": "string" },
                        "predicate": { "type": "string" },
                        "object": { "type": "string" },
                        "confidence": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0
                        }
                    },
                    "required": [
                        "claimant",
                        "stance",
                        "subject",
                        "predicate",
                        "object",
                        "confidence"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["assertions"],
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// Per-Document Event extraction (Session 78)
// ---------------------------------------------------------------------------
//
// Sibling path to relation-shaped assertion extraction. Where the
// relation extractor emits a subject-predicate-object triple per row,
// the event extractor emits `(event_type, headline, actors, direction,
// when)` rows that the pipeline orchestrator wraps in `Event`.
//
// **Closed-vocabulary `event_type`.** The caller hands the extractor
// the plan's declared `event_kinds[].kind` list; the schema bakes
// those as a JSON-Schema `enum` so a schema-respecting provider
// rejects out-of-vocab kinds upstream. The validator defends against
// lax providers by re-checking membership. This mirrors the same
// discipline `relation_synth` enforces on classifier-supplied
// exemplar triples — the dashboard's per-kind panels can only light
// up for kinds the plan declared.

/// Wire shape for one event the LLM emits, before validation. Like
/// [`RawExtractedAssertion`], loosely-typed; the validator
/// ([`validate_event_one`]) projects to typed [`EventDraft`] and
/// drops malformed rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedEvent {
    /// Event type. Must be one of the plan's declared
    /// `event_kinds[].kind` strings (see `extract_events_from_document`'s
    /// `allowed_event_types` argument). Out-of-vocab kinds fail the
    /// closed-vocab gate in [`validate_event_one`].
    pub event_type: String,
    /// One-line description. The dashboard's events panel renders
    /// this as the event's title. Must be non-empty.
    pub headline: String,
    /// Actors involved in the event (acquirer/target for M&A;
    /// company for an earnings release; agency + company for a
    /// regulatory action). `prefix:slug` shape consumed by
    /// [`EntityId::new`]; rows with an invalid actor are dropped
    /// entirely (the actor list is the join key for the dashboard's
    /// entity panel).
    #[serde(default)]
    pub actors: Vec<String>,
    /// Optional supply/demand direction. Closed vocab matching
    /// [`EventDirection`]; unknown values map to `None` (the event
    /// still surfaces, just without a direction tag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Optional ISO-8601 datetime when the event occurred (or is
    /// reported to occur). Parsed via `chrono::DateTime::parse_from_rfc3339`;
    /// parse failures map to `None` and the row still emits with the
    /// fetched-at timestamp as `observed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// 0.0..=1.0 confidence. Clamped in [`validate_event_one`].
    pub confidence: f64,
}

/// LLM wire envelope for the event extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedEvents {
    #[serde(default)]
    pub events: Vec<RawExtractedEvent>,
}

/// Typed projection of one extracted event, ready for the pipeline
/// orchestrator to wrap in an [`Event`](situation_room_core::Event)
/// envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    pub event_type: situation_room_core::vocab::EventType,
    pub headline: String,
    pub actors: Vec<situation_room_core::vocab::EntityId>,
    pub direction: Option<situation_room_core::schema::EventDirection>,
    pub when: Option<DateTime<Utc>>,
    pub confidence: situation_room_core::vocab::Confidence,
}

/// Run one event-extraction pass against a Document body.
///
/// `allowed_event_types` is the plan's declared
/// `event_kinds[].kind` list — the schema enum + the validator's
/// closed-vocab gate both key off it. If the slice is empty the
/// function returns `Ok(vec![])` without calling the provider: a
/// plan that declared no event kinds is a plan that doesn't want
/// events from this Document. Cost-bounded by design.
///
/// Returns **valid** drafts only. Invalid rows (bad `EventType`,
/// out-of-vocab kind, invalid actor `EntityId`, empty headline) are
/// warn-logged and dropped. An empty Vec is a legal outcome.
pub async fn extract_events_from_document(
    provider: &dyn LlmProvider,
    cfg: &ExtractionConfig,
    prompt_template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
    allowed_event_types: &[&str],
) -> Result<Vec<EventDraft>, ExtractionError> {
    if allowed_event_types.is_empty() {
        // No declared event kinds → nothing to extract under the
        // closed-vocab discipline. The orchestrator surfaces this as
        // a silent skip; logs at the call site if the operator wants
        // visibility.
        return Ok(Vec::new());
    }

    let user = build_event_extraction_prompt(
        prompt_template,
        topic,
        source_url,
        mime,
        body,
        allowed_event_types,
    );
    let schema = event_extraction_schema_value(allowed_event_types);

    let req = CompletionRequest {
        system: Some(
            "You are the situation_room document-extraction layer. \
             Read the supplied document body and emit only event \
             records whose event_type is one of the allowed kinds. \
             Output only JSON conforming to the provided schema. \
             No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(StructuredOutputSchema {
            name: "DocumentEvents".to_string(),
            schema,
        }),
        // Output is a small JSON list; 2048 covers ~15 events
        // comfortably with the per-row payload heavier than the
        // relation extractor (headline + actors[] + direction).
        max_tokens: 2048,
        // Extraction is mechanical: low temperature, deterministic.
        temperature: 0.0,
        // Tier mapping picks the per-tier default; no per-call override.
        reasoning_effort: None,
    };

    let resp = provider.complete(cfg.tier, req).await?;
    let drafts = parse_events_response(&resp, allowed_event_types)?;
    Ok(drafts)
}

/// Pure helper: render the event prompt template. Adds the
/// `{{ALLOWED_EVENT_TYPES}}` substitution on top of the four
/// substitutions [`build_extraction_prompt`] already does — the
/// closed-vocab list is comma-separated so the LLM sees it inline
/// in the prompt body, not buried in the JSON schema.
pub fn build_event_extraction_prompt(
    template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
    allowed_event_types: &[&str],
) -> String {
    let allowed_joined = allowed_event_types.join(", ");
    build_extraction_prompt(template, topic, source_url, mime, body)
        .replace("{{ALLOWED_EVENT_TYPES}}", &allowed_joined)
}

/// Parse a [`CompletionResponse`] into validated event drafts. Split
/// out for testability: synthetic responses exercise the
/// closed-vocab + parse branches without standing up a provider.
pub fn parse_events_response(
    resp: &CompletionResponse,
    allowed_event_types: &[&str],
) -> Result<Vec<EventDraft>, ExtractionError> {
    let raw_value = resp
        .structured
        .as_ref()
        .ok_or(ExtractionError::NoStructuredOutput)?;

    let parsed: RawExtractedEvents = serde_json::from_value(raw_value.clone())
        .map_err(|e| ExtractionError::OutputParse(e.to_string()))?;

    let mut drafts = Vec::with_capacity(parsed.events.len());
    for raw in parsed.events {
        match validate_event_one(raw, allowed_event_types) {
            Ok(draft) => drafts.push(draft),
            Err(reason) => {
                warn!(
                    reason = %reason,
                    "document extractor dropped malformed event"
                );
            }
        }
    }
    Ok(drafts)
}

/// Project one [`RawExtractedEvent`] to [`EventDraft`]. Drops the
/// row when:
///   - `event_type` fails [`EventType::new`] (non-snake_case, too
///     long, etc.)
///   - `event_type` is not in `allowed_event_types` (closed-vocab
///     gate; the row's a real event but not one the plan tracks)
///   - `headline` is empty after trim
///   - any `actor` fails [`EntityId::new`]
///
/// `direction` parses leniently — unknown values become `None`
/// without dropping the row. `when` parses RFC-3339; bad input
/// becomes `None`. Confidence is clamped to `[0.0, 1.0]`.
fn validate_event_one(
    raw: RawExtractedEvent,
    allowed_event_types: &[&str],
) -> Result<EventDraft, String> {
    use situation_room_core::vocab::{Confidence, EntityId, EventType};

    let kind_s = raw.event_type.trim();
    if kind_s.is_empty() {
        return Err("empty event_type".into());
    }
    let event_type = EventType::new(kind_s)
        .map_err(|e| format!("invalid event_type `{kind_s}`: {e:?}"))?;
    if !allowed_event_types.iter().any(|k| *k == kind_s) {
        return Err(format!(
            "event_type `{kind_s}` not in plan's declared event_kinds; \
             dropping under closed-vocab discipline"
        ));
    }

    let headline = raw.headline.trim().to_string();
    if headline.is_empty() {
        return Err("empty headline".into());
    }

    let mut actors = Vec::with_capacity(raw.actors.len());
    for actor in raw.actors {
        let s = actor.trim();
        if s.is_empty() {
            // Skip empty entries silently — actors[] is unordered
            // and an interior blank is a hiccup, not a fatal row.
            continue;
        }
        let id = EntityId::new(s)
            .map_err(|e| format!("invalid actor entity id `{s}`: {e}"))?;
        actors.push(id);
    }

    let direction = raw.direction.as_deref().and_then(parse_event_direction);
    let when = raw
        .when
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let confidence = Confidence::clamp(raw.confidence as f32);

    Ok(EventDraft {
        event_type,
        headline,
        actors,
        direction,
        when,
        confidence,
    })
}

/// Closed-vocab parser for the LLM's `direction` string. Lowercase
/// match against the `EventDirection` enum's snake_case wire forms;
/// unknown values return `None` and the caller leaves
/// `EventContent::direction = None`. The Display impl on
/// `EventDirection` (via serde rename_all = "snake_case") is the
/// source of truth for wire forms.
fn parse_event_direction(
    raw: &str,
) -> Option<situation_room_core::schema::EventDirection> {
    use situation_room_core::schema::EventDirection;
    match raw.trim().to_ascii_lowercase().as_str() {
        "supply_positive" => Some(EventDirection::SupplyPositive),
        "supply_negative" => Some(EventDirection::SupplyNegative),
        "demand_positive" => Some(EventDirection::DemandPositive),
        "demand_negative" => Some(EventDirection::DemandNegative),
        "context" => Some(EventDirection::Context),
        _ => None,
    }
}

/// The JSON Schema constraint we hand the provider for event
/// extraction. Bakes `allowed_event_types` as a closed `enum` on
/// the `event_type` field so a schema-respecting provider rejects
/// out-of-vocab kinds upstream. The validator still defends against
/// lax providers (see [`validate_event_one`]).
///
/// `direction` is enum-constrained at the schema level (matches
/// `EventDirection`'s serde wire forms); `when` is a free-form
/// string parsed RFC-3339 in the validator. Hand-written rather
/// than schemars-derived because the wire shape uses plain strings
/// for `actors` (validated server-side).
fn event_extraction_schema_value(allowed_event_types: &[&str]) -> serde_json::Value {
    let allowed_json: Vec<serde_json::Value> = allowed_event_types
        .iter()
        .map(|s| serde_json::Value::String((*s).to_string()))
        .collect();

    serde_json::json!({
        "type": "object",
        "properties": {
            "events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "event_type": {
                            "type": "string",
                            "enum": allowed_json
                        },
                        "headline": { "type": "string" },
                        "actors": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "direction": {
                            "type": "string",
                            "enum": [
                                "supply_positive",
                                "supply_negative",
                                "demand_positive",
                                "demand_negative",
                                "context"
                            ]
                        },
                        "when": { "type": "string" },
                        "confidence": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0
                        }
                    },
                    "required": [
                        "event_type",
                        "headline",
                        "confidence"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["events"],
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// Per-Document Observation extraction (Session 79)
// ---------------------------------------------------------------------------
//
// Third sibling path alongside the relation-shaped assertion extractor
// (Session 77) and the discrete-event extractor (Session 78). Where
// the relation extractor emits SPO triples and the event extractor
// emits dated occurrences, the observation extractor emits
// `(metric, value, unit, period, when)` numeric measurements that the
// pipeline orchestrator wraps in `Observation` rows.
//
// **Closed-vocabulary `metric`.** The caller hands the extractor the
// plan's declared `observation_metrics[].name` list; the schema bakes
// those as a JSON-Schema `enum` so a schema-respecting provider rejects
// out-of-vocab metric names upstream. The validator defends against
// lax providers by re-checking membership. This mirrors the same
// closed-vocab discipline the event extractor enforces — the dashboard's
// per-metric tiles can only light up for metrics the plan declared.

/// Wire shape for one observation the LLM emits, before validation.
/// Like [`RawExtractedEvent`], loosely-typed; the validator
/// ([`validate_observation_one`]) projects to typed
/// [`ObservationDraft`] and drops malformed rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedObservation {
    /// Metric name. Must be one of the plan's declared
    /// `observation_metrics[].name` strings (see
    /// `extract_observations_from_document`'s `allowed_metrics`
    /// argument). Out-of-vocab names fail the closed-vocab gate in
    /// [`validate_observation_one`].
    pub metric: String,
    /// The measured numeric value. Free-form; range checking is left
    /// to the consumer (a negative price should not silently drop —
    /// downstream consensus is responsible for outlier detection).
    pub value: f64,
    /// UCUM-style unit string (`USD/t`, `%`, `t`, `MWh`, `1`).
    /// Validated via [`Unit::new`] in the validator; rows whose unit
    /// fails the constructor are dropped (a value with no unit is
    /// useless downstream).
    pub unit: String,
    /// Optional symmetric uncertainty bound (absolute, same unit as
    /// value). Most narrative documents don't supply uncertainty;
    /// `None` is the common case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_uncertainty: Option<f64>,
    /// Optional ISO 4217 currency code (`USD`, `EUR`, `JPY`).
    /// Validated via [`Currency::new`]; bad values map to `None`
    /// without dropping the row (the unit usually carries currency
    /// info via `USD/t`-style composites, so a malformed standalone
    /// currency is non-fatal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Period the measurement covers. Closed vocabulary matching
    /// [`ObservationPeriod`]'s `instant`/`daily`/`weekly`/`monthly`/
    /// `quarterly`/`annual` snake_case wire forms. Unknown values
    /// drop the row (the period is structurally required on
    /// `ObservationContent` and there is no safe default).
    pub period: String,
    /// Optional ISO-8601 / RFC-3339 datetime the measurement was
    /// taken (or for a forecast, the date the value applies to).
    /// Parsed via `chrono::DateTime::parse_from_rfc3339`; parse
    /// failures map to `None` and the row still emits with the
    /// fetched-at timestamp as `observed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// 0.0..=1.0 confidence. Clamped in [`validate_observation_one`].
    pub confidence: f64,
}

/// LLM wire envelope for the observation extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExtractedObservations {
    #[serde(default)]
    pub observations: Vec<RawExtractedObservation>,
}

/// Typed projection of one extracted observation, ready for the
/// pipeline orchestrator to wrap in an
/// [`Observation`](situation_room_core::schema::records::Observation)
/// envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationDraft {
    pub metric: String,
    pub value: f64,
    pub unit: situation_room_core::vocab::Unit,
    pub value_uncertainty: Option<f64>,
    pub currency: Option<situation_room_core::vocab::Currency>,
    pub period: situation_room_core::schema::content::ObservationPeriod,
    pub when: Option<DateTime<Utc>>,
    pub confidence: situation_room_core::vocab::Confidence,
}

/// Run one observation-extraction pass against a Document body.
///
/// `allowed_metrics` is the plan's declared
/// `observation_metrics[].name` list — the schema enum + the
/// validator's closed-vocab gate both key off it. If the slice is
/// empty the function returns `Ok(vec![])` without calling the
/// provider: a plan that declared no metrics is a plan that doesn't
/// want observations from this Document. Cost-bounded by design,
/// matching the event-extractor posture.
///
/// Returns **valid** drafts only. Invalid rows (out-of-vocab metric,
/// invalid unit, unknown period) are warn-logged and dropped. An
/// empty Vec is a legal outcome.
pub async fn extract_observations_from_document(
    provider: &dyn LlmProvider,
    cfg: &ExtractionConfig,
    prompt_template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
    allowed_metrics: &[&str],
) -> Result<Vec<ObservationDraft>, ExtractionError> {
    if allowed_metrics.is_empty() {
        // No declared metrics → nothing to extract under the
        // closed-vocab discipline. The orchestrator surfaces this as
        // a silent skip; logs at the call site if the operator wants
        // visibility.
        return Ok(Vec::new());
    }

    let user = build_observation_extraction_prompt(
        prompt_template,
        topic,
        source_url,
        mime,
        body,
        allowed_metrics,
    );
    let schema = observation_extraction_schema_value(allowed_metrics);

    let req = CompletionRequest {
        system: Some(
            "You are the situation_room document-extraction layer. \
             Read the supplied document body and emit only numeric \
             observations whose metric is one of the allowed names. \
             Output only JSON conforming to the provided schema. \
             No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(StructuredOutputSchema {
            name: "DocumentObservations".to_string(),
            schema,
        }),
        // Output is a small JSON list; 2048 covers ~25 observations
        // comfortably (per-row payload is a touch lighter than events
        // — no actors[] array — so headroom is similar).
        max_tokens: 2048,
        // Extraction is mechanical: low temperature, deterministic.
        temperature: 0.0,
        // Tier mapping picks the per-tier default; no per-call override.
        reasoning_effort: None,
    };

    let resp = provider.complete(cfg.tier, req).await?;
    let drafts = parse_observations_response(&resp, allowed_metrics)?;
    Ok(drafts)
}

/// Pure helper: render the observation prompt template. Adds the
/// `{{ALLOWED_METRICS}}` substitution on top of the four substitutions
/// [`build_extraction_prompt`] already does — the closed-vocab list
/// is comma-separated so the LLM sees it inline in the prompt body,
/// not buried in the JSON schema.
pub fn build_observation_extraction_prompt(
    template: &str,
    topic: &str,
    source_url: &str,
    mime: &str,
    body: &str,
    allowed_metrics: &[&str],
) -> String {
    let allowed_joined = allowed_metrics.join(", ");
    build_extraction_prompt(template, topic, source_url, mime, body)
        .replace("{{ALLOWED_METRICS}}", &allowed_joined)
}

/// Parse a [`CompletionResponse`] into validated observation drafts.
/// Split out for testability: synthetic responses exercise the
/// closed-vocab + parse branches without standing up a provider.
pub fn parse_observations_response(
    resp: &CompletionResponse,
    allowed_metrics: &[&str],
) -> Result<Vec<ObservationDraft>, ExtractionError> {
    let raw_value = resp
        .structured
        .as_ref()
        .ok_or(ExtractionError::NoStructuredOutput)?;

    let parsed: RawExtractedObservations = serde_json::from_value(raw_value.clone())
        .map_err(|e| ExtractionError::OutputParse(e.to_string()))?;

    let mut drafts = Vec::with_capacity(parsed.observations.len());
    for raw in parsed.observations {
        match validate_observation_one(raw, allowed_metrics) {
            Ok(draft) => drafts.push(draft),
            Err(reason) => {
                warn!(
                    reason = %reason,
                    "document extractor dropped malformed observation"
                );
            }
        }
    }
    Ok(drafts)
}

/// Project one [`RawExtractedObservation`] to [`ObservationDraft`].
/// Drops the row when:
///   - `metric` is empty after trim
///   - `metric` is not in `allowed_metrics` (closed-vocab gate)
///   - `unit` fails [`Unit::new`] (empty, too long, contains
///     whitespace/control chars)
///   - `period` is not one of the closed vocabulary names
///
/// `value_uncertainty` passes through. `currency` parses leniently —
/// bad values become `None` without dropping the row. `when` parses
/// RFC-3339; bad input becomes `None`. Confidence is clamped to
/// `[0.0, 1.0]`.
fn validate_observation_one(
    raw: RawExtractedObservation,
    allowed_metrics: &[&str],
) -> Result<ObservationDraft, String> {
    use situation_room_core::vocab::{Confidence, Currency, Unit};

    let metric_s = raw.metric.trim();
    if metric_s.is_empty() {
        return Err("empty metric".into());
    }
    if !allowed_metrics.iter().any(|m| *m == metric_s) {
        return Err(format!(
            "metric `{metric_s}` not in plan's declared observation_metrics; \
             dropping under closed-vocab discipline"
        ));
    }

    let unit = Unit::new(raw.unit.trim())
        .map_err(|e| format!("invalid unit `{}`: {e:?}", raw.unit))?;

    let period = parse_observation_period(&raw.period)
        .ok_or_else(|| format!("unknown period `{}`", raw.period))?;

    let currency = raw
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| Currency::new(s).ok());

    let when = raw
        .when
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let confidence = Confidence::clamp(raw.confidence as f32);

    Ok(ObservationDraft {
        metric: metric_s.to_string(),
        value: raw.value,
        unit,
        value_uncertainty: raw.value_uncertainty,
        currency,
        period,
        when,
        confidence,
    })
}

/// Closed-vocab parser for the LLM's `period` string. Lowercase
/// match against the snake_case wire forms of
/// [`ObservationPeriod`]'s non-`Custom` variants. We deliberately do
/// not surface the `Custom(String)` variant from this path — letting
/// the LLM emit arbitrary ISO-8601 period strings would widen the
/// extractor's surface area beyond what closed-vocab discipline
/// admits, and downstream rollups would have to special-case the
/// shape. A future session can add a structured `custom_iso8601`
/// emission path if a real source needs it.
fn parse_observation_period(
    raw: &str,
) -> Option<situation_room_core::schema::content::ObservationPeriod> {
    use situation_room_core::schema::content::ObservationPeriod;
    match raw.trim().to_ascii_lowercase().as_str() {
        "instant" => Some(ObservationPeriod::Instant),
        "daily" => Some(ObservationPeriod::Daily),
        "weekly" => Some(ObservationPeriod::Weekly),
        "monthly" => Some(ObservationPeriod::Monthly),
        "quarterly" => Some(ObservationPeriod::Quarterly),
        "annual" => Some(ObservationPeriod::Annual),
        _ => None,
    }
}

/// The JSON Schema constraint we hand the provider for observation
/// extraction. Bakes `allowed_metrics` as a closed `enum` on the
/// `metric` field so a schema-respecting provider rejects out-of-vocab
/// names upstream. The validator still defends against lax providers
/// (see [`validate_observation_one`]).
///
/// `period` is enum-constrained at the schema level (matches
/// `ObservationPeriod`'s non-`Custom` snake_case wire forms);
/// `currency` and `when` are free-form strings parsed in the validator.
fn observation_extraction_schema_value(allowed_metrics: &[&str]) -> serde_json::Value {
    let allowed_json: Vec<serde_json::Value> = allowed_metrics
        .iter()
        .map(|s| serde_json::Value::String((*s).to_string()))
        .collect();

    serde_json::json!({
        "type": "object",
        "properties": {
            "observations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "metric": {
                            "type": "string",
                            "enum": allowed_json
                        },
                        "value": { "type": "number" },
                        "unit": { "type": "string" },
                        "value_uncertainty": { "type": "number" },
                        "currency": { "type": "string" },
                        "period": {
                            "type": "string",
                            "enum": [
                                "instant",
                                "daily",
                                "weekly",
                                "monthly",
                                "quarterly",
                                "annual"
                            ]
                        },
                        "when": { "type": "string" },
                        "confidence": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0
                        }
                    },
                    "required": [
                        "metric",
                        "value",
                        "unit",
                        "period",
                        "confidence"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["observations"],
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use situation_room_core::vocab::Stance;

    #[test]
    fn build_extraction_prompt_substitutes_placeholders() {
        let template = "topic=`{{TOPIC}}` url=`{{SOURCE_URL}}` mime=`{{MIME}}` body=`{{BODY}}`";
        let out = build_extraction_prompt(
            template,
            "lithium",
            "https://example.test/p",
            "text/html",
            "Hello world",
        );
        assert!(out.contains("topic=`lithium`"));
        assert!(out.contains("url=`https://example.test/p`"));
        assert!(out.contains("mime=`text/html`"));
        assert!(out.contains("body=`Hello world`"));
    }

    #[test]
    fn parse_stance_accepts_all_variants() {
        assert!(matches!(parse_stance("asserted"), Some(Stance::Asserted)));
        assert!(matches!(parse_stance("hedged"), Some(Stance::Hedged)));
        assert!(matches!(parse_stance("denied"), Some(Stance::Denied)));
        assert!(matches!(parse_stance("reported"), Some(Stance::Reported)));
        assert!(matches!(parse_stance("predicted"), Some(Stance::Predicted)));
        assert!(matches!(parse_stance("speculated"), Some(Stance::Speculated)));
        // case + whitespace tolerance.
        assert!(matches!(parse_stance("  ASSERTED  "), Some(Stance::Asserted)));
    }

    #[test]
    fn parse_stance_rejects_unknown() {
        assert!(parse_stance("maybe").is_none());
        assert!(parse_stance("").is_none());
    }

    #[test]
    fn validate_one_builds_draft_for_well_formed_input() {
        let raw = RawExtractedAssertion {
            claimant: "agency:reuters".into(),
            stance: "reported".into(),
            subject: "company:panasonic".into(),
            predicate: "supplies_to".into(),
            object: "company:tsla".into(),
            confidence: 0.85,
        };
        let draft = validate_one(raw).expect("should validate");
        assert_eq!(draft.claimant.as_str(), "agency:reuters");
        assert!(matches!(draft.stance, Stance::Reported));
        assert_eq!(draft.kind, "supplies_to");
        assert_eq!(draft.from.as_str(), "company:panasonic");
        assert_eq!(draft.to.as_str(), "company:tsla");
    }

    #[test]
    fn validate_one_drops_unknown_stance() {
        let raw = RawExtractedAssertion {
            claimant: "agency:reuters".into(),
            stance: "wonders".into(),
            subject: "company:a".into(),
            predicate: "supplies_to".into(),
            object: "company:b".into(),
            confidence: 0.8,
        };
        let result = validate_one(raw);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown stance"));
    }

    #[test]
    fn validate_one_drops_empty_predicate() {
        let raw = RawExtractedAssertion {
            claimant: "agency:reuters".into(),
            stance: "asserted".into(),
            subject: "company:a".into(),
            predicate: "   ".into(),
            object: "company:b".into(),
            confidence: 1.0,
        };
        assert!(validate_one(raw).is_err());
    }

    #[test]
    fn validate_one_clamps_confidence_to_unit_range() {
        // 1.5 → 1.0 (clamped); the validator does not drop.
        let raw = RawExtractedAssertion {
            claimant: "agency:reuters".into(),
            stance: "asserted".into(),
            subject: "company:a".into(),
            predicate: "supplies_to".into(),
            object: "company:b".into(),
            confidence: 1.5,
        };
        let draft = validate_one(raw).expect("should clamp not drop");
        assert!((draft.confidence.value() - 1.0_f32).abs() < 1e-6);

        let raw = RawExtractedAssertion {
            claimant: "agency:reuters".into(),
            stance: "asserted".into(),
            subject: "company:a".into(),
            predicate: "supplies_to".into(),
            object: "company:b".into(),
            confidence: -0.5,
        };
        let draft = validate_one(raw).expect("should clamp not drop");
        assert!((draft.confidence.value() - 0.0_f32).abs() < 1e-6);
    }

    #[test]
    fn parse_response_drops_invalid_keeps_valid() {
        // Mixed batch: one good item, one bad stance. parse_response
        // should warn-log the bad one and return the good one.
        let body = serde_json::json!({
            "assertions": [
                {
                    "claimant": "agency:reuters",
                    "stance": "reported",
                    "subject": "company:panasonic",
                    "predicate": "supplies_to",
                    "object": "company:tsla",
                    "confidence": 0.9
                },
                {
                    "claimant": "agency:reuters",
                    "stance": "wonders",
                    "subject": "company:a",
                    "predicate": "x",
                    "object": "company:b",
                    "confidence": 0.5
                }
            ]
        });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let drafts = parse_response(&resp).expect("parse should succeed");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, "supplies_to");
    }

    #[test]
    fn parse_response_fails_when_structured_absent() {
        let resp = CompletionResponse {
            text: "".into(),
            structured: None,
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        match parse_response(&resp) {
            Err(ExtractionError::NoStructuredOutput) => {}
            other => panic!("expected NoStructuredOutput, got: {other:?}"),
        }
    }

    #[test]
    fn parse_response_returns_empty_for_empty_list() {
        let body = serde_json::json!({ "assertions": [] });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let drafts = parse_response(&resp).expect("parse should succeed");
        assert!(drafts.is_empty());
    }

    #[test]
    fn extraction_schema_value_is_object_with_assertions_array() {
        let schema = extraction_schema_value();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["assertions"]["type"], "array");
        // The closed-vocab stance enum is present so a strict
        // schema-respecting provider rejects unknown values upstream;
        // the validator still defends against lax providers.
        assert!(schema["properties"]["assertions"]["items"]["properties"]["stance"]["enum"]
            .is_array());
    }

    #[test]
    fn extraction_config_default_uses_workhorse_tier() {
        let cfg = ExtractionConfig::default();
        assert!(matches!(cfg.tier, ModelTier::Workhorse));
        assert_eq!(cfg.prompt_id, "document_assertions");
        assert_eq!(cfg.max_retries, 0);
    }

    // -------------------------------------------------------------------
    // Per-Document Event extraction tests (Session 78)
    // -------------------------------------------------------------------

    use situation_room_core::schema::EventDirection;

    #[test]
    fn build_event_extraction_prompt_substitutes_all_placeholders() {
        let template = "topic=`{{TOPIC}}` url=`{{SOURCE_URL}}` mime=`{{MIME}}` \
                        body=`{{BODY}}` kinds=`{{ALLOWED_EVENT_TYPES}}`";
        let out = build_event_extraction_prompt(
            template,
            "nvidia stock price",
            "https://example.test/p",
            "text/html",
            "Hello",
            &["earnings_release", "product_launch"],
        );
        assert!(out.contains("topic=`nvidia stock price`"));
        assert!(out.contains("url=`https://example.test/p`"));
        assert!(out.contains("mime=`text/html`"));
        assert!(out.contains("body=`Hello`"));
        assert!(out.contains("kinds=`earnings_release, product_launch`"));
    }

    #[test]
    fn parse_event_direction_accepts_all_variants() {
        assert_eq!(
            parse_event_direction("supply_positive"),
            Some(EventDirection::SupplyPositive)
        );
        assert_eq!(
            parse_event_direction("supply_negative"),
            Some(EventDirection::SupplyNegative)
        );
        assert_eq!(
            parse_event_direction("demand_positive"),
            Some(EventDirection::DemandPositive)
        );
        assert_eq!(
            parse_event_direction("demand_negative"),
            Some(EventDirection::DemandNegative)
        );
        assert_eq!(parse_event_direction("context"), Some(EventDirection::Context));
        // Case + whitespace tolerance.
        assert_eq!(
            parse_event_direction("  SUPPLY_POSITIVE  "),
            Some(EventDirection::SupplyPositive)
        );
    }

    #[test]
    fn parse_event_direction_rejects_unknown() {
        assert!(parse_event_direction("up").is_none());
        assert!(parse_event_direction("").is_none());
    }

    #[test]
    fn validate_event_one_builds_draft_for_well_formed_input() {
        let raw = RawExtractedEvent {
            event_type: "earnings_release".into(),
            headline: "NVIDIA reports record Q4 revenue.".into(),
            actors: vec!["company:nvda".into()],
            direction: Some("demand_positive".into()),
            when: Some("2026-02-21T21:00:00Z".into()),
            confidence: 0.9,
        };
        let allowed = ["earnings_release", "product_launch"];
        let draft = validate_event_one(raw, &allowed).expect("should validate");
        assert_eq!(draft.event_type.as_str(), "earnings_release");
        assert_eq!(draft.headline, "NVIDIA reports record Q4 revenue.");
        assert_eq!(draft.actors.len(), 1);
        assert_eq!(draft.actors[0].as_str(), "company:nvda");
        assert_eq!(draft.direction, Some(EventDirection::DemandPositive));
        assert!(draft.when.is_some());
    }

    #[test]
    fn validate_event_one_drops_out_of_vocab_kind() {
        // The LLM emitted a valid-snake_case event_type but it's
        // not in the plan's declared list — closed-vocab gate must
        // drop the row regardless of how clean the rest looks.
        let raw = RawExtractedEvent {
            event_type: "stock_split".into(),
            headline: "NVIDIA announces 10-for-1 split.".into(),
            actors: vec!["company:nvda".into()],
            direction: None,
            when: None,
            confidence: 0.95,
        };
        let allowed = ["earnings_release", "product_launch"];
        let result = validate_event_one(raw, &allowed);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in plan's declared event_kinds"));
    }

    #[test]
    fn validate_event_one_drops_invalid_event_type() {
        // Non-snake_case kind fails the EventType newtype regardless
        // of whether `allowed_event_types` contains the same shape.
        let raw = RawExtractedEvent {
            event_type: "Earnings Release".into(),
            headline: "x".into(),
            actors: vec![],
            direction: None,
            when: None,
            confidence: 0.5,
        };
        let allowed = ["Earnings Release"];
        assert!(validate_event_one(raw, &allowed).is_err());
    }

    #[test]
    fn validate_event_one_drops_empty_headline() {
        let raw = RawExtractedEvent {
            event_type: "earnings_release".into(),
            headline: "   ".into(),
            actors: vec![],
            direction: None,
            when: None,
            confidence: 0.8,
        };
        let allowed = ["earnings_release"];
        assert!(validate_event_one(raw, &allowed).is_err());
    }

    #[test]
    fn validate_event_one_drops_bad_actor_entity_id() {
        let raw = RawExtractedEvent {
            event_type: "earnings_release".into(),
            headline: "NVIDIA earnings.".into(),
            actors: vec!["malformed entity name".into()],
            direction: None,
            when: None,
            confidence: 0.7,
        };
        let allowed = ["earnings_release"];
        let result = validate_event_one(raw, &allowed);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid actor"));
    }

    #[test]
    fn validate_event_one_tolerates_bad_iso_when_and_unknown_direction() {
        let raw = RawExtractedEvent {
            event_type: "earnings_release".into(),
            headline: "NVIDIA earnings.".into(),
            actors: vec![],
            direction: Some("up_and_to_the_right".into()),
            when: Some("yesterday".into()),
            confidence: 0.6,
        };
        let allowed = ["earnings_release"];
        let draft = validate_event_one(raw, &allowed).expect("should validate");
        // Both bad fields → None; row still emits.
        assert!(draft.direction.is_none());
        assert!(draft.when.is_none());
    }

    #[test]
    fn validate_event_one_clamps_confidence_to_unit_range() {
        let raw = RawExtractedEvent {
            event_type: "earnings_release".into(),
            headline: "NVIDIA earnings.".into(),
            actors: vec![],
            direction: None,
            when: None,
            confidence: 1.7,
        };
        let allowed = ["earnings_release"];
        let draft = validate_event_one(raw, &allowed).expect("should clamp");
        assert!((draft.confidence.value() - 1.0_f32).abs() < 1e-6);
    }

    #[test]
    fn parse_events_response_drops_invalid_keeps_valid() {
        let body = serde_json::json!({
            "events": [
                {
                    "event_type": "earnings_release",
                    "headline": "NVIDIA Q4 earnings.",
                    "actors": ["company:nvda"],
                    "direction": "demand_positive",
                    "when": "2026-02-21T21:00:00Z",
                    "confidence": 0.9
                },
                {
                    // Out-of-vocab kind → drop.
                    "event_type": "stock_split",
                    "headline": "NVIDIA announces 10-for-1 split.",
                    "actors": ["company:nvda"],
                    "confidence": 0.95
                },
                {
                    // Empty headline → drop.
                    "event_type": "product_launch",
                    "headline": "",
                    "actors": [],
                    "confidence": 0.8
                }
            ]
        });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let allowed = ["earnings_release", "product_launch"];
        let drafts = parse_events_response(&resp, &allowed).expect("parse should succeed");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].event_type.as_str(), "earnings_release");
    }

    #[test]
    fn parse_events_response_returns_empty_for_empty_list() {
        let body = serde_json::json!({ "events": [] });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let allowed = ["earnings_release"];
        let drafts = parse_events_response(&resp, &allowed).expect("parse should succeed");
        assert!(drafts.is_empty());
    }

    #[test]
    fn event_extraction_schema_bakes_allowed_kinds_as_enum() {
        let schema = event_extraction_schema_value(&["earnings_release", "product_launch"]);
        let enum_vals =
            &schema["properties"]["events"]["items"]["properties"]["event_type"]["enum"];
        assert!(enum_vals.is_array());
        let arr = enum_vals.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], serde_json::Value::String("earnings_release".into()));
        assert_eq!(arr[1], serde_json::Value::String("product_launch".into()));
    }

    #[test]
    fn extract_events_from_document_with_empty_allowed_returns_empty() {
        // Cost-bounded by design: no declared event kinds → no
        // provider call. We can't easily build a real LlmProvider in
        // a unit test, so we lean on the early-return: if the
        // function returns before touching the provider, an
        // unconstructed null provider would not be hit. Use a
        // helper macro pattern: pass a panic-on-use provider and
        // confirm we don't panic.
        struct PanickyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for PanickyProvider {
            fn id(&self) -> &'static str {
                "panicky"
            }
            fn supported_tiers(&self) -> &[ModelTier] {
                &[ModelTier::Workhorse]
            }
            async fn complete(
                &self,
                _tier: ModelTier,
                _req: CompletionRequest,
            ) -> std::result::Result<CompletionResponse, LlmError> {
                panic!("extract_events_from_document must not call provider when allowed is empty")
            }
        }
        let provider = PanickyProvider;
        let cfg = ExtractionConfig::default();
        // Tokio runtime to drive the async function.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let drafts = rt
            .block_on(extract_events_from_document(
                &provider,
                &cfg,
                "template",
                "topic",
                "url",
                "text/html",
                "body",
                &[],
            ))
            .expect("should succeed without calling provider");
        assert!(drafts.is_empty());
    }

    // -------------------------------------------------------------------
    // Per-Document Observation extraction tests (Session 79)
    // -------------------------------------------------------------------

    use situation_room_core::schema::content::ObservationPeriod;
    use situation_room_core::vocab::{Currency, Unit};

    #[test]
    fn build_observation_extraction_prompt_substitutes_all_placeholders() {
        let template = "topic=`{{TOPIC}}` url=`{{SOURCE_URL}}` mime=`{{MIME}}` \
                        body=`{{BODY}}` metrics=`{{ALLOWED_METRICS}}`";
        let out = build_observation_extraction_prompt(
            template,
            "nvidia stock price",
            "https://example.test/p",
            "text/html",
            "Hello",
            &["price", "volume"],
        );
        assert!(out.contains("topic=`nvidia stock price`"));
        assert!(out.contains("url=`https://example.test/p`"));
        assert!(out.contains("mime=`text/html`"));
        assert!(out.contains("body=`Hello`"));
        assert!(out.contains("metrics=`price, volume`"));
    }

    #[test]
    fn parse_observation_period_accepts_all_closed_variants() {
        assert_eq!(
            parse_observation_period("instant"),
            Some(ObservationPeriod::Instant)
        );
        assert_eq!(
            parse_observation_period("daily"),
            Some(ObservationPeriod::Daily)
        );
        assert_eq!(
            parse_observation_period("weekly"),
            Some(ObservationPeriod::Weekly)
        );
        assert_eq!(
            parse_observation_period("monthly"),
            Some(ObservationPeriod::Monthly)
        );
        assert_eq!(
            parse_observation_period("quarterly"),
            Some(ObservationPeriod::Quarterly)
        );
        assert_eq!(
            parse_observation_period("annual"),
            Some(ObservationPeriod::Annual)
        );
        // Case + whitespace tolerance.
        assert_eq!(
            parse_observation_period("  INSTANT  "),
            Some(ObservationPeriod::Instant)
        );
    }

    #[test]
    fn parse_observation_period_rejects_unknown_and_custom() {
        assert!(parse_observation_period("yearly").is_none());
        assert!(parse_observation_period("").is_none());
        // We deliberately do not surface the `Custom(String)` variant
        // — see parse_observation_period docs.
        assert!(parse_observation_period("P3M").is_none());
    }

    #[test]
    fn validate_observation_one_builds_draft_for_well_formed_input() {
        let raw = RawExtractedObservation {
            metric: "price".into(),
            value: 875.42,
            unit: "USD".into(),
            value_uncertainty: None,
            currency: Some("USD".into()),
            period: "instant".into(),
            when: Some("2026-05-15T16:00:00Z".into()),
            confidence: 0.95,
        };
        let allowed = ["price", "volume"];
        let draft =
            validate_observation_one(raw, &allowed).expect("should validate");
        assert_eq!(draft.metric, "price");
        assert_eq!(draft.value, 875.42);
        assert_eq!(draft.unit, Unit::new("USD").unwrap());
        assert_eq!(draft.currency, Some(Currency::new("USD").unwrap()));
        assert_eq!(draft.period, ObservationPeriod::Instant);
        assert!(draft.when.is_some());
    }

    #[test]
    fn validate_observation_one_drops_out_of_vocab_metric() {
        // The LLM emitted a clean numeric observation but with a
        // metric the plan didn't declare — closed-vocab gate must
        // drop the row regardless of how clean the rest looks.
        let raw = RawExtractedObservation {
            metric: "market_cap".into(),
            value: 4_000_000_000_000.0,
            unit: "USD".into(),
            value_uncertainty: None,
            currency: Some("USD".into()),
            period: "instant".into(),
            when: None,
            confidence: 0.9,
        };
        let allowed = ["price", "volume"];
        let result = validate_observation_one(raw, &allowed);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("not in plan's declared observation_metrics"));
    }

    #[test]
    fn validate_observation_one_drops_invalid_unit() {
        // Empty unit fails Unit::new — values without units can't be
        // joined across sources, so the row is useless downstream.
        let raw = RawExtractedObservation {
            metric: "price".into(),
            value: 100.0,
            unit: "".into(),
            value_uncertainty: None,
            currency: None,
            period: "instant".into(),
            when: None,
            confidence: 0.5,
        };
        let allowed = ["price"];
        assert!(validate_observation_one(raw, &allowed).is_err());
    }

    #[test]
    fn validate_observation_one_drops_unknown_period() {
        let raw = RawExtractedObservation {
            metric: "price".into(),
            value: 100.0,
            unit: "USD".into(),
            value_uncertainty: None,
            currency: None,
            period: "fortnightly".into(),
            when: None,
            confidence: 0.5,
        };
        let allowed = ["price"];
        let result = validate_observation_one(raw, &allowed);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown period"));
    }

    #[test]
    fn validate_observation_one_tolerates_bad_currency_and_when() {
        let raw = RawExtractedObservation {
            metric: "price".into(),
            value: 100.0,
            unit: "USD".into(),
            value_uncertainty: None,
            currency: Some("dollars".into()),
            period: "daily".into(),
            when: Some("yesterday".into()),
            confidence: 0.6,
        };
        let allowed = ["price"];
        let draft =
            validate_observation_one(raw, &allowed).expect("should validate");
        // Both bad fields → None; row still emits.
        assert!(draft.currency.is_none());
        assert!(draft.when.is_none());
    }

    #[test]
    fn validate_observation_one_clamps_confidence_to_unit_range() {
        let raw = RawExtractedObservation {
            metric: "price".into(),
            value: 100.0,
            unit: "USD".into(),
            value_uncertainty: None,
            currency: None,
            period: "instant".into(),
            when: None,
            confidence: 1.7,
        };
        let allowed = ["price"];
        let draft = validate_observation_one(raw, &allowed).expect("should clamp");
        assert!((draft.confidence.value() - 1.0_f32).abs() < 1e-6);
    }

    #[test]
    fn parse_observations_response_drops_invalid_keeps_valid() {
        let body = serde_json::json!({
            "observations": [
                {
                    "metric": "price",
                    "value": 875.42,
                    "unit": "USD",
                    "currency": "USD",
                    "period": "instant",
                    "when": "2026-05-15T16:00:00Z",
                    "confidence": 0.95
                },
                {
                    // Out-of-vocab metric → drop.
                    "metric": "market_cap",
                    "value": 4000000000000.0,
                    "unit": "USD",
                    "period": "instant",
                    "confidence": 0.9
                },
                {
                    // Empty unit → drop.
                    "metric": "volume",
                    "value": 12345678.0,
                    "unit": "",
                    "period": "daily",
                    "confidence": 0.8
                }
            ]
        });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let allowed = ["price", "volume"];
        let drafts =
            parse_observations_response(&resp, &allowed).expect("parse should succeed");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].metric, "price");
    }

    #[test]
    fn parse_observations_response_returns_empty_for_empty_list() {
        let body = serde_json::json!({ "observations": [] });
        let resp = CompletionResponse {
            text: "".into(),
            structured: Some(body),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let allowed = ["price"];
        let drafts =
            parse_observations_response(&resp, &allowed).expect("parse should succeed");
        assert!(drafts.is_empty());
    }

    #[test]
    fn observation_extraction_schema_bakes_allowed_metrics_as_enum() {
        let schema = observation_extraction_schema_value(&["price", "volume"]);
        let enum_vals =
            &schema["properties"]["observations"]["items"]["properties"]["metric"]["enum"];
        assert!(enum_vals.is_array());
        let arr = enum_vals.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], serde_json::Value::String("price".into()));
        assert_eq!(arr[1], serde_json::Value::String("volume".into()));
    }

    #[test]
    fn extract_observations_from_document_with_empty_allowed_returns_empty() {
        // Cost-bounded by design: no declared metrics → no provider
        // call. Mirrors the event-extractor test pattern with a
        // panic-on-use provider; if the early-return holds we never
        // touch the provider.
        struct PanickyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for PanickyProvider {
            fn id(&self) -> &'static str {
                "panicky"
            }
            fn supported_tiers(&self) -> &[ModelTier] {
                &[ModelTier::Workhorse]
            }
            async fn complete(
                &self,
                _tier: ModelTier,
                _req: CompletionRequest,
            ) -> std::result::Result<CompletionResponse, LlmError> {
                panic!(
                    "extract_observations_from_document must not call provider when allowed is empty"
                )
            }
        }
        let provider = PanickyProvider;
        let cfg = ExtractionConfig::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let drafts = rt
            .block_on(extract_observations_from_document(
                &provider,
                &cfg,
                "template",
                "topic",
                "url",
                "text/html",
                "body",
                &[],
            ))
            .expect("should succeed without calling provider");
        assert!(drafts.is_empty());
    }
}
