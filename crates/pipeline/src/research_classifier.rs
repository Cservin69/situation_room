//! Research classification — Level 1 of the research function (ADR 0007).
//!
//! This module asks an LLM to produce a [`ResearchPlan`] given:
//! - the user's free-text topic,
//! - the set of [`Topic`] strings already in use across past sessions
//!   (the existing-topics injection mechanic — see ADR 0007),
//! - a description of the registered sources Stockpile can fetch from
//!   (so the plan's `document_sources` hints reference real source ids
//!   rather than freely-invented ones).
//!
//! The LLM runs **once per session, at session start**. Its output is the
//! single source of truth for "what is this research about." Subsequent
//! Level-2 recipe authoring consumes the plan; the runtime is LLM-free
//! per ADR 0007.
//!
//! ## What this module guarantees
//!
//! - The LLM is called through a `&dyn LlmProvider`, not a concrete
//!   provider. Swapping xAI → Anthropic requires no change here.
//! - The LLM's JSON output is constrained by a schema generated via
//!   `schemars` from [`AuthoredResearchPlan`]. The LLM cannot return
//!   shapes the runtime wouldn't understand.
//! - Every string the LLM returns that maps to a typed vocab newtype
//!   ([`Topic`], [`Unit`], [`EventType`], [`EntityId`]) is constructed
//!   via that newtype's `new(...)` validator. A malformed string is a
//!   classification failure, not a silently-mangled record.
//! - Server-side fields (`id`, `topic`, `created_at`) are stamped by
//!   [`build_validated_plan`]. The LLM never sees these, never invents
//!   them, never echoes a different topic than the user typed.
//! - Structural sanity checks (non-degenerate plan, sane historical
//!   window) catch obviously-broken outputs before they propagate.
//!
//! ## What this module does NOT do
//!
//! - Validate that the plan's *content* is correct or rich. A plan
//!   with one metric for "global semiconductor supply chain" is
//!   structurally valid; it's just a thin classification. Richness is
//!   the prompt's responsibility (see
//!   `config/prompts/research_classifier.md`); this module checks
//!   format, not depth. The user reviews the plan in the UI before
//!   anything downstream runs.
//! - Match the plan against registered sources to produce a coverage
//!   report. That's what Level-2 source matching does, against the
//!   plan this module produces.
//! - Persist the plan. Persistence happens in the caller, alongside
//!   the session.

use chrono::Utc;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use stockpile_core::vocab::{EntityId, EventType, Topic, Unit, VocabError};
use stockpile_llm::{CompletionRequest, LlmError, LlmProvider, ModelTier};
use stockpile_secure::bounds::{check_string, Bounds};
use thiserror::Error;
use uuid::Uuid;

use crate::research::{
    DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
    MetricExpectation, RecordExpectations, RelationKindExpectation, ResearchPlan,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Context the classifier sees alongside the user's topic.
///
/// The caller assembles this from storage (`existing_topics`) and the
/// source registry (`registered_sources`). Keeping the assembly in the
/// caller means the pipeline crate doesn't take a dependency on the
/// sources crate or on storage internals — the classifier just gets a
/// flat list of descriptors.
#[derive(Debug, Clone)]
pub struct ClassificationContext {
    /// Topics already used in past sessions, sorted by frequency
    /// (most-used first). The prompt encourages reuse to keep the
    /// vocabulary cohesive.
    pub existing_topics: Vec<TopicUsage>,

    /// Registered sources Stockpile can fetch from. Surfaced to the
    /// LLM so the plan's `document_sources` hints reference real ids.
    /// An empty list is legal — the plan will then nominate sources
    /// only by description, and the user / Level-2 will resolve.
    pub registered_sources: Vec<SourceDescriptor>,
}

/// One row from a `Store::topics_in_use(...)` query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicUsage {
    pub topic: String,
    pub uses: u64,
}

/// Compact view of a known data source for prompt injection.
///
/// Stockpile no longer carries hand-coded source adapters; the LLM
/// nominates sources from the descriptors the caller supplies here.
/// Callers typically load these from `config/sources.toml` (or
/// equivalent) at the binary layer — the pipeline crate stays
/// agnostic of where the list comes from.
///
/// ## `endpoint_hint` — Session 10, Option F
///
/// `endpoint_hint` is consumed by the **fetch executor's** Level-2
/// authoring step, not by the classifier prompt. It is a stable URL
/// the executor pre-fetches to obtain a real document excerpt before
/// asking the LLM to author a recipe. Without it, the executor falls
/// back to a synthetic placeholder URL — which the Session 9
/// production run revealed the LLM tends to keep verbatim, producing
/// recipes that fetch `example.invalid` at runtime and fail.
///
/// The classifier itself does not render `endpoint_hint` into its
/// prompt: the classifier teaches the LLM via descriptions, and a
/// plan whose `document_sources` reference an id is enough — the
/// runtime side resolves URLs. Keeping `endpoint_hint` invisible to
/// the classifier is deliberate and keeps the two prompts' contracts
/// independent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDescriptor {
    /// Stable id used by recipes (`source_id` in `FetchRecipe`).
    pub id: String,
    /// Human-readable name ("USGS Mineral Commodity Summaries").
    pub display_name: String,
    /// One-paragraph description of the source.
    pub description: String,
    /// Free-form labels for what the source is authoritative on
    /// (e.g. `["production", "reserves"]`). Empty if not declared.
    #[serde(default)]
    pub authoritative_for: Vec<String>,
    /// Stable URL the fetch executor pre-fetches at recipe-authoring
    /// time so the LLM sees a real excerpt of the source's current
    /// shape. Validated through `UrlGuard` at use-time, not at
    /// load-time — a bad URL here produces a clean fallback to the
    /// synthetic-placeholder behaviour, not a hard configuration
    /// error.
    ///
    /// `None` is legal: the executor will synthesize a placeholder
    /// (`https://example.invalid/{id}`) and emit a warning. Sources
    /// for which the LLM already knows the URL pattern (e.g.
    /// well-known APIs documented in the description) can author
    /// usable recipes from a placeholder; sources whose URL the LLM
    /// would need to invent will benefit from setting this.
    #[serde(default)]
    pub endpoint_hint: Option<String>,
}

/// Errors that can arise during classification.
#[derive(Debug, Error)]
pub enum ClassificationError {
    #[error("llm call failed: {0}")]
    Llm(#[from] LlmError),

    #[error("llm returned no structured output (schema ignored?)")]
    NoStructuredOutput,

    #[error("llm output failed to deserialize: {0}")]
    OutputParse(String),

    #[error("plan vocab validation failed: {0}")]
    Vocab(#[from] VocabError),

    #[error("plan structural validation failed: {0}")]
    InvalidPlan(String),

    #[error("prompt construction failed: {0}")]
    Prompt(String),
}

/// Assemble the user-message prompt from a template + runtime inputs.
///
/// The template string must contain `{{TOPIC}}`, `{{EXISTING_TOPICS}}`,
/// and `{{REGISTERED_SOURCES}}` placeholders. Missing placeholders are
/// not errors — they're assumed to be intentional omissions by the
/// prompt author.
///
/// Pure function (no I/O, no LLM call) so tests can assert the
/// rendered prompt contains the expected markers without hitting a
/// network.
pub fn build_prompt(
    template: &str,
    topic: &str,
    ctx: &ClassificationContext,
) -> Result<String, ClassificationError> {
    let existing = render_existing_topics(&ctx.existing_topics);
    let sources = render_registered_sources(&ctx.registered_sources);

    let out = template
        .replace("{{TOPIC}}", topic)
        .replace("{{EXISTING_TOPICS}}", &existing)
        .replace("{{REGISTERED_SOURCES}}", &sources);

    check_string("llm_prompt_user", &out, Bounds::LLM_PROMPT_BODY)
        .map_err(|e| ClassificationError::Prompt(e.to_string()))?;

    Ok(out)
}

/// Classify a free-text topic into a [`ResearchPlan`] by calling the
/// LLM once with the given prompt template and context.
///
/// The prompt template is passed as a string so callers control how
/// they load it (from disk, embedded in the binary, a test literal).
/// The pipeline crate deliberately doesn't reach into the filesystem.
pub async fn classify_topic(
    provider: &dyn LlmProvider,
    tier: ModelTier,
    prompt_template: &str,
    topic: &str,
    ctx: &ClassificationContext,
) -> Result<ResearchPlan, ClassificationError> {
    if topic.trim().is_empty() {
        return Err(ClassificationError::InvalidPlan(
            "topic must be non-empty".into(),
        ));
    }

    let user = build_prompt(prompt_template, topic, ctx)?;

    let schema = schema_for!(AuthoredResearchPlan);
    let schema_value = serde_json::to_value(&schema)
        .map_err(|e| ClassificationError::Prompt(format!("schema serialization: {e}")))?;

    let req = CompletionRequest {
        system: Some(
            "You are the research classifier for Stockpile. Output only JSON \
             conforming to the provided schema. No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(stockpile_llm::providers::StructuredOutputSchema {
            name: "AuthoredResearchPlan".to_string(),
            schema: schema_value,
        }),
        max_tokens: 4096,
        // Low but non-zero: classification is interpretive, not extraction.
        // A little variation produces better topic_tags reuse than a hard
        // greedy decode.
        temperature: 0.2,
    };

    let resp = provider.complete(tier, req).await?;
    let raw = resp.structured.ok_or(ClassificationError::NoStructuredOutput)?;
    let output: AuthoredResearchPlan = serde_json::from_value(raw)
        .map_err(|e| ClassificationError::OutputParse(e.to_string()))?;

    build_validated_plan(output, topic)
}

// ---------------------------------------------------------------------------
// Authoring output shape — what the LLM returns
// ---------------------------------------------------------------------------

/// Subset of [`ResearchPlan`] that the LLM is responsible for producing.
///
/// Server-stamped fields (`id`, `topic`, `created_at`) are filled by
/// [`build_validated_plan`]. The LLM never sees these. In particular,
/// `topic` is the user's verbatim string — the LLM's `interpretation`
/// field is where it gets to restate.
///
/// Vocab newtypes ([`Topic`], [`Unit`], [`EventType`], [`EntityId`])
/// are sent to the LLM as plain strings. Validation walks them through
/// the newtype constructors in [`build_validated_plan`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredResearchPlan {
    /// One-paragraph restatement of the topic. Surfaced to the user as
    /// the trust moment before anything downstream runs.
    pub interpretation: String,

    /// Topic tags as plain strings; validated against [`Topic::new`].
    /// Must contain at least one entry.
    pub topic_tags: Vec<String>,

    /// Geographic scope entries. Each one carries the canonical
    /// machine code and an optional human display label produced in
    /// the session's chosen linguistic register. The schema is
    /// permissive on `code` (just `String`) because regions like
    /// `east_asia` or `lithium_triangle` are legitimate scopes
    /// alongside ISO 3166 alpha-2 codes; the prompt disciplines the
    /// choice toward ISO codes when applicable.
    #[serde(default)]
    pub geographic_scope: Vec<AuthoredGeoScope>,

    /// Historical window in days. Bounded `1..=365 * 50` at validation
    /// time — anything outside is a classification failure.
    pub historical_window_days: u32,

    /// What records the session expects to produce, organized by the
    /// six record types. Empty buckets are legal individually; the
    /// union must be non-empty (an entirely-empty plan is a failed
    /// classification).
    pub expectations: AuthoredRecordExpectations,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredRecordExpectations {
    #[serde(default)]
    pub observation_metrics: Vec<AuthoredMetricExpectation>,
    #[serde(default)]
    pub event_types: Vec<AuthoredEventTypeExpectation>,
    #[serde(default)]
    pub entity_kinds: Vec<AuthoredEntityKindExpectation>,
    #[serde(default)]
    pub relation_kinds: Vec<AuthoredRelationKindExpectation>,
    #[serde(default)]
    pub document_sources: Vec<AuthoredDocumentSourceHint>,
    #[serde(default)]
    pub assertion_guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredMetricExpectation {
    pub name: String,
    #[serde(default)]
    pub unit_hint: Option<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredEventTypeExpectation {
    pub event_type: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredEntityKindExpectation {
    pub kind: String,
    #[serde(default)]
    pub exemplars: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredRelationKindExpectation {
    pub kind: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredDocumentSourceHint {
    pub description: String,
    #[serde(default)]
    pub preferred_source_ids: Vec<String>,
}

/// LLM-facing form of a geographic scope entry. Mirrors
/// [`crate::research::GeoScope`].
///
/// `code` is the canonical machine string (ISO 3166-1 alpha-2 like
/// `HU`, or a `lowercase_snake_case` region label). `display` is the
/// session-register label the LLM produces (`Magyarország`,
/// `Hungary`, `Ungarn`). Empty `display` means "no per-session
/// preference; the renderer falls back to `code`."
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredGeoScope {
    pub code: String,
    #[serde(default)]
    pub display: String,
}

// ---------------------------------------------------------------------------
// Validation + conversion: AuthoredResearchPlan -> ResearchPlan
// ---------------------------------------------------------------------------

/// Maximum sane historical window — fifty years. Beyond this is almost
/// certainly an LLM hallucination ("centuries of context"); bounded so
/// downstream cadence calculations stay reasonable.
const MAX_HISTORICAL_WINDOW_DAYS: u32 = 365 * 50;

/// Maximum length of a [`GeoScope::display`] label. Long enough for
/// "Democratic Republic of the Congo" (32 chars) and most non-Latin
/// renditions; tight enough to discipline the LLM away from prose
/// in a field that's meant to be a label. The bound is enforced
/// in graphemes-approximating chars, not bytes — non-Latin scripts
/// would otherwise be unfairly truncated by a byte cap.
const MAX_GEO_DISPLAY_CHARS: usize = 64;

fn build_validated_plan(
    output: AuthoredResearchPlan,
    user_topic: &str,
) -> Result<ResearchPlan, ClassificationError> {
    // Topic tags: at least one, every entry validates as a Topic.
    if output.topic_tags.is_empty() {
        return Err(ClassificationError::InvalidPlan(
            "plan must contain at least one topic tag".into(),
        ));
    }
    let topic_tags = output
        .topic_tags
        .iter()
        .map(|s| Topic::new(s.as_str()))
        .collect::<Result<Vec<_>, _>>()?;

    // Historical window: sane bounds.
    if output.historical_window_days == 0 {
        return Err(ClassificationError::InvalidPlan(
            "historical_window_days must be >= 1".into(),
        ));
    }
    if output.historical_window_days > MAX_HISTORICAL_WINDOW_DAYS {
        return Err(ClassificationError::InvalidPlan(format!(
            "historical_window_days {} exceeds limit of {MAX_HISTORICAL_WINDOW_DAYS}",
            output.historical_window_days
        )));
    }

    // Geographic scope: per-entry sanity. `code` non-empty; `display`
    // length-bounded; control characters rejected from `display` so
    // the renderer can never receive a label that disrupts a TUI.
    let geographic_scope = convert_geographic_scope(output.geographic_scope)?;

    // Expectations: each typed bucket validates through its vocab newtype.
    let expectations = convert_expectations(output.expectations)?;

    // Non-degenerate: the union of all expectations buckets must be
    // non-empty. An entirely-empty plan is a classification failure
    // (the LLM gave up); a single-bucket plan is fine.
    let total_items = expectations.observation_metrics.len()
        + expectations.event_types.len()
        + expectations.entity_kinds.len()
        + expectations.relation_kinds.len()
        + expectations.document_sources.len();
    if total_items == 0 {
        return Err(ClassificationError::InvalidPlan(
            "plan has no expectations across any bucket — classification failed".into(),
        ));
    }

    // Interpretation: must not be empty. We don't bound length here
    // beyond the prompt's overall body bound — a long interpretation
    // is a minor UX issue, not a classification failure.
    if output.interpretation.trim().is_empty() {
        return Err(ClassificationError::InvalidPlan(
            "interpretation must be non-empty".into(),
        ));
    }

    Ok(ResearchPlan {
        id: Uuid::now_v7(),
        topic: user_topic.to_string(),
        interpretation: output.interpretation,
        topic_tags,
        geographic_scope,
        historical_window_days: output.historical_window_days,
        expectations,
        created_at: Utc::now(),
    })
}

fn convert_geographic_scope(
    raw: Vec<AuthoredGeoScope>,
) -> Result<Vec<GeoScope>, ClassificationError> {
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let code = entry.code.trim().to_string();
        if code.is_empty() {
            return Err(ClassificationError::InvalidPlan(
                "geographic_scope entry has empty code".into(),
            ));
        }

        // Display: empty is the "no preference" wire form. When set,
        // bound the length and reject control characters. We don't
        // enforce a script or language — the whole point is that the
        // LLM picked the register and we trust its choice.
        let display = entry.display.trim().to_string();
        if !display.is_empty() {
            if display.chars().count() > MAX_GEO_DISPLAY_CHARS {
                return Err(ClassificationError::InvalidPlan(format!(
                    "geographic_scope display label for {code:?} is {} chars (limit {MAX_GEO_DISPLAY_CHARS})",
                    display.chars().count()
                )));
            }
            if display.chars().any(|c| c.is_control()) {
                return Err(ClassificationError::InvalidPlan(format!(
                    "geographic_scope display label for {code:?} contains control characters"
                )));
            }
        }

        out.push(GeoScope { code, display });
    }
    Ok(out)
}

fn convert_expectations(
    raw: AuthoredRecordExpectations,
) -> Result<RecordExpectations, ClassificationError> {
    let observation_metrics = raw
        .observation_metrics
        .into_iter()
        .map(|m| {
            let unit_hint = match m.unit_hint {
                Some(s) if !s.is_empty() => Some(Unit::new(s.as_str())?),
                _ => None,
            };
            Ok::<_, ClassificationError>(MetricExpectation {
                name: m.name,
                unit_hint,
                rationale: m.rationale,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let event_types = raw
        .event_types
        .into_iter()
        .map(|e| {
            Ok::<_, ClassificationError>(EventTypeExpectation {
                event_type: EventType::new(e.event_type.as_str())?,
                rationale: e.rationale,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let entity_kinds = raw
        .entity_kinds
        .into_iter()
        .map(|k| {
            let exemplars = k
                .exemplars
                .into_iter()
                .map(|s| EntityId::new(s.as_str()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, ClassificationError>(EntityKindExpectation {
                kind: k.kind,
                exemplars,
                rationale: k.rationale,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let relation_kinds = raw
        .relation_kinds
        .into_iter()
        .map(|r| RelationKindExpectation {
            kind: r.kind,
            rationale: r.rationale,
        })
        .collect();

    let document_sources = raw
        .document_sources
        .into_iter()
        .map(|d| DocumentSourceHint {
            description: d.description,
            preferred_source_ids: d.preferred_source_ids,
        })
        .collect();

    Ok(RecordExpectations {
        observation_metrics,
        event_types,
        entity_kinds,
        relation_kinds,
        document_sources,
        assertion_guidance: raw.assertion_guidance,
    })
}

// ---------------------------------------------------------------------------
// Prompt rendering helpers — pure string formatting
// ---------------------------------------------------------------------------

fn render_existing_topics(topics: &[TopicUsage]) -> String {
    if topics.is_empty() {
        return "(none yet — this is the first session, or no topics have been used)".to_string();
    }
    let mut out = String::new();
    for t in topics {
        // One per line: `topic_string  (used N times)`. The whitespace
        // is intentional — easy to scan, hard to confuse with prose.
        out.push_str(&format!("- {}  (used {} times)\n", t.topic, t.uses));
    }
    out.trim_end().to_string()
}

fn render_registered_sources(sources: &[SourceDescriptor]) -> String {
    if sources.is_empty() {
        return "(no sources currently registered — nominate by description only)".to_string();
    }
    let mut out = String::new();
    for s in sources {
        out.push_str(&format!("- `{}` — {}\n", s.id, s.display_name));
        out.push_str(&format!("  {}\n", s.description.trim()));
        if !s.authoritative_for.is_empty() {
            out.push_str(&format!(
                "  authoritative on: {}\n",
                s.authoritative_for.join(", ")
            ));
        }
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn good_output() -> AuthoredResearchPlan {
        AuthoredResearchPlan {
            interpretation: "Lithium supply chain: production, reserves, refining, \
                             trade flows, and the policy actions affecting them."
                .into(),
            topic_tags: vec!["lithium".into(), "battery_supply_chain".into()],
            geographic_scope: vec![
                AuthoredGeoScope {
                    code: "AU".into(),
                    display: "Australia".into(),
                },
                AuthoredGeoScope {
                    code: "CL".into(),
                    display: "Chile".into(),
                },
                AuthoredGeoScope {
                    code: "CN".into(),
                    display: "".into(),
                },
            ],
            historical_window_days: 730,
            expectations: AuthoredRecordExpectations {
                observation_metrics: vec![AuthoredMetricExpectation {
                    name: "production".into(),
                    unit_hint: Some("t".into()),
                    rationale: "Primary volume metric".into(),
                }],
                event_types: vec![AuthoredEventTypeExpectation {
                    event_type: "mine_opened".into(),
                    rationale: "Capacity expansion signal".into(),
                }],
                entity_kinds: vec![AuthoredEntityKindExpectation {
                    kind: "mine".into(),
                    exemplars: vec!["mine:greenbushes".into()],
                    rationale: "Atomic unit of supply".into(),
                }],
                relation_kinds: vec![AuthoredRelationKindExpectation {
                    kind: "operator_of".into(),
                    rationale: "Operator-asset link".into(),
                }],
                document_sources: vec![AuthoredDocumentSourceHint {
                    description: "USGS Mineral Commodity Summaries".into(),
                    preferred_source_ids: vec!["usgs_mcs".into()],
                }],
                assertion_guidance: None,
            },
        }
    }

    fn sample_ctx() -> ClassificationContext {
        ClassificationContext {
            existing_topics: vec![
                TopicUsage {
                    topic: "lithium".into(),
                    uses: 12,
                },
                TopicUsage {
                    topic: "battery_supply_chain".into(),
                    uses: 5,
                },
            ],
            registered_sources: vec![SourceDescriptor {
                id: "usgs_mcs".into(),
                display_name: "USGS Mineral Commodity Summaries".into(),
                description: "Annual US Geological Survey reports on mineral \
                              production, reserves, and trade."
                    .into(),
                authoritative_for: vec!["production".into(), "reserves".into()],
                endpoint_hint: None,
            }],
        }
    }

    // -----------------------------------------------------------------------
    // Prompt rendering
    // -----------------------------------------------------------------------

    #[test]
    fn render_existing_topics_renders_each_with_count() {
        let s = render_existing_topics(&[
            TopicUsage {
                topic: "lithium".into(),
                uses: 12,
            },
            TopicUsage {
                topic: "cobalt".into(),
                uses: 1,
            },
        ]);
        assert!(s.contains("lithium"));
        assert!(s.contains("12"));
        assert!(s.contains("cobalt"));
    }

    #[test]
    fn render_existing_topics_handles_empty_list() {
        let s = render_existing_topics(&[]);
        assert!(s.contains("first session") || s.contains("no topics"));
    }

    #[test]
    fn render_registered_sources_renders_each() {
        let s = render_registered_sources(&[SourceDescriptor {
            id: "usgs_mcs".into(),
            display_name: "USGS MCS".into(),
            description: "Annual reports.".into(),
            authoritative_for: vec!["production".into()],
            endpoint_hint: None,
        }]);
        assert!(s.contains("usgs_mcs"));
        assert!(s.contains("USGS MCS"));
        assert!(s.contains("production"));
    }

    #[test]
    fn build_prompt_substitutes_all_placeholders() {
        let template = "TOPIC: {{TOPIC}}\nKNOWN: {{EXISTING_TOPICS}}\nSOURCES: {{REGISTERED_SOURCES}}";
        let out = build_prompt(template, "lithium supply chain", &sample_ctx()).unwrap();
        assert!(out.contains("lithium supply chain"));
        assert!(out.contains("usgs_mcs"));
        assert!(!out.contains("{{TOPIC}}"));
        assert!(!out.contains("{{EXISTING_TOPICS}}"));
        assert!(!out.contains("{{REGISTERED_SOURCES}}"));
    }

    // -----------------------------------------------------------------------
    // Validation: happy path
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_plan_accepts_good_output() {
        let plan =
            build_validated_plan(good_output(), "lithium supply chain").unwrap();
        assert_eq!(plan.topic, "lithium supply chain");
        assert_eq!(plan.id.get_version_num(), 7);
        assert_eq!(plan.topic_tags.len(), 2);
        assert_eq!(plan.expectations.observation_metrics.len(), 1);
        assert_eq!(plan.expectations.event_types.len(), 1);
        assert_eq!(plan.geographic_scope.len(), 3);
        // Display labels survive the conversion; empty stays empty.
        assert_eq!(plan.geographic_scope[0].code, "AU");
        assert_eq!(plan.geographic_scope[0].display, "Australia");
        assert_eq!(plan.geographic_scope[2].code, "CN");
        assert_eq!(plan.geographic_scope[2].display, "");
    }

    #[test]
    fn build_validated_plan_stamps_user_topic_verbatim() {
        // The LLM's interpretation may rephrase, but plan.topic must
        // be exactly what the user typed.
        let plan = build_validated_plan(
            good_output(),
            "  lithium supply chain  ", // includes whitespace deliberately
        )
        .unwrap();
        assert_eq!(plan.topic, "  lithium supply chain  ");
    }

    // -----------------------------------------------------------------------
    // Validation: vocab rejection
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_plan_rejects_invalid_topic_tag() {
        let mut out = good_output();
        out.topic_tags = vec!["valid_topic".into(), "Has Spaces".into()];
        let err = build_validated_plan(out, "x").unwrap_err();
        assert!(matches!(err, ClassificationError::Vocab(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_plan_rejects_invalid_event_type() {
        let mut out = good_output();
        out.expectations.event_types[0].event_type = "Bad Event Name".into();
        let err = build_validated_plan(out, "x").unwrap_err();
        assert!(matches!(err, ClassificationError::Vocab(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_plan_rejects_invalid_unit_hint() {
        let mut out = good_output();
        out.expectations.observation_metrics[0].unit_hint = Some("not a unit".into());
        let err = build_validated_plan(out, "x").unwrap_err();
        assert!(matches!(err, ClassificationError::Vocab(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_plan_rejects_invalid_entity_exemplar() {
        let mut out = good_output();
        out.expectations.entity_kinds[0].exemplars = vec!["".into()];
        let err = build_validated_plan(out, "x").unwrap_err();
        assert!(matches!(err, ClassificationError::Vocab(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_plan_accepts_missing_unit_hint() {
        // Some metrics legitimately have no unit (counts, indices that
        // are dimensionless). `None` and missing must both work.
        let mut out = good_output();
        out.expectations.observation_metrics[0].unit_hint = None;
        let plan = build_validated_plan(out, "x").unwrap();
        assert!(plan.expectations.observation_metrics[0].unit_hint.is_none());
    }

    // -----------------------------------------------------------------------
    // Validation: structural
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_plan_rejects_empty_topic_tags() {
        let mut out = good_output();
        out.topic_tags = vec![];
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("topic tag"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_empty_interpretation() {
        let mut out = good_output();
        out.interpretation = "   ".into();
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("interpretation"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_zero_historical_window() {
        let mut out = good_output();
        out.historical_window_days = 0;
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("historical_window_days"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_excessive_historical_window() {
        let mut out = good_output();
        out.historical_window_days = MAX_HISTORICAL_WINDOW_DAYS + 1;
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exceeds limit"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_geographic_scope_with_empty_code() {
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "  ".into(),
            display: "Atlantis".into(),
        }];
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("empty code"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_geographic_scope_display_too_long() {
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "HU".into(),
            display: "X".repeat(MAX_GEO_DISPLAY_CHARS + 1),
        }];
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("display label"), "got {msg}");
        assert!(msg.contains("limit"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_rejects_geographic_scope_display_with_control_char() {
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "HU".into(),
            // Embedded newline — would disrupt a TUI render.
            display: "Magyar\norszag".into(),
        }];
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("control character"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_accepts_geographic_scope_with_non_latin_display() {
        // The whole point of the display field is that the LLM can
        // pick a label in the session's chosen register, including
        // non-Latin scripts. The character-count cap (not byte-count)
        // means non-Latin labels aren't unfairly truncated.
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "HU".into(),
            display: "Magyarország".into(),
        }];
        let plan = build_validated_plan(out, "Hungarian sovereign debt").unwrap();
        assert_eq!(plan.geographic_scope.len(), 1);
        assert_eq!(plan.geographic_scope[0].code, "HU");
        assert_eq!(plan.geographic_scope[0].display, "Magyarország");
    }

    #[test]
    fn build_validated_plan_accepts_geographic_scope_with_empty_display() {
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "US".into(),
            display: "".into(),
        }];
        let plan = build_validated_plan(out, "x").unwrap();
        assert_eq!(plan.geographic_scope[0].code, "US");
        assert_eq!(plan.geographic_scope[0].display, "");
    }

    #[test]
    fn build_validated_plan_accepts_empty_geographic_scope() {
        // Global topics legitimately have no scope at all.
        let mut out = good_output();
        out.geographic_scope = vec![];
        let plan = build_validated_plan(out, "global pandemic preparedness").unwrap();
        assert!(plan.geographic_scope.is_empty());
    }

    #[test]
    fn build_validated_plan_trims_geographic_scope_whitespace() {
        let mut out = good_output();
        out.geographic_scope = vec![AuthoredGeoScope {
            code: "  HU  ".into(),
            display: "  Magyarország  ".into(),
        }];
        let plan = build_validated_plan(out, "x").unwrap();
        assert_eq!(plan.geographic_scope[0].code, "HU");
        assert_eq!(plan.geographic_scope[0].display, "Magyarország");
    }

    #[test]
    fn build_validated_plan_rejects_entirely_empty_expectations() {
        // All buckets empty — this is what a failed classification
        // looks like, and it must not slip through.
        let mut out = good_output();
        out.expectations = AuthoredRecordExpectations::default();
        let err = build_validated_plan(out, "x").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no expectations"), "got {msg}");
    }

    #[test]
    fn build_validated_plan_accepts_single_bucket_filled() {
        // A documents-only plan ("OFAC SDN list updates") is a
        // legitimate classification — only `document_sources` filled.
        let mut out = good_output();
        out.expectations = AuthoredRecordExpectations {
            document_sources: vec![AuthoredDocumentSourceHint {
                description: "OFAC SDN list".into(),
                preferred_source_ids: vec!["ofac_sdn".into()],
            }],
            ..Default::default()
        };
        let plan = build_validated_plan(out, "OFAC SDN list updates").unwrap();
        assert_eq!(plan.expectations.observation_metrics.len(), 0);
        assert_eq!(plan.expectations.document_sources.len(), 1);
    }

    #[test]
    fn build_prompt_handles_empty_topic_string() {
        // The empty-topic short-circuit is in classify_topic, before
        // the network call. build_prompt itself is content-agnostic:
        // it renders whatever it's given. This test guards that the
        // pure rendering path doesn't panic on edge inputs.
        let template = "topic: {{TOPIC}}";
        let ctx = sample_ctx();
        let s = build_prompt(template, "", &ctx).unwrap();
        assert!(s.contains("topic:"));
    }

    // -----------------------------------------------------------------------
    // Schema generation sanity
    // -----------------------------------------------------------------------

    #[test]
    fn schema_for_authored_plan_is_producible() {
        let schema = schema_for!(AuthoredResearchPlan);
        let json = serde_json::to_value(&schema).expect("schema must serialize");
        let s = json.to_string();
        assert!(s.contains("interpretation"), "schema missing interpretation");
        assert!(s.contains("topic_tags"), "schema missing topic_tags");
        assert!(s.contains("expectations"), "schema missing expectations");
        assert!(s.contains("historical_window_days"));
        assert!(s.contains("observation_metrics"));
        assert!(s.contains("event_types"));
        assert!(s.contains("entity_kinds"));
        assert!(s.contains("relation_kinds"));
        assert!(s.contains("document_sources"));
    }

    // -----------------------------------------------------------------------
    // Authored* mirror serde-equivalence with the runtime types.
    //
    // We can't make these `assert_eq!` on serialized JSON because the
    // runtime types use vocab newtypes (`Topic`, `EventType`) that
    // serialize to plain strings — same wire shape. The test is that a
    // round-trip through Authored* and back is lossless for any plan
    // a competent LLM would produce.
    // -----------------------------------------------------------------------

    #[test]
    fn authored_plan_round_trips_through_validation() {
        let plan = build_validated_plan(good_output(), "lithium supply chain").unwrap();
        // Re-serialize and check the runtime fields all parse.
        let j = serde_json::to_value(&plan).unwrap();
        let back: ResearchPlan = serde_json::from_value(j).unwrap();
        assert_eq!(back.topic, plan.topic);
        assert_eq!(back.id, plan.id);
        assert_eq!(back.topic_tags.len(), plan.topic_tags.len());
        assert_eq!(
            back.expectations.observation_metrics.len(),
            plan.expectations.observation_metrics.len()
        );
    }

    // -----------------------------------------------------------------------
    // Live LLM test — structural assertions only.
    //
    // Mirrors the recipe_author live-test posture: the prompt's content
    // discipline (ISO codes, named entities, source priority) is not
    // asserted in code — those checks are empirical and live in the
    // human review of the rendered plan in the UI. The test asserts:
    //   - the call returns a structurally-valid ResearchPlan,
    //   - all vocab strings parse,
    //   - the plan is non-degenerate (the union of buckets is non-empty),
    //   - the user's topic survives verbatim.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore]
    async fn live_classify_topic_against_xai_produces_valid_plan() {
        use stockpile_llm::XaiProvider;
        use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();
        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();
        let Some(provider) = XaiProvider::from_env(http) else {
            panic!("XAI_API_KEY not set in environment or .env — cannot run live test");
        };

        // Test-scoped minimal template. Production loads the real
        // markdown via include_str! at the binary layer.
        let template = "\
            You are the research classifier for Stockpile.\n\
            TOPIC: {{TOPIC}}\n\
            EXISTING TOPICS:\n{{EXISTING_TOPICS}}\n\
            REGISTERED SOURCES:\n{{REGISTERED_SOURCES}}\n\
            Return JSON conforming to AuthoredResearchPlan. Use lowercase \
            snake_case for topic_tags and event_type. Include at least one \
            entry across the expectations buckets. For geographic_scope \
            entries, use ISO 3166-1 alpha-2 codes when applicable, and \
            provide a human-readable display label.\
        ";

        let ctx = sample_ctx();
        let topic = "lithium supply chain";

        let plan = classify_topic(&provider, ModelTier::Workhorse, template, topic, &ctx)
            .await
            .expect("live classification should succeed");

        // Structural assertions only.
        assert_eq!(plan.topic, topic, "user topic must survive verbatim");
        assert!(!plan.topic_tags.is_empty(), "must have >=1 topic tag");
        assert!(!plan.interpretation.trim().is_empty());
        let total = plan.expectations.observation_metrics.len()
            + plan.expectations.event_types.len()
            + plan.expectations.entity_kinds.len()
            + plan.expectations.relation_kinds.len()
            + plan.expectations.document_sources.len();
        assert!(total > 0, "plan must be non-degenerate");
        assert!(plan.historical_window_days >= 1);
        assert!(plan.historical_window_days <= MAX_HISTORICAL_WINDOW_DAYS);
        // The plan id was minted server-side, not echoed by the LLM.
        assert_eq!(plan.id.get_version_num(), 7);
    }
}
