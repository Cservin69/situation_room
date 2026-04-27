//! Topic research orchestrator.
//!
//! When the user types a topic ("chip production", "uranium enrichment",
//! "rare earth refining outside China"), this module:
//!
//! 1. Asks the LLM to **classify** it — produce a structured
//!    [`ResearchPlan`] whose core is a [`RecordExpectations`]: which of
//!    the six record types are relevant, and what specific metrics /
//!    event types / entity kinds / relation kinds / document sources
//!    apply *within* each of those types.
//! 2. Matches the plan against the source registry to discover which
//!    sources can contribute (and which gaps will remain).
//! 3. Triggers targeted ingestion via the regular pipeline.
//! 4. Surfaces both the plan and its coverage to the UI so users see
//!    what's being researched and where the gaps are.
//!
//! ## The design principle
//!
//! The classifier does **not** invent new schemas or register new
//! "domains" with their own field sets. The six record types
//! (Observation, Event, Entity, Relation, Document, Assertion) are the
//! universal schema. The classifier's job is to *instantiate* that
//! schema for the topic — to say "for 'chip production', the relevant
//! Observations are of metrics like `wafer_starts` and
//! `fab_utilization`; the relevant Events are of types like
//! `fab_announcement` and `export_control_enacted`" — not to invent a
//! new kind of record.
//!
//! This keeps the schema universal while making the research plans
//! rich and topic-specific. See ADR 0007.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ResearchPlan — the classifier's output
// ---------------------------------------------------------------------------

/// A structured plan for researching a free-text topic. Produced by the
/// LLM classifier; consumed by source matching and panel layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchPlan {
    /// Stable identifier for this plan. Threaded into every recipe
    /// authored against it (as `FetchRecipe::plan_id`) and into the
    /// `dedup_key` so re-authoring against the same plan converges
    /// on the same recipe row rather than creating duplicates.
    ///
    /// UUIDv7 per ADR 0003 — sortable, contains the construction time,
    /// fits the same identity convention as records.
    pub id: Uuid,

    /// User's original topic string.
    pub topic: String,

    /// LLM's restated interpretation of the topic, surfaced to the user
    /// for verification before anything is fetched. This is the
    /// single most important UX moment: the user reads this paragraph,
    /// confirms or corrects it, and only then does ingestion begin.
    pub interpretation: String,

    /// The Topic tags that should be attached to every record produced
    /// by this research session. The classifier chooses these, reusing
    /// existing tag strings when appropriate.
    pub topic_tags: Vec<Topic>,

    /// Geographic scope of the research, if any. Each entry carries a
    /// canonical machine code (ISO 3166-1 alpha-2 like `HU`, or a
    /// snake_case region descriptor like `east_asia`) and an optional
    /// human display label produced by the LLM in the session's
    /// chosen register (`Magyarország`, `Hungary`, `Ungarn`).
    ///
    /// Cross-session reasoning, persistence joins, and recipe matching
    /// all key off `code` — the display label is render-only and
    /// never participates in equality, lookup, or aggregation. The
    /// display label persists with the plan so re-rendering it later
    /// preserves the session's voice.
    #[serde(default)]
    pub geographic_scope: Vec<GeoScope>,

    /// How far back should historical ingestion reach?
    pub historical_window_days: u32,

    /// What the session expects to find. Typed against the six record
    /// types so source matching and panel rendering can be structured.
    pub expectations: RecordExpectations,

    /// When the plan was produced.
    pub created_at: DateTime<Utc>,
}

/// One geographic scope entry on a [`ResearchPlan`].
///
/// `code` is canonical and machine-comparable: an ISO 3166-1 alpha-2
/// code (`HU`, `CD`, `BR`) or a `lowercase_snake_case` region label
/// (`east_asia`, `lithium_triangle`, `eu_27`). Every cross-session
/// query, every record subject join, every recipe-author match keys
/// off `code`.
///
/// `display` is the LLM's free-text label for this code, in the
/// session's chosen linguistic register. It may be Hungarian
/// (`Magyarország`), German (`Ungarn`), English (`Hungary`), or any
/// other label the LLM chose to match the topic's voice. An empty
/// `display` means "no per-session preference; render `code`."
///
/// `display` participates in **no** equality, hashing, joining, or
/// vocabulary control. It is render-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeoScope {
    pub code: String,
    /// Empty string is the "no display preference" wire form. See
    /// [`GeoScope`] docs for why this is a `String` and not an
    /// `Option<String>`.
    #[serde(default)]
    pub display: String,
}

impl GeoScope {
    /// Build a scope entry with no display label. Convenience for
    /// tests and code paths that don't care about presentation.
    pub fn code_only(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            display: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// RecordExpectations — typed intents against the six record types
// ---------------------------------------------------------------------------

/// What a research session expects to find, organized by record type.
///
/// The classifier produces this structure; the source-matching step
/// then picks which registered sources can contribute to each bucket.
///
/// Every field is a Vec so the classifier can nominate multiple
/// candidates, and the fields are allowed to be empty (a research
/// session purely about policy events may have no
/// `observation_metrics`, for example). Empty collections are
/// legitimate and indicate "no expectation of this record type."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordExpectations {
    /// Metrics the session expects to capture as Observations.
    /// Examples for "chip production": `wafer_starts`,
    /// `fab_utilization`, `capex`, `process_node_rollout`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_metrics: Vec<MetricExpectation>,

    /// Event types the session expects to capture.
    /// Examples for "chip production": `fab_announcement`,
    /// `equipment_delivery`, `export_control_enacted`,
    /// `supply_chain_disruption`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_types: Vec<EventTypeExpectation>,

    /// Kinds of entities the session expects to track. "Kind" here
    /// matches [`Entity::kind`](stockpile_core::Entity::kind) — the
    /// free-form classifier of what an entity is (`company`, `fab`,
    /// `equipment_vendor`, `government_agency`, etc.). Known exemplars
    /// are listed so source matching can seed ingestion with known
    /// targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_kinds: Vec<EntityKindExpectation>,

    /// Kinds of relations the session expects. Each kind is a free-form
    /// string matching [`RelationContent::kind`](stockpile_core::RelationContent::kind):
    /// `supply_contract`, `fab_operator`, `equipment_source`, `subsidiary`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_kinds: Vec<RelationKindExpectation>,

    /// Document sources the session wants to monitor. Each hint is
    /// matched against registered sources by the source-matching step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_sources: Vec<DocumentSourceHint>,

    /// Notes from the classifier about expected Assertion patterns —
    /// what claims the LLM extraction layer should prioritize extracting
    /// from incoming documents. Free-form; read by the extraction prompt
    /// composer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion_guidance: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-type expectation records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricExpectation {
    /// Canonical metric name (lowercase snake_case), e.g. `wafer_starts`.
    pub name: String,

    /// Unit hint, if the classifier knows it. Helps downstream
    /// normalization match sources that report the same quantity in
    /// different units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_hint: Option<Unit>,

    /// Why this metric matters for the topic. Shown in the UI if the
    /// user inspects the research plan.
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTypeExpectation {
    /// Canonical event type.
    pub event_type: EventType,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityKindExpectation {
    /// Entity kind (e.g. `company`, `fab`, `equipment_vendor`).
    pub kind: String,

    /// Known exemplars the classifier is aware of. Populating these
    /// lets source matching seed tracker lists (e.g. "watch SEC
    /// filings from TSMC, Samsung, Intel").
    #[serde(default)]
    pub exemplars: Vec<EntityId>,

    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationKindExpectation {
    /// Relation kind (e.g. `supply_contract`, `fab_operator`).
    pub kind: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSourceHint {
    /// Human-readable description of the source (e.g. "SEC EDGAR
    /// filings of listed semiconductor companies", "SEMI industry
    /// reports").
    pub description: String,

    /// Source id preferences. If the source-matching step finds a
    /// registered source whose id matches one of these, it's used.
    /// Empty means "match by description against all sources."
    #[serde(default)]
    pub preferred_source_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// CoverageReport — what we can actually fulfill
// ---------------------------------------------------------------------------

/// Coverage report — which parts of a [`ResearchPlan`] have data sources
/// and which don't. Surfaced to the UI alongside the populated panels so
/// the user knows what's missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    pub plan_topic: String,
    pub matched_sources: Vec<MatchedSource>,
    pub gaps: Vec<CoverageGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedSource {
    pub source_id: String,
    /// Which expectations this source covers. Strings identify the
    /// expectation items for logging — the actual wiring is done by
    /// the source-matching step.
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageGap {
    /// What's missing — metric name, event type name, entity kind, etc.
    pub item: String,

    /// What kind of gap it is.
    pub kind: GapKind,

    /// Suggested action the user could take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GapKind {
    /// No registered source covers this item.
    NoSourceAvailable,
    /// A source could cover it but requires an API key the user hasn't set.
    NeedsApiKey {
        source_id: String,
        env_var: String,
    },
    /// Source available but only at low cadence — surface so user knows.
    LowCadenceOnly { source_id: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_plan_roundtrips() {
        let plan = ResearchPlan {
            id: Uuid::now_v7(),
            topic: "chip production".into(),
            interpretation:
                "Research covering semiconductor wafer manufacturing: fab capacity, equipment \
                supply chain, and regulatory environment."
                    .into(),
            topic_tags: vec![
                Topic::new("semiconductors").unwrap(),
                Topic::new("ai_export_controls").unwrap(),
            ],
            geographic_scope: vec![
                GeoScope {
                    code: "TW".into(),
                    display: "Taiwan".into(),
                },
                GeoScope {
                    code: "KR".into(),
                    display: "South Korea".into(),
                },
                GeoScope::code_only("US"),
            ],
            historical_window_days: 365,
            expectations: RecordExpectations {
                observation_metrics: vec![MetricExpectation {
                    name: "wafer_starts".into(),
                    unit_hint: Some(Unit::new("kwspm").unwrap()),
                    rationale: "Proxy for fab utilization trend".into(),
                }],
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("fab_announcement").unwrap(),
                    rationale: "Signals medium-term supply".into(),
                }],
                entity_kinds: vec![EntityKindExpectation {
                    kind: "fab".into(),
                    exemplars: vec![EntityId::new("fab:TSMC-Arizona-F21").unwrap()],
                    rationale: "Fabs are the atomic unit of capacity".into(),
                }],
                relation_kinds: vec![],
                document_sources: vec![DocumentSourceHint {
                    description: "SEC filings of listed semi companies".into(),
                    preferred_source_ids: vec!["sec_edgar".into()],
                }],
                assertion_guidance: Some("Prioritize named-official guidance".into()),
            },
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: ResearchPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, plan.id);
        assert_eq!(back.topic, plan.topic);
        assert_eq!(back.topic_tags.len(), 2);
        assert_eq!(back.expectations.observation_metrics.len(), 1);

        // GeoScope: code is preserved; display survives the round trip
        // when set; empty display also survives.
        assert_eq!(back.geographic_scope.len(), 3);
        assert_eq!(back.geographic_scope[0].code, "TW");
        assert_eq!(back.geographic_scope[0].display, "Taiwan");
        assert_eq!(back.geographic_scope[2].code, "US");
        assert_eq!(back.geographic_scope[2].display, "");
    }

    #[test]
    fn geo_scope_code_only_constructs_with_empty_display() {
        let g = GeoScope::code_only("HU");
        assert_eq!(g.code, "HU");
        assert!(g.display.is_empty());
    }

    #[test]
    fn geo_scope_serializes_with_default_display() {
        // The renderer relies on `display` being present after
        // deserialization (via `#[serde(default)]`) even when the
        // wire form omitted it. This guards that.
        let g: GeoScope = serde_json::from_str(r#"{"code":"HU"}"#).unwrap();
        assert_eq!(g.code, "HU");
        assert_eq!(g.display, "");
    }

    #[test]
    fn coverage_gap_variants_serialize() {
        let gap = CoverageGap {
            item: "fab_utilization".into(),
            kind: GapKind::NoSourceAvailable,
            suggestion: Some("Try SEMI industry reports".into()),
        };
        let json = serde_json::to_string(&gap).unwrap();
        assert!(json.contains("no_source_available"));
        let back: CoverageGap = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.kind, GapKind::NoSourceAvailable));
    }

    #[test]
    fn empty_expectations_valid() {
        // A topic that the classifier determines has no structured-data
        // expectations (e.g. purely narrative research) is still legal.
        let exp = RecordExpectations::default();
        let json = serde_json::to_string(&exp).unwrap();
        assert_eq!(json, "{}");
    }
}
