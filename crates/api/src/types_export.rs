//! TypeScript type generation.
//!
//! Uses `ts-rs` to derive TS types from the wire-shape structs in this
//! module. The frontend imports the generated `.ts` files so type
//! changes in Rust immediately break the TypeScript build.
//!
//! ## Why DTOs and not direct ts-rs on `pipeline::research::ResearchPlan`
//!
//! The pipeline crate owns the typed plan shape. Putting `#[derive(TS)]`
//! on it would force `ts-rs` into the pipeline crate's dependency tree
//! — which is a tooling dep, not a runtime one, and shouldn't infect a
//! crate that runs in the situation-room CLI as well. So this module
//! defines DTOs that mirror the plan one-for-one and converts via
//! `From`. The DTOs are the wire schema; the typed plan is the
//! internal model.
//!
//! ## Where the generated files land
//!
//! Each `#[derive(TS)]` type carries `#[ts(export, export_to = "…")]`
//! pointing at `apps/desktop/src/lib/api/types/`. Running
//! `cargo test --package stockpile-api` triggers ts-rs to write the
//! files (the export hook runs inside a generated test). The Svelte
//! frontend imports from that directory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use stockpile_pipeline::research::{
    DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
    MetricExpectation, RecordExpectations, RelationKindExpectation, ResearchPlan,
};
use stockpile_storage::research_plans::{PlanStatus, StoredResearchPlan};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// PlanStatusDto — wire mirror of storage::PlanStatus
// ---------------------------------------------------------------------------

/// Lifecycle state for a plan, as seen by the frontend. Mirrors
/// [`stockpile_storage::research_plans::PlanStatus`] one-for-one.
///
/// The serde representation is lowercase and unit-tagged
/// (`"pending"` / `"accepted"` / `"rejected"`), matching both the
/// storage column form and the `serde(rename_all = "lowercase")` on
/// the underlying enum. Bend either side and the other follows; the
/// `command_error_dto` shadow-type pattern doesn't apply here because
/// `PlanStatus` is a plain unit enum and ts-rs handles those cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
#[serde(rename_all = "lowercase")]
pub enum PlanStatusDto {
    Pending,
    Accepted,
    Rejected,
}

impl From<PlanStatus> for PlanStatusDto {
    fn from(s: PlanStatus) -> Self {
        match s {
            PlanStatus::Pending => PlanStatusDto::Pending,
            PlanStatus::Accepted => PlanStatusDto::Accepted,
            PlanStatus::Rejected => PlanStatusDto::Rejected,
        }
    }
}

impl From<PlanStatusDto> for PlanStatus {
    fn from(s: PlanStatusDto) -> Self {
        match s {
            PlanStatusDto::Pending => PlanStatus::Pending,
            PlanStatusDto::Accepted => PlanStatus::Accepted,
            PlanStatusDto::Rejected => PlanStatus::Rejected,
        }
    }
}

// ---------------------------------------------------------------------------
// ResearchPlanDto — the full plan shape, on the wire
// ---------------------------------------------------------------------------

/// Wire shape for a research plan. Mirrors
/// [`stockpile_pipeline::research::ResearchPlan`] one-for-one, with
/// the storage-layer audit field [`status`](Self::status) tacked on so
/// the frontend can render the lifecycle pill / accept-reject buttons
/// without a second IPC roundtrip.
///
/// ## Why every nested type is also a DTO
///
/// `ts-rs` generates each `#[derive(TS)]` type independently. If we
/// embedded the pipeline's `MetricExpectation` directly here, it would
/// have to derive `TS` too — pulling ts-rs into the pipeline crate.
/// Mirroring the shape with local types keeps the dependency direction
/// clean: `pipeline` doesn't know about ts-rs; `api` owns the wire
/// schema.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct ResearchPlanDto {
    pub id: String,
    pub topic: String,
    pub interpretation: String,
    pub topic_tags: Vec<String>,
    pub geographic_scope: Vec<GeoScopeDto>,
    pub historical_window_days: u32,
    pub expectations: RecordExpectationsDto,
    pub created_at: DateTime<Utc>,
    pub status: PlanStatusDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct GeoScopeDto {
    pub code: String,
    /// Empty string means "no per-session display preference; render
    /// `code`." Wire form matches `pipeline::research::GeoScope`.
    pub display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RecordExpectationsDto {
    pub observation_metrics: Vec<MetricExpectationDto>,
    pub event_types: Vec<EventTypeExpectationDto>,
    pub entity_kinds: Vec<EntityKindExpectationDto>,
    pub relation_kinds: Vec<RelationKindExpectationDto>,
    pub document_sources: Vec<DocumentSourceHintDto>,
    /// `None` is preserved as `null` on the wire; the frontend renders
    /// the assertion-guidance panel only when present.
    pub assertion_guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct MetricExpectationDto {
    pub name: String,
    /// Unit hint as a plain string (the pipeline's `Unit` newtype's
    /// inner string). Empty when absent.
    pub unit_hint: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct EventTypeExpectationDto {
    pub event_type: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct EntityKindExpectationDto {
    pub kind: String,
    pub exemplars: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RelationKindExpectationDto {
    pub kind: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct DocumentSourceHintDto {
    pub description: String,
    pub preferred_source_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// PlanSummary — lightweight row for the listing screen
// ---------------------------------------------------------------------------

/// Light summary used by the listing screen. Includes per-bucket counts
/// so the listing can show "4 obs · 3 events · 4 entities" without
/// needing to materialize the full plan for every row.
///
/// The counts are computed from the stored JSON columns on the way out.
/// If a column fails to parse, [`PlanSummary::from_stored`] returns an
/// error rather than a misleading zero — better to surface the
/// corruption than hide it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct PlanSummary {
    pub id: String,
    pub topic: String,
    pub created_at: DateTime<Utc>,
    pub status: PlanStatusDto,
    pub topic_tag_count: u32,
    pub observation_count: u32,
    pub event_count: u32,
    pub entity_count: u32,
    pub relation_count: u32,
    pub document_source_count: u32,
}

impl PlanSummary {
    /// Compose a summary from a [`StoredResearchPlan`] by parsing the
    /// `expectations` JSON for bucket counts and the `topic_tags` JSON
    /// for tag count. Geographic scope is omitted from the summary
    /// (it's small enough to render in the full view; including it
    /// here would balloon the list payload).
    pub fn from_stored(s: StoredResearchPlan) -> Result<Self, serde_json::Error> {
        let tags: Vec<String> = serde_json::from_str(&s.topic_tags_json)?;
        let exp: RecordExpectations = serde_json::from_str(&s.expectations_json)?;

        Ok(Self {
            id: s.id.to_string(),
            topic: s.topic,
            created_at: s.created_at,
            status: s.status.into(),
            topic_tag_count: tags.len() as u32,
            observation_count: exp.observation_metrics.len() as u32,
            event_count: exp.event_types.len() as u32,
            entity_count: exp.entity_kinds.len() as u32,
            relation_count: exp.relation_kinds.len() as u32,
            document_source_count: exp.document_sources.len() as u32,
        })
    }
}

// ---------------------------------------------------------------------------
// SourceDescriptorDto — wire shape for the registered source list
// ---------------------------------------------------------------------------

/// Wire shape for a registered source descriptor. Mirrors
/// [`stockpile_pipeline::research_classifier::SourceDescriptor`].
///
/// Currently the frontend doesn't fetch this directly (the binary
/// loads `config/sources.toml` and stuffs descriptors into `AppState`),
/// but the type is exported so a future settings or "sources used
/// by this plan" view has a stable wire schema to import.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct SourceDescriptorDto {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub authoritative_for: Vec<String>,
}

// ---------------------------------------------------------------------------
// CommandError wire shape — re-exported for the generator
// ---------------------------------------------------------------------------

/// Wire-shape mirror of [`crate::commands::CommandError`]. We don't
/// derive TS on the real `CommandError` because `thiserror`'s
/// `#[derive(Error)]` interaction with `#[serde(tag = …)]` plus
/// `ts-rs`'s tagged-enum support is finicky; this shadow type gives
/// the frontend a clean discriminated union to match on.
///
/// Keep in lockstep with `CommandError`. The unit tests in
/// `commands.rs` check the JSON shape; this type is just the TS
/// declaration the frontend consumes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandErrorDto {
    InvalidInput { field: String, message: String },
    ClassificationFailed { message: String },
    Storage { message: String },
    NotFound { id: String },
}

// ---------------------------------------------------------------------------
// From<ResearchPlan> for ResearchPlanDto and friends
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ResearchPlanDto constructors
//
// Two paths to a DTO. We deliberately do NOT impl `From<ResearchPlan>`
// because the typed plan carries no `status` and a blanket `From`
// would have to invent one — which is fine for a freshly-classified
// plan (always Pending) and wrong for any plan re-loaded from
// storage. Forcing the call site to choose its constructor makes the
// implicit choice explicit and audit-greppable.
// ---------------------------------------------------------------------------

impl ResearchPlanDto {
    /// Build a DTO from a freshly-classified typed plan. The caller
    /// asserts (by choosing this constructor) that the plan has just
    /// been written by `save_research_plan`, which always inserts
    /// with `PlanStatus::Pending`.
    ///
    /// Use [`Self::from_stored`] for any plan re-read from storage.
    pub fn from_typed_pending(p: ResearchPlan) -> Self {
        Self::from_typed_with_status(p, PlanStatusDto::Pending)
    }

    /// Build a DTO from a typed plan plus an explicit status. Used
    /// by [`Self::from_stored`]; exposed for tests and any future
    /// caller that has both pieces in hand without going through a
    /// `StoredResearchPlan`.
    pub fn from_typed_with_status(p: ResearchPlan, status: PlanStatusDto) -> Self {
        Self {
            id: p.id.to_string(),
            topic: p.topic,
            interpretation: p.interpretation,
            topic_tags: p.topic_tags.into_iter().map(|t| t.as_str().to_string()).collect(),
            geographic_scope: p.geographic_scope.into_iter().map(GeoScopeDto::from).collect(),
            historical_window_days: p.historical_window_days,
            expectations: RecordExpectationsDto::from(p.expectations),
            created_at: p.created_at,
            status,
        }
    }

    /// Build a DTO from a [`StoredResearchPlan`] — parsing the JSON
    /// columns and carrying the storage-layer `status` through. This
    /// is the path used by `get_plan`, `accept_plan`, `reject_plan`,
    /// and any other command that re-reads a plan from disk.
    pub fn from_stored(s: StoredResearchPlan) -> Result<Self, serde_json::Error> {
        let topic_tags: Vec<stockpile_core::vocab::Topic> =
            serde_json::from_str(&s.topic_tags_json)?;
        let geographic_scope: Vec<GeoScope> =
            serde_json::from_str(&s.geographic_scope_json)?;
        let expectations: RecordExpectations = serde_json::from_str(&s.expectations_json)?;
        let status: PlanStatusDto = s.status.into();

        let plan = ResearchPlan {
            id: s.id,
            topic: s.topic,
            interpretation: s.interpretation,
            topic_tags,
            geographic_scope,
            historical_window_days: s.historical_window_days,
            expectations,
            created_at: s.created_at,
        };
        Ok(Self::from_typed_with_status(plan, status))
    }
}

impl From<GeoScope> for GeoScopeDto {
    fn from(g: GeoScope) -> Self {
        Self {
            code: g.code,
            display: g.display,
        }
    }
}

impl From<RecordExpectations> for RecordExpectationsDto {
    fn from(e: RecordExpectations) -> Self {
        Self {
            observation_metrics: e
                .observation_metrics
                .into_iter()
                .map(MetricExpectationDto::from)
                .collect(),
            event_types: e
                .event_types
                .into_iter()
                .map(EventTypeExpectationDto::from)
                .collect(),
            entity_kinds: e
                .entity_kinds
                .into_iter()
                .map(EntityKindExpectationDto::from)
                .collect(),
            relation_kinds: e
                .relation_kinds
                .into_iter()
                .map(RelationKindExpectationDto::from)
                .collect(),
            document_sources: e
                .document_sources
                .into_iter()
                .map(DocumentSourceHintDto::from)
                .collect(),
            assertion_guidance: e.assertion_guidance,
        }
    }
}

impl From<MetricExpectation> for MetricExpectationDto {
    fn from(m: MetricExpectation) -> Self {
        Self {
            name: m.name,
            unit_hint: m
                .unit_hint
                .map(|u| u.as_str().to_string())
                .unwrap_or_default(),
            rationale: m.rationale,
        }
    }
}

impl From<EventTypeExpectation> for EventTypeExpectationDto {
    fn from(e: EventTypeExpectation) -> Self {
        Self {
            event_type: e.event_type.as_str().to_string(),
            rationale: e.rationale,
        }
    }
}

impl From<EntityKindExpectation> for EntityKindExpectationDto {
    fn from(e: EntityKindExpectation) -> Self {
        Self {
            kind: e.kind,
            exemplars: e.exemplars.into_iter().map(|i| i.as_str().to_string()).collect(),
            rationale: e.rationale,
        }
    }
}

impl From<RelationKindExpectation> for RelationKindExpectationDto {
    fn from(r: RelationKindExpectation) -> Self {
        Self {
            kind: r.kind,
            rationale: r.rationale,
        }
    }
}

impl From<DocumentSourceHint> for DocumentSourceHintDto {
    fn from(h: DocumentSourceHint) -> Self {
        Self {
            description: h.description,
            preferred_source_ids: h.preferred_source_ids,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};
    use stockpile_pipeline::research::{
        DocumentSourceHint as P_DSH, EntityKindExpectation as P_EKE,
        EventTypeExpectation as P_ETE, GeoScope as P_GS, MetricExpectation as P_ME,
        RecordExpectations as P_RE, RelationKindExpectation as P_RKE,
        ResearchPlan as P_RP,
    };

    fn sample_plan() -> P_RP {
        P_RP {
            id: uuid::Uuid::now_v7(),
            topic: "lithium supply chain".into(),
            interpretation: "Production, refining, trade.".into(),
            topic_tags: vec![
                Topic::new("lithium").unwrap(),
                Topic::new("battery_supply_chain").unwrap(),
            ],
            geographic_scope: vec![
                P_GS {
                    code: "AU".into(),
                    display: "Australia".into(),
                },
                P_GS::code_only("CL"),
            ],
            historical_window_days: 730,
            expectations: P_RE {
                observation_metrics: vec![P_ME {
                    name: "production".into(),
                    unit_hint: Some(Unit::new("t").unwrap()),
                    rationale: "Volume metric".into(),
                }],
                event_types: vec![P_ETE {
                    event_type: EventType::new("mine_opened").unwrap(),
                    rationale: "Capacity expansion".into(),
                }],
                entity_kinds: vec![P_EKE {
                    kind: "mine".into(),
                    exemplars: vec![EntityId::new("mine:greenbushes").unwrap()],
                    rationale: "Atomic supply unit".into(),
                }],
                relation_kinds: vec![P_RKE {
                    kind: "operator_of".into(),
                    rationale: "Asset link".into(),
                }],
                document_sources: vec![P_DSH {
                    description: "USGS MCS".into(),
                    preferred_source_ids: vec!["usgs_mcs".into()],
                }],
                assertion_guidance: Some("Prioritize official guidance".into()),
            },
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn plan_dto_round_trips_via_from_typed_pending() {
        let p = sample_plan();
        let id_str = p.id.to_string();
        let topic_count = p.topic_tags.len();
        let obs_count = p.expectations.observation_metrics.len();
        let geo_count = p.geographic_scope.len();

        let dto = ResearchPlanDto::from_typed_pending(p);
        assert_eq!(dto.id, id_str);
        assert_eq!(dto.topic_tags.len(), topic_count);
        assert_eq!(dto.expectations.observation_metrics.len(), obs_count);
        assert_eq!(dto.geographic_scope.len(), geo_count);
        // The "pending" constructor name is load-bearing — guard it.
        assert_eq!(dto.status, PlanStatusDto::Pending);

        // Geo display field is preserved verbatim (including empty for code-only).
        assert_eq!(dto.geographic_scope[0].display, "Australia");
        assert_eq!(dto.geographic_scope[1].display, "");

        // Unit hint is the inner string, not the newtype's debug rep.
        assert_eq!(dto.expectations.observation_metrics[0].unit_hint, "t");
    }

    #[test]
    fn plan_dto_serializes_topic_tags_as_strings() {
        // The frontend treats topic_tags as a plain string array.
        // Guard the wire shape.
        let dto = ResearchPlanDto::from_typed_pending(sample_plan());
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains(r#""topic_tags":["lithium","battery_supply_chain"]"#));
    }

    #[test]
    fn plan_dto_serializes_status_as_lowercase_string() {
        // The frontend pattern-matches on status as a string union;
        // guard the wire shape so a future serde rename can't break it.
        let dto = ResearchPlanDto::from_typed_with_status(sample_plan(), PlanStatusDto::Accepted);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains(r#""status":"accepted""#), "got: {json}");
    }

    #[test]
    fn metric_expectation_dto_omits_unit_when_absent() {
        let m = P_ME {
            name: "x".into(),
            unit_hint: None,
            rationale: "y".into(),
        };
        let dto: MetricExpectationDto = m.into();
        // Empty-string convention matches the broader "absent" wire form
        // documented in the classifier (see SESSION5 handoff #4).
        assert_eq!(dto.unit_hint, "");
    }

    #[test]
    fn plan_summary_counts_buckets_correctly() {
        let p = sample_plan();
        let stored = StoredResearchPlan {
            id: p.id,
            topic: p.topic.clone(),
            interpretation: p.interpretation.clone(),
            topic_tags_json: serde_json::to_string(&p.topic_tags).unwrap(),
            geographic_scope_json: serde_json::to_string(&p.geographic_scope).unwrap(),
            historical_window_days: p.historical_window_days,
            expectations_json: serde_json::to_string(&p.expectations).unwrap(),
            created_at: p.created_at,
            classified_by: "xai".into(),
            status: stockpile_storage::research_plans::PlanStatus::Pending,
        };

        let s = PlanSummary::from_stored(stored).unwrap();
        assert_eq!(s.id, p.id.to_string());
        assert_eq!(s.status, PlanStatusDto::Pending);
        assert_eq!(s.topic_tag_count, 2);
        assert_eq!(s.observation_count, 1);
        assert_eq!(s.event_count, 1);
        assert_eq!(s.entity_count, 1);
        assert_eq!(s.relation_count, 1);
        assert_eq!(s.document_source_count, 1);
    }

    #[test]
    fn plan_summary_from_stored_surfaces_corrupt_json_as_error() {
        let p = sample_plan();
        let stored = StoredResearchPlan {
            id: p.id,
            topic: p.topic.clone(),
            interpretation: p.interpretation.clone(),
            topic_tags_json: "not json".into(),
            geographic_scope_json: serde_json::to_string(&p.geographic_scope).unwrap(),
            historical_window_days: p.historical_window_days,
            expectations_json: serde_json::to_string(&p.expectations).unwrap(),
            created_at: p.created_at,
            classified_by: "xai".into(),
            status: stockpile_storage::research_plans::PlanStatus::Pending,
        };
        assert!(PlanSummary::from_stored(stored).is_err());
    }

    #[test]
    fn plan_dto_from_stored_carries_status_through() {
        // The from_stored path is what get_plan / accept_plan /
        // reject_plan use; status must flow through unmodified or the
        // listing pill and the review-pane badge will lie.
        let p = sample_plan();
        let stored = StoredResearchPlan {
            id: p.id,
            topic: p.topic.clone(),
            interpretation: p.interpretation.clone(),
            topic_tags_json: serde_json::to_string(&p.topic_tags).unwrap(),
            geographic_scope_json: serde_json::to_string(&p.geographic_scope).unwrap(),
            historical_window_days: p.historical_window_days,
            expectations_json: serde_json::to_string(&p.expectations).unwrap(),
            created_at: p.created_at,
            classified_by: "xai".into(),
            status: stockpile_storage::research_plans::PlanStatus::Rejected,
        };
        let dto = ResearchPlanDto::from_stored(stored).unwrap();
        assert_eq!(dto.status, PlanStatusDto::Rejected);
    }

    #[test]
    fn plan_status_dto_round_trips_via_storage_status() {
        use stockpile_storage::research_plans::PlanStatus as S;
        for (storage, dto) in [
            (S::Pending, PlanStatusDto::Pending),
            (S::Accepted, PlanStatusDto::Accepted),
            (S::Rejected, PlanStatusDto::Rejected),
        ] {
            let lifted: PlanStatusDto = storage.into();
            assert_eq!(lifted, dto);
            let back: S = dto.into();
            assert_eq!(back, storage);
        }
    }

    #[test]
    fn command_error_dto_serializes_with_kind_tag() {
        // Mirror of commands.rs test, but on the shadow DTO so the
        // generated TS file's union shape matches the JSON Tauri sends.
        let e = CommandErrorDto::InvalidInput {
            field: "topic".into(),
            message: "too long".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"invalid_input""#));
    }
}
