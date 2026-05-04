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
//! `cargo test --package situation_room-api` triggers ts-rs to write the
//! files (the export hook runs inside a generated test). The Svelte
//! frontend imports from that directory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use situation_room_pipeline::research::{
    DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
    MetricExpectation, RecordExpectations, RelationKindExpectation, ResearchPlan,
};
use situation_room_storage::research_plans::{PlanStatus, StoredResearchPlan};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// PlanStatusDto — wire mirror of storage::PlanStatus
// ---------------------------------------------------------------------------

/// Lifecycle state for a plan, as seen by the frontend. Mirrors
/// [`situation_room_storage::research_plans::PlanStatus`] one-for-one.
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
/// [`situation_room_pipeline::research::ResearchPlan`] one-for-one, with
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
    /// Free-text note the user attached when rejecting. Empty string
    /// for plans that were never rejected, plans rejected before
    /// Session 15, and rejections where the user supplied no note.
    /// Empty-string-as-absent matches the wire convention for
    /// optional strings throughout the api crate (see `unit_hint`,
    /// `display`, etc.); we don't introduce a separate `null` shape
    /// here for parity.
    #[serde(default)]
    pub rejection_reason: String,
    /// UUID of the rejected plan that prompted this re-classification,
    /// as a string. Empty string when this plan was not produced by
    /// the re-classify-with-feedback flow. Stringly-typed on the wire
    /// for the same reason `id` is — ts-rs doesn't have a Uuid
    /// primitive at the TS side.
    #[serde(default)]
    pub reclassified_from: String,
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
    /// True when this plan has a non-empty `rejection_reason`. Lets
    /// the listing show a "has note" indicator on rejected rows
    /// without dragging the full text through the summary payload —
    /// the text itself is fetched on demand via `get_plan`.
    #[serde(default)]
    pub has_rejection_reason: bool,
    /// True when this plan was produced by re-classifying a
    /// previously-rejected plan. Same payload-trimming rationale as
    /// `has_rejection_reason`: the predecessor's id (when needed for
    /// chain navigation) lives on the full plan DTO.
    #[serde(default)]
    pub is_reclassified: bool,
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
        let has_rejection_reason = s
            .rejection_reason
            .as_deref()
            .map(|r| !r.trim().is_empty())
            .unwrap_or(false);
        let is_reclassified = s.reclassified_from.is_some();

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
            has_rejection_reason,
            is_reclassified,
        })
    }
}

// ---------------------------------------------------------------------------
// SourceDescriptorDto — wire shape for the registered source list
// ---------------------------------------------------------------------------

/// Wire shape for a registered source descriptor. Mirrors
/// [`situation_room_pipeline::research_classifier::SourceDescriptor`].
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
    FetchFailed {
        recipes_attempted: u32,
        recipes_succeeded: u32,
        message: String,
    },
    /// Track A, ADR 0012 amendment 1: the manual re-author command
    /// failed before producing a new recipe. Distinct from
    /// `FetchFailed` because the frontend renders the two
    /// differently — `FetchFailed` lives in the fetch-report panel;
    /// re-author failures live in the dialog the operator just
    /// closed. The `prior_recipe_id` lets the dialog tell the
    /// operator which recipe didn't get superseded.
    ReauthorFailed {
        prior_recipe_id: String,
        message: String,
    },
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
    ///
    /// `rejection_reason` and `reclassified_from` are not part of the
    /// typed `ResearchPlan` (they're storage-layer audit fields), so
    /// this constructor leaves them blank. Use [`Self::from_stored`]
    /// for any plan re-read from disk; that path carries the audit
    /// fields through.
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
            rejection_reason: String::new(),
            reclassified_from: String::new(),
        }
    }

    /// Build a DTO from a [`StoredResearchPlan`] — parsing the JSON
    /// columns and carrying the storage-layer `status`,
    /// `rejection_reason`, and `reclassified_from` through. This is
    /// the path used by `get_plan`, `accept_plan`, `reject_plan`,
    /// `reclassify_plan`, and any other command that re-reads a plan
    /// from disk.
    pub fn from_stored(s: StoredResearchPlan) -> Result<Self, serde_json::Error> {
        let topic_tags: Vec<situation_room_core::vocab::Topic> =
            serde_json::from_str(&s.topic_tags_json)?;
        let geographic_scope: Vec<GeoScope> =
            serde_json::from_str(&s.geographic_scope_json)?;
        let expectations: RecordExpectations = serde_json::from_str(&s.expectations_json)?;
        let status: PlanStatusDto = s.status.into();
        let rejection_reason = s.rejection_reason.unwrap_or_default();
        let reclassified_from = s
            .reclassified_from
            .map(|u| u.to_string())
            .unwrap_or_default();

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
        let mut dto = Self::from_typed_with_status(plan, status);
        dto.rejection_reason = rejection_reason;
        dto.reclassified_from = reclassified_from;
        Ok(dto)
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
// Fetch executor DTOs (Session 8)
// ---------------------------------------------------------------------------

/// Wire shape for one fetch run's outcome. Mirrors
/// [`situation_room_pipeline::fetch_executor::FetchReport`] one-for-one.
///
/// Returned synchronously by the `run_fetch_for_plan` command. The
/// frontend renders it in the review pane so the user sees, in one
/// place, what each recipe produced.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct FetchReportDto {
    pub plan_id: String,
    pub run_id: String,
    pub outcomes: Vec<RecipeOutcomeDto>,
    pub recipes_attempted: u32,
    pub recipes_succeeded: u32,
    pub records_produced: u32,
    pub error_summary: Option<String>,
}

/// One per-recipe outcome on the wire. Discriminated union with the
/// same shape as the Rust enum — the frontend pattern-matches on
/// `kind`.
///
/// The discriminator naming follows the codebase's existing `kind`
/// convention (see `CommandErrorDto`). Adding a new outcome variant
/// is an additive change; the frontend's `kind` switch must add the
/// new arm or the type-checker will flag the missing case.
///
/// ## Variants
///
/// - `succeeded` — the recipe ran end-to-end and produced records.
/// - `skipped` — the executor declined to run the recipe (e.g.
///   extraction mode not yet enabled in the runtime).
/// - `failed` — the recipe ran and broke at a named stage. The
///   `stage` discriminator lets the UI render per-stage hints.
/// - `rate_limited` — Track D, Session 25. The source returned 429
///   in a way the executor's inline backoff didn't wait through:
///   either `Retry-After` exceeded the short-backoff ceiling, or no
///   `Retry-After` was provided. The frontend renders this in
///   warning amber to distinguish it from `failed` red — re-running
///   later is meaningful for a rate-limit, pointless for a broken
///   recipe.
/// - `declined` — Track B, Session 28, ADR 0007 amendment 4. The
///   recipe-author LLM declined to write a recipe for this source
///   and explained why. **No `recipe_id`** — no recipe was created.
///   The frontend renders this in a distinct tone (`'declined'`) so
///   the operator sees an authoring-stage decision, not a runtime
///   failure. Remediation is editorial: drop the source, find an
///   alternative, escalate the model tier — re-running the same
///   inputs gets the same decline.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeOutcomeDto {
    Succeeded {
        recipe_id: String,
        source_id: String,
        records_produced: u32,
    },
    Skipped {
        recipe_id: String,
        source_id: String,
        reason: String,
    },
    Failed {
        recipe_id: String,
        source_id: String,
        stage: FailureStageDto,
        message: String,
    },
    RateLimited {
        recipe_id: String,
        source_id: String,
        /// Parsed `Retry-After` value in seconds, when the server
        /// supplied one (per RFC 9110 §10.2.3). `None` means the
        /// server returned 429 with no machine-readable hint. The
        /// frontend formats both cases via `outcomes.ts`.
        retry_after_seconds: Option<u64>,
    },
    /// Track B (Session 28, ADR 0007 amendment 4): the LLM declined
    /// to author a recipe and explained why. No `recipe_id` because
    /// no recipe was created. `source_id` carries the source the
    /// decline applied to; `reason` is the LLM's verbatim
    /// explanation (bounded by `Bounds::DECLINE_REASON` at authoring
    /// time, ~2 000 chars).
    Declined {
        source_id: String,
        reason: String,
    },
}

/// Per-failure stage on the wire. Mirrors
/// [`situation_room_pipeline::fetch_executor::FailureStage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
#[serde(rename_all = "snake_case")]
pub enum FailureStageDto {
    Fetch,
    Apply,
    Insert,
}

/// Summary row for the fetch-runs list.
///
/// `started_at` and `finished_at` are full timestamps; the frontend
/// formats them locally. `finished_at` of `None` means the run is
/// still in flight (or — in the corner case where closing the row
/// failed — the final write was lost; the next session's UI may want
/// to surface that state distinctly, but for now an open-looking row
/// is treated as in-flight).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct FetchRunSummaryDto {
    pub id: String,
    pub plan_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub recipes_attempted: u32,
    pub recipes_succeeded: u32,
    pub records_produced: u32,
    pub error_summary: Option<String>,
}

impl FetchReportDto {
    /// Lift a typed `FetchReport` into the wire shape.
    pub fn from_typed(r: situation_room_pipeline::fetch_executor::FetchReport) -> Self {
        Self {
            plan_id: r.plan_id.to_string(),
            run_id: r.run_id.to_string(),
            outcomes: r.outcomes.into_iter().map(RecipeOutcomeDto::from).collect(),
            recipes_attempted: r.recipes_attempted,
            recipes_succeeded: r.recipes_succeeded,
            records_produced: r.records_produced,
            error_summary: r.error_summary,
        }
    }
}

impl From<situation_room_pipeline::fetch_executor::RecipeOutcome> for RecipeOutcomeDto {
    fn from(o: situation_room_pipeline::fetch_executor::RecipeOutcome) -> Self {
        use situation_room_pipeline::fetch_executor::RecipeOutcome as O;
        match o {
            O::Succeeded {
                recipe_id,
                source_id,
                records_produced,
            } => RecipeOutcomeDto::Succeeded {
                recipe_id: recipe_id.to_string(),
                source_id,
                records_produced,
            },
            O::Skipped {
                recipe_id,
                source_id,
                reason,
            } => RecipeOutcomeDto::Skipped {
                recipe_id: recipe_id.to_string(),
                source_id,
                reason,
            },
            O::Failed {
                recipe_id,
                source_id,
                stage,
                message,
            } => RecipeOutcomeDto::Failed {
                recipe_id: recipe_id.to_string(),
                source_id,
                stage: stage.into(),
                message,
            },
            O::RateLimited {
                recipe_id,
                source_id,
                retry_after_seconds,
            } => RecipeOutcomeDto::RateLimited {
                recipe_id: recipe_id.to_string(),
                source_id,
                retry_after_seconds,
            },
            // Track B (Session 28, ADR 0007 amendment 4): the LLM
            // declined to author a recipe. No recipe_id because no
            // recipe exists; only the source the decline applied to
            // and the LLM's verbatim explanation cross the wire.
            O::Declined { source_id, reason } => RecipeOutcomeDto::Declined {
                source_id,
                reason,
            },
        }
    }
}

impl From<situation_room_pipeline::fetch_executor::FailureStage> for FailureStageDto {
    fn from(s: situation_room_pipeline::fetch_executor::FailureStage) -> Self {
        use situation_room_pipeline::fetch_executor::FailureStage as S;
        match s {
            S::Fetch => FailureStageDto::Fetch,
            S::Apply => FailureStageDto::Apply,
            S::Insert => FailureStageDto::Insert,
        }
    }
}

impl FetchRunSummaryDto {
    pub fn from_stored(r: situation_room_storage::StoredFetchRun) -> Self {
        Self {
            id: r.id.to_string(),
            plan_id: r.plan_id.to_string(),
            started_at: r.started_at,
            finished_at: r.finished_at,
            recipes_attempted: r.recipes_attempted,
            recipes_succeeded: r.recipes_succeeded,
            records_produced: r.records_produced,
            error_summary: r.error_summary,
        }
    }
}

// ---------------------------------------------------------------------------
// RecipeDto — wire shape for inspecting authored recipes
// ---------------------------------------------------------------------------

/// Where the recipe-author prompt's document excerpt came from.
/// Wire mirror of [`situation_room_storage::AuthoredFrom`]. ADR 0014.
///
/// ## Why a separate DTO type rather than `#[derive(TS)]` on the
/// storage enum
///
/// Storage doesn't take ts-rs as a dependency (same boundary
/// rationale as for `RecipeDto` vs the typed `FetchRecipe` — see the
/// long comment above). A small mirror here keeps storage free of
/// tooling deps and gives the frontend a stable type at exactly one
/// location. The `From` impl below is the single conversion point.
///
/// ## Wire form discipline
///
/// `serde(rename_all = "snake_case")` matches the storage enum
/// byte-for-byte, so values produced by either side deserialize on
/// the other. The frontend's `AuthoredFromDto` TS type is a literal
/// union of the same three strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
#[serde(rename_all = "snake_case")]
pub enum AuthoredFromDto {
    /// Pre-fetch returned the source's actual response bytes; the
    /// LLM had ground truth at authoring time.
    FetchedBytes,
    /// Pre-fetch failed (or the source has no endpoint_hint); the
    /// LLM saw a synthesized stub and guessed the response shape.
    /// The frontend renders this as the `STUB-AUTHORED` chip and
    /// surfaces a hint banner in the flag dialog.
    StubExcerpt,
    /// Migration v10 default for legacy rows. Renders as no chip;
    /// absence is the signal.
    Unknown,
}

impl From<situation_room_storage::AuthoredFrom> for AuthoredFromDto {
    fn from(a: situation_room_storage::AuthoredFrom) -> Self {
        use situation_room_storage::AuthoredFrom as A;
        match a {
            A::FetchedBytes => AuthoredFromDto::FetchedBytes,
            A::StubExcerpt => AuthoredFromDto::StubExcerpt,
            A::Unknown => AuthoredFromDto::Unknown,
        }
    }
}

/// Wire shape for a recipe as the frontend renders it in the
/// inspection panel.
///
/// ## Why scalar fields are typed but `extraction` / `produces` aren't
///
/// The internal [`situation_room_pipeline::recipes::FetchRecipe`] has
/// strongly-typed `extraction: ExtractionSpec` (closed enum of five
/// modes) and `produces: Vec<ProductionBinding>` (with nested closed
/// enums for `record_type`, `field_value_source`, etc.). Mirroring all
/// of that into ts-rs DTOs is feasible but adds a lot of code that the
/// frontend's recipe-inspection panel doesn't need: it renders these
/// fields as pretty-printed JSON for the user to read.
///
/// So we type the scalar fields strongly (id, source_id, source_url,
/// version, authored_*) and leave the structured fields as
/// `serde_json::Value` on the wire (`unknown` in TypeScript). If a
/// future session wants per-mode rendering on the frontend, the DTO
/// can grow per-variant mirrors then. Pay for type safety when
/// rendering needs it; until then, the round-trip honesty of the JSON
/// is enough.
///
/// ## Where this comes from
///
/// Storage (`StoredRecipe`) carries `extraction_json: String` and
/// `produces_json: String` — JSON strings, not parsed values. The
/// `from_stored` constructor parses both back into `Value` so the
/// frontend doesn't have to do a `JSON.parse` at render time. A parse
/// failure surfaces as a structured error in the `extraction` /
/// `produces` field, which is honest about which recipe is broken
/// rather than 500-ing the whole listing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RecipeDto {
    pub id: String,
    /// `None` if the recipe was authored without a stable dedup key
    /// (older entries; the executor stamps one as of Session 10).
    pub dedup_key: Option<String>,
    pub plan_id: String,
    pub source_id: String,
    pub source_url: String,
    /// The extraction spec — mode + parameters. Opaque on the wire;
    /// TypeScript sees `unknown` and the frontend pretty-prints.
    #[ts(type = "unknown")]
    pub extraction: serde_json::Value,
    /// The production bindings — record_type + field_mappings per
    /// binding. Same opacity rationale as `extraction`.
    #[ts(type = "unknown")]
    pub produces: serde_json::Value,
    pub authored_at: DateTime<Utc>,
    /// Identifier for what authored this recipe — typically a
    /// provider id like `"xai"` or `"recording"` (in tests).
    pub authored_by: String,
    pub version: u32,
    /// Bake-time-frozen payload (ADR 0007 Amendment 3). `None` for
    /// the common HTML-addressable case (recipe fetches `source_url`
    /// at runtime). `Some(payload)` means the recipe is "baked" —
    /// the runtime serves these bytes to extraction in place of an
    /// HTTP fetch, and the recipe will produce the same records
    /// every fetch until re-authored. The frontend renders this
    /// as a visible BAKED badge so the freshness model is explicit
    /// in the UI.
    pub static_payload: Option<String>,
    /// Where the recipe-author prompt's document excerpt came from
    /// (real bytes vs. stub) at authoring time. ADR 0014.
    ///
    /// The frontend renders `StubExcerpt` as a visible
    /// `STUB-AUTHORED` chip on the recipe card and as an
    /// informational banner in the flag dialog. `FetchedBytes`
    /// renders no chip (the absence is the signal: the recipe is
    /// grounded). `Unknown` renders no chip either — it's the
    /// pre-ADR-0014 legacy value, and showing a chip for "we don't
    /// know" would create noise on every existing recipe in the
    /// database the moment the operator updates.
    pub authored_from: AuthoredFromDto,
    /// The recipe id this row supersedes, if any. ADR 0012 §"Storage:
    /// recipe version chain". `None` for first-authored recipes (the
    /// chain head); `Some(prior_id)` for re-authored recipes (Track
    /// A, Session 25/26 — manual re-author UI). The frontend renders
    /// `Some` as a small lineage chip in the recipe head, citing the
    /// prior id.
    ///
    /// Wire form: empty-string-as-absent. The xAI structured-output
    /// schema convention used elsewhere on this DTO doesn't apply
    /// here (this DTO isn't an LLM output), but the same wire
    /// shape is used so the frontend's "Some-vs-None" branch is
    /// consistent across `static_payload`, `dedup_key`, and the
    /// re-author lineage. `Option<String>` of a UUID string is
    /// the chosen idiom — distinct from the typed `Uuid` so the
    /// TypeScript surface stays a plain string.
    pub prior_recipe_id: Option<String>,
    /// Why this recipe was re-authored, if it was. The persisted
    /// short form: failure message + (optional) operator note. `None`
    /// for first-authored recipes; `Some(text)` for re-authored ones.
    /// Travels alongside [`Self::prior_recipe_id`]: a `Some(prior)`
    /// row carries a `Some(reason)`. The frontend's lineage chip
    /// surfaces the reason as a tooltip / details disclosure.
    pub reauthor_reason: Option<String>,
}

impl RecipeDto {
    /// Lift a [`situation_room_storage::StoredRecipe`] into wire shape.
    /// Parses the JSON-string columns back to `Value`s; if either
    /// fails to parse, the field carries a structured error object
    /// instead of crashing the whole listing.
    pub fn from_stored(r: situation_room_storage::StoredRecipe) -> Self {
        let extraction = serde_json::from_str::<serde_json::Value>(&r.extraction_json)
            .unwrap_or_else(|e| {
                serde_json::json!({
                    "_parse_error": e.to_string(),
                    "_raw": r.extraction_json,
                })
            });
        let produces = serde_json::from_str::<serde_json::Value>(&r.produces_json)
            .unwrap_or_else(|e| {
                serde_json::json!({
                    "_parse_error": e.to_string(),
                    "_raw": r.produces_json,
                })
            });
        Self {
            id: r.id.to_string(),
            dedup_key: r.dedup_key,
            plan_id: r.plan_id.to_string(),
            source_id: r.source_id,
            source_url: r.source_url,
            extraction,
            produces,
            authored_at: r.authored_at,
            authored_by: r.authored_by,
            version: r.version,
            static_payload: r.static_payload,
            // ADR 0014: storage already coerced NULL → Unknown; we
            // just lift the typed value into wire form via the
            // `From` impl above.
            authored_from: r.authored_from.into(),
            // Track A, Session 25/26: lineage to the wire. UUID →
            // string for the TS surface (the rest of the DTO uses
            // `String` for ids, not `Uuid`).
            prior_recipe_id: r.prior_recipe_id.map(|id| id.to_string()),
            reauthor_reason: r.reauthor_reason,
        }
    }
}

// ---------------------------------------------------------------------------
// RecipeFeedbackDto — wire shape for ADR 0013 operator feedback
// ---------------------------------------------------------------------------

/// Per-(plan, source) operator note attached via the recipe-inspection
/// panel's flag affordance. Fed back to the LLM as `{{RECIPE_FEEDBACK}}`
/// the next time recipe-authoring runs for the same (plan, source).
///
/// Stored as a single row per (plan, source) — overwrite, not history
/// (ADR 0013 §"The overwrite choice"). The frontend renders the chip
/// next to the recipe card; hover surfaces the note text.
///
/// Wire identifiers are `String`-form UUIDs to keep the TS type
/// consistent with `RecipeDto.plan_id` and `PlanSummary.id`. The
/// `created_at` carries the time the note was last set (so a re-flag
/// surfaces a fresh timestamp); on `clear`, the row is deleted and
/// no DTO is emitted.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RecipeFeedbackDto {
    pub plan_id: String,
    pub source_id: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

impl RecipeFeedbackDto {
    /// Lift a [`situation_room_storage::StoredRecipeFeedback`] into
    /// wire shape. Pure renaming + UUID stringification; no parse
    /// failures possible because the storage layer's columns are
    /// already strongly typed.
    pub fn from_stored(r: situation_room_storage::StoredRecipeFeedback) -> Self {
        Self {
            plan_id: r.plan_id.to_string(),
            source_id: r.source_id,
            note: r.note,
            created_at: r.created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// RecipeFetchAttemptDto — wire shape for ADR 0012 amendment 1 captures
// ---------------------------------------------------------------------------

/// Per-(recipe, run) attempt as it crosses the IPC boundary for the
/// re-author dialog. Track A. The dialog shows the failure message
/// verbatim and the bytes the runtime saw, so the operator reviews
/// the same evidence the LLM will see at re-author time.
///
/// `bytes_excerpt` is the head of the response, capped server-side
/// at `MAX_EXCERPT_BYTES` (64 KiB; see
/// `crates/storage/src/recipe_fetch_attempts.rs`). The wire type is
/// `Option<String>` rather than always-string because:
///
///   - A failure stage *before* bytes were obtained (DNS, TCP,
///     transport-level error before the body) leaves no excerpt.
///   - A row from before this migration would also lack one.
///
/// `failure_message` is similarly `Option<String>` for symmetry; in
/// practice every captured row from Track A has both populated, but
/// the wire shape doesn't pin that and shouldn't (storage can carry
/// looser shapes than the runtime emits).
///
/// `response_content_type` (Session 32) carries the raw response
/// `Content-Type` header value when the underlying transport
/// surfaced one, else `None`. The frontend's response-bytes chip
/// (`RecipesPanel.svelte`) prefers this value when present and
/// falls back to the heuristic byte-sniffer when absent. `None`
/// means one of: row predates migration 0014, server omitted the
/// header, or the bytes came from a `static_payload` (no transport).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RecipeFetchAttemptDto {
    pub id: String,
    pub recipe_id: String,
    pub run_id: String,
    pub attempted_at: DateTime<Utc>,
    pub succeeded: bool,
    pub failure_message: Option<String>,
    pub bytes_excerpt: Option<String>,
    pub response_content_type: Option<String>,
}

impl RecipeFetchAttemptDto {
    /// Lift a [`situation_room_storage::StoredRecipeFetchAttempt`]
    /// into wire shape. Pure renaming + UUID stringification.
    pub fn from_stored(r: situation_room_storage::StoredRecipeFetchAttempt) -> Self {
        Self {
            id: r.id.to_string(),
            recipe_id: r.recipe_id.to_string(),
            run_id: r.run_id.to_string(),
            attempted_at: r.attempted_at,
            succeeded: r.succeeded,
            failure_message: r.failure_message,
            bytes_excerpt: r.bytes_excerpt,
            response_content_type: r.response_content_type,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use situation_room_core::vocab::{EntityId, EventType, Topic, Unit};
    use situation_room_pipeline::research::{
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
            status: situation_room_storage::research_plans::PlanStatus::Pending,
            rejection_reason: None,
            reclassified_from: None,
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
        assert!(!s.has_rejection_reason);
        assert!(!s.is_reclassified);
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
            status: situation_room_storage::research_plans::PlanStatus::Pending,
            rejection_reason: None,
            reclassified_from: None,
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
            status: situation_room_storage::research_plans::PlanStatus::Rejected,
            rejection_reason: None,
            reclassified_from: None,
        };
        let dto = ResearchPlanDto::from_stored(stored).unwrap();
        assert_eq!(dto.status, PlanStatusDto::Rejected);
    }

    #[test]
    fn plan_dto_from_stored_carries_rejection_reason_and_lineage() {
        // Session 15: round-trip the new audit columns. Empty-string
        // wire convention for absence is asserted in
        // plan_dto_from_stored_with_no_audit_fields_uses_empty_strings.
        let p = sample_plan();
        let predecessor = uuid::Uuid::now_v7();
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
            status: situation_room_storage::research_plans::PlanStatus::Rejected,
            rejection_reason: Some("framed under the wrong regulation".into()),
            reclassified_from: Some(predecessor),
        };
        let dto = ResearchPlanDto::from_stored(stored).unwrap();
        assert_eq!(dto.rejection_reason, "framed under the wrong regulation");
        assert_eq!(dto.reclassified_from, predecessor.to_string());
    }

    #[test]
    fn plan_dto_from_stored_with_no_audit_fields_uses_empty_strings() {
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
            status: situation_room_storage::research_plans::PlanStatus::Pending,
            rejection_reason: None,
            reclassified_from: None,
        };
        let dto = ResearchPlanDto::from_stored(stored).unwrap();
        assert_eq!(dto.rejection_reason, "");
        assert_eq!(dto.reclassified_from, "");
    }

    #[test]
    fn plan_summary_with_rejection_reason_sets_has_flag() {
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
            status: situation_room_storage::research_plans::PlanStatus::Rejected,
            rejection_reason: Some("the framing was wrong".into()),
            reclassified_from: None,
        };
        let s = PlanSummary::from_stored(stored).unwrap();
        assert!(s.has_rejection_reason);
        assert!(!s.is_reclassified);
    }

    #[test]
    fn plan_summary_whitespace_only_reason_does_not_set_flag() {
        // A reason that round-trips as whitespace-only shouldn't show
        // a "has note" indicator on the listing — there's no note
        // worth surfacing.
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
            status: situation_room_storage::research_plans::PlanStatus::Rejected,
            rejection_reason: Some("   \t  ".into()),
            reclassified_from: None,
        };
        let s = PlanSummary::from_stored(stored).unwrap();
        assert!(!s.has_rejection_reason);
    }

    #[test]
    fn plan_summary_reclassified_sets_lineage_flag() {
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
            status: situation_room_storage::research_plans::PlanStatus::Pending,
            rejection_reason: None,
            reclassified_from: Some(uuid::Uuid::now_v7()),
        };
        let s = PlanSummary::from_stored(stored).unwrap();
        assert!(s.is_reclassified);
        assert!(!s.has_rejection_reason);
    }

    #[test]
    fn plan_status_dto_round_trips_via_storage_status() {
        use situation_room_storage::research_plans::PlanStatus as S;
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

    /// Track A, ADR 0012 amendment 1: the new ReauthorFailed variant
    /// must round-trip through the shadow DTO with the same kind tag
    /// as the real `CommandError` variant. The generated TS union
    /// gains a new branch on this; the frontend's discriminated-union
    /// match on `error.kind === 'reauthor_failed'` must see the same
    /// JSON the real Tauri serialization produces.
    #[test]
    fn command_error_dto_reauthor_failed_serializes_with_kind_and_prior_id() {
        let e = CommandErrorDto::ReauthorFailed {
            prior_recipe_id: "019dee9a-ba75-7533-aa4f-ee673f03fece".into(),
            message: "no captured fetch attempt exists for this recipe".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"reauthor_failed""#), "got {json}");
        assert!(
            json.contains(r#""prior_recipe_id":"019dee9a-ba75-7533-aa4f-ee673f03fece""#),
            "got {json}"
        );
    }

    // -----------------------------------------------------------------
    // Session 8 — fetch executor DTOs
    // -----------------------------------------------------------------

    #[test]
    fn recipe_outcome_dto_serializes_with_kind_tag_per_variant() {
        let succeeded = RecipeOutcomeDto::Succeeded {
            recipe_id: "id".into(),
            source_id: "demo_csv".into(),
            records_produced: 1,
        };
        let json = serde_json::to_string(&succeeded).unwrap();
        assert!(json.contains(r#""kind":"succeeded""#), "got {json}");

        let skipped = RecipeOutcomeDto::Skipped {
            recipe_id: "id".into(),
            source_id: "demo_csv".into(),
            reason: "json_path: not yet enabled".into(),
        };
        let json = serde_json::to_string(&skipped).unwrap();
        assert!(json.contains(r#""kind":"skipped""#), "got {json}");

        let failed = RecipeOutcomeDto::Failed {
            recipe_id: "id".into(),
            source_id: "demo_csv".into(),
            stage: FailureStageDto::Fetch,
            message: "404".into(),
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains(r#""kind":"failed""#), "got {json}");
        assert!(json.contains(r#""stage":"fetch""#), "got {json}");

        // Track D, Session 25 — rate-limited variant. Tagged with
        // `kind: "rate_limited"` (snake_case via the serde rename).
        // The retry_after_seconds round-trips both as a number and
        // as null; the frontend reads `null` as "no Retry-After
        // provided" (see outcomes.ts).
        let limited_with_value = RecipeOutcomeDto::RateLimited {
            recipe_id: "id".into(),
            source_id: "gdelt".into(),
            retry_after_seconds: Some(120),
        };
        let json = serde_json::to_string(&limited_with_value).unwrap();
        assert!(json.contains(r#""kind":"rate_limited""#), "got {json}");
        assert!(json.contains(r#""retry_after_seconds":120"#), "got {json}");

        let limited_no_value = RecipeOutcomeDto::RateLimited {
            recipe_id: "id".into(),
            source_id: "gdelt".into(),
            retry_after_seconds: None,
        };
        let json = serde_json::to_string(&limited_no_value).unwrap();
        assert!(json.contains(r#""kind":"rate_limited""#), "got {json}");
        assert!(json.contains(r#""retry_after_seconds":null"#), "got {json}");

        // Track B, Session 28 — declined variant. Tagged with
        // `kind: "declined"` (snake_case via the serde rename).
        // Distinguishing wire-shape feature: there is **no
        // `recipe_id` field** because no recipe was created. The
        // frontend's discriminated-union handling must therefore
        // accept that key as absent on the `declined` arm — this
        // test pins it.
        let declined = RecipeOutcomeDto::Declined {
            source_id: "demo_spa".into(),
            reason: "this source is a JS-rendered SPA; the static \
                     HTTP response carries no extractable data"
                .into(),
        };
        let json = serde_json::to_string(&declined).unwrap();
        assert!(json.contains(r#""kind":"declined""#), "got {json}");
        assert!(json.contains(r#""source_id":"demo_spa""#), "got {json}");
        assert!(json.contains("JS-rendered SPA"), "got {json}");
        assert!(
            !json.contains("recipe_id"),
            "declined must not carry a recipe_id field; got {json}"
        );
    }

    /// Track B (Session 28): a `Declined` outcome from the typed
    /// `RecipeOutcome` enum lifts cleanly into the DTO via the
    /// `From` impl. Pins the wire shape against accidental drift.
    #[test]
    fn recipe_outcome_dto_lifts_declined_from_typed() {
        use situation_room_pipeline::fetch_executor::RecipeOutcome;
        let typed = RecipeOutcome::Declined {
            source_id: "demo_spa".into(),
            reason: "no static payload available".into(),
        };
        let dto: RecipeOutcomeDto = typed.into();
        match dto {
            RecipeOutcomeDto::Declined { source_id, reason } => {
                assert_eq!(source_id, "demo_spa");
                assert_eq!(reason, "no static payload available");
            }
            other => panic!("expected Declined, got: {other:?}"),
        }
    }

    #[test]
    fn fetch_report_dto_round_trips_from_typed() {
        use situation_room_pipeline::fetch_executor::{FetchReport, RecipeOutcome};
        let plan_id = uuid::Uuid::now_v7();
        let run_id = uuid::Uuid::now_v7();
        let recipe_id = uuid::Uuid::now_v7();
        let report = FetchReport {
            plan_id,
            run_id,
            outcomes: vec![RecipeOutcome::Succeeded {
                recipe_id,
                source_id: "demo_csv".into(),
                records_produced: 1,
            }],
            recipes_attempted: 1,
            recipes_succeeded: 1,
            records_produced: 1,
            error_summary: None,
        };
        let dto = FetchReportDto::from_typed(report);
        assert_eq!(dto.plan_id, plan_id.to_string());
        assert_eq!(dto.run_id, run_id.to_string());
        assert_eq!(dto.recipes_succeeded, 1);
        assert_eq!(dto.outcomes.len(), 1);
        match &dto.outcomes[0] {
            RecipeOutcomeDto::Succeeded {
                records_produced, ..
            } => assert_eq!(*records_produced, 1),
            other => panic!("expected Succeeded, got {other:?}"),
        }
    }

    #[test]
    fn fetch_run_summary_dto_round_trips_from_stored() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredFetchRun {
            id: uuid::Uuid::now_v7(),
            plan_id: uuid::Uuid::now_v7(),
            started_at: chrono::Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 0).unwrap(),
            finished_at: Some(chrono::Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 5).unwrap()),
            recipes_attempted: 2,
            recipes_succeeded: 1,
            records_produced: 1,
            error_summary: None,
        };
        let dto = FetchRunSummaryDto::from_stored(stored.clone());
        assert_eq!(dto.id, stored.id.to_string());
        assert_eq!(dto.recipes_attempted, 2);
        assert!(dto.finished_at.is_some());
    }

    /// Session 32: the response Content-Type round-trips from
    /// `StoredRecipeFetchAttempt` into the wire DTO. The chip in
    /// `RecipesPanel.svelte` consumes this field directly; the
    /// from_stored path is the only place the wire shape can
    /// silently drop the value.
    #[test]
    fn recipe_fetch_attempt_dto_round_trips_response_content_type() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipeFetchAttempt {
            id: uuid::Uuid::now_v7(),
            recipe_id: uuid::Uuid::now_v7(),
            run_id: uuid::Uuid::now_v7(),
            attempted_at: chrono::Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            succeeded: false,
            failure_message: Some("apply: jsonpath did not match".into()),
            bytes_excerpt: Some("{\"hello\":\"world\"}".into()),
            response_content_type: Some("application/json; charset=utf-8".into()),
        };
        let dto = RecipeFetchAttemptDto::from_stored(stored.clone());
        assert_eq!(dto.id, stored.id.to_string());
        assert_eq!(dto.recipe_id, stored.recipe_id.to_string());
        assert!(!dto.succeeded);
        assert_eq!(
            dto.response_content_type.as_deref(),
            Some("application/json; charset=utf-8")
        );
    }

    /// Mirror of the round-trip test for the absent-header case:
    /// pre-migration rows and rows whose server omitted the header
    /// must surface as `None` on the wire, not as the empty string.
    /// The frontend chip's fallback-to-heuristic branch keys on
    /// `null`.
    #[test]
    fn recipe_fetch_attempt_dto_carries_none_when_header_absent() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipeFetchAttempt {
            id: uuid::Uuid::now_v7(),
            recipe_id: uuid::Uuid::now_v7(),
            run_id: uuid::Uuid::now_v7(),
            attempted_at: chrono::Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            succeeded: false,
            failure_message: Some("apply: regex pattern did not match".into()),
            bytes_excerpt: Some("plain text body".into()),
            response_content_type: None,
        };
        let dto = RecipeFetchAttemptDto::from_stored(stored);
        assert_eq!(dto.response_content_type, None);
    }

    #[test]
    fn recipe_dto_round_trips_from_stored_happy_path() {
        // Both extraction and produces are well-formed JSON, so they
        // land on the wire as parsed `Value`s and serde round-trips
        // them cleanly.
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some("plan-x:demo_csv".into()),
            plan_id: uuid::Uuid::now_v7(),
            source_id: "demo_csv".into(),
            source_url: "https://api.example.com/data.csv".into(),
            extraction_json: r#"{"mode":"csv_cell","column":"production"}"#.into(),
            produces_json: r#"[{"record_type":"observation","field_mappings":[]}]"#.into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: StoredRecipe test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored.clone());
        assert_eq!(dto.id, stored.id.to_string());
        assert_eq!(dto.source_id, "demo_csv");
        assert_eq!(dto.dedup_key.as_deref(), Some("plan-x:demo_csv"));
        // extraction parsed into a JSON object with the expected mode
        assert_eq!(
            dto.extraction.get("mode").and_then(|v| v.as_str()),
            Some("csv_cell")
        );
        // produces parsed into a JSON array with one binding
        assert!(dto.produces.is_array());
        assert_eq!(dto.produces.as_array().unwrap().len(), 1);
        // Default shape: no baked payload.
        assert!(dto.static_payload.is_none());
    }

    #[test]
    fn recipe_dto_surfaces_corrupt_extraction_as_structured_error() {
        // If a stored recipe's extraction column is malformed JSON
        // (which shouldn't normally happen — the executor authors
        // valid JSON — but a hand-edit or future schema change could
        // produce one), the DTO's `extraction` field carries a
        // `_parse_error` marker instead of crashing the listing.
        // This is the discipline the handoff calls "surfacing parse
        // failures rather than zeroing them out."
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: None,
            plan_id: uuid::Uuid::now_v7(),
            source_id: "broken".into(),
            source_url: "https://example.com/".into(),
            extraction_json: "{not valid json".into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: StoredRecipe test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored);
        let err = dto
            .extraction
            .get("_parse_error")
            .and_then(|v| v.as_str())
            .expect("malformed extraction should surface _parse_error");
        assert!(!err.is_empty(), "_parse_error should carry the serde message");
        // produces was valid; it round-tripped cleanly.
        assert!(dto.produces.is_array());
    }

    // -----------------------------------------------------------------
    // Session 18 — static_payload field on RecipeDto (ADR 0007 A3)
    // -----------------------------------------------------------------

    /// Default shape: a `StoredRecipe` with `None` payload threads
    /// through `from_stored` as `None` on the DTO. The frontend's
    /// BAKED-badge predicate is `static_payload != null`, so this
    /// is the case where the badge is hidden.
    #[test]
    fn recipe_dto_static_payload_is_none_when_absent() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some("plan-x:html".into()),
            plan_id: uuid::Uuid::now_v7(),
            source_id: "html_source".into(),
            source_url: "https://example.com/data.html".into(),
            extraction_json: r#"{"mode":"css_select","selector":"h1"}"#.into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: StoredRecipe test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored);
        assert!(dto.static_payload.is_none());
    }

    /// Baked shape: a `StoredRecipe` with `Some(payload)` threads
    /// through `from_stored` as `Some(payload)` on the DTO,
    /// verbatim. The frontend renders this with a BAKED badge plus
    /// a collapsible details block showing the raw payload.
    #[test]
    fn recipe_dto_static_payload_round_trips_from_stored() {
        use chrono::TimeZone;
        let payload = r#"{"date":"2026-03-26","rate":"6.50","direction":"hold"}"#;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some("plan-x:baked".into()),
            plan_id: uuid::Uuid::now_v7(),
            source_id: "mnb_press".into(),
            source_url: "https://www.mnb.hu/press_release_2026Q1.pdf".into(),
            extraction_json: r#"{"mode":"json_path","path":"$.rate"}"#.into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: Some(payload.into()),
            // ADR 0014: StoredRecipe test fixture; provenance not exercised here.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored);
        assert_eq!(dto.static_payload.as_deref(), Some(payload));
    }

    /// ADR 0013 — `StoredRecipeFeedback` lifts cleanly into the wire
    /// DTO. Pure renaming + UUID-to-string; verifying the round-trip
    /// catches the kind of accidental field-rename drift that ts-rs
    /// would otherwise only catch on TS rebuild.
    #[test]
    fn recipe_feedback_dto_round_trips_from_stored() {
        use chrono::TimeZone;
        let plan_id = uuid::Uuid::now_v7();
        let stored = situation_room_storage::StoredRecipeFeedback {
            plan_id,
            source_id: "gdelt".into(),
            note: "fetched the channel <title>, not the article titles".into(),
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 8, 30, 0).unwrap(),
        };
        let dto = RecipeFeedbackDto::from_stored(stored);
        assert_eq!(dto.plan_id, plan_id.to_string());
        assert_eq!(dto.source_id, "gdelt");
        assert_eq!(
            dto.note,
            "fetched the channel <title>, not the article titles"
        );
        assert_eq!(dto.created_at.to_rfc3339(), "2026-05-02T08:30:00+00:00");
    }

    // -----------------------------------------------------------------
    // Session 21 — authored_from on RecipeDto (ADR 0014)
    // -----------------------------------------------------------------

    /// `AuthoredFromDto` and `situation_room_storage::AuthoredFrom`
    /// must serialize byte-for-byte identically. The `From` impl
    /// above is the only conversion path; if the wire forms ever
    /// drift, the chip in the UI silently misreads `stub_excerpt`
    /// rows as `Unknown` (no chip) — exactly the failure ADR 0014
    /// is closing.
    #[test]
    fn authored_from_dto_wire_form_matches_storage_enum() {
        for (storage, expected_dto) in [
            (
                situation_room_storage::AuthoredFrom::FetchedBytes,
                AuthoredFromDto::FetchedBytes,
            ),
            (
                situation_room_storage::AuthoredFrom::StubExcerpt,
                AuthoredFromDto::StubExcerpt,
            ),
            (
                situation_room_storage::AuthoredFrom::Unknown,
                AuthoredFromDto::Unknown,
            ),
        ] {
            let dto: AuthoredFromDto = storage.into();
            assert_eq!(dto, expected_dto);

            let storage_json = serde_json::to_string(&storage).unwrap();
            let dto_json = serde_json::to_string(&dto).unwrap();
            assert_eq!(
                storage_json, dto_json,
                "AuthoredFromDto and storage AuthoredFrom must serialize identically; got dto={dto_json}, storage={storage_json}"
            );
        }
    }

    /// A `StoredRecipe` whose authoring fell back to the stub path
    /// surfaces as `AuthoredFromDto::StubExcerpt` on the wire. This
    /// is the GDELT 429 case from the Session 20 live run — the
    /// single concrete instance that motivated ADR 0014. Pinning it
    /// guards against a future refactor that drops the field from
    /// `from_stored` and silently coerces every recipe to
    /// FetchedBytes.
    #[test]
    fn recipe_dto_surfaces_stub_excerpt_authored_from() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some("plan-x:gdelt".into()),
            plan_id: uuid::Uuid::now_v7(),
            source_id: "gdelt".into(),
            source_url: "https://api.gdeltproject.org/api/v2/doc/doc?query=...".into(),
            extraction_json: r#"{"mode":"json_path","path":"$.articles[0].title"}"#.into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 9, 27, 17).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::StubExcerpt,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored);
        assert_eq!(dto.authored_from, AuthoredFromDto::StubExcerpt);
    }

    /// Legacy rows (pre-v10) load as `Unknown` from storage and
    /// surface as `Unknown` on the wire — no chip, no banner, just
    /// the absence of a positive signal. This is the load-bearing
    /// guarantee for "no UI noise on every existing recipe the
    /// moment migration v10 runs."
    #[test]
    fn recipe_dto_surfaces_unknown_authored_from_for_legacy_rows() {
        use chrono::TimeZone;
        let stored = situation_room_storage::StoredRecipe {
            id: uuid::Uuid::now_v7(),
            dedup_key: Some("plan-x:legacy".into()),
            plan_id: uuid::Uuid::now_v7(),
            source_id: "legacy_source".into(),
            source_url: "https://example.com/data.csv".into(),
            extraction_json: r#"{"mode":"csv_cell","column":"value"}"#.into(),
            produces_json: "[]".into(),
            authored_at: chrono::Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // Migration v10 NULL → AuthoredFrom::Unknown coercion
            // happens in storage; here we simulate that having
            // already happened.
            authored_from: situation_room_storage::AuthoredFrom::Unknown,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let dto = RecipeDto::from_stored(stored);
        assert_eq!(dto.authored_from, AuthoredFromDto::Unknown);
    }
}
