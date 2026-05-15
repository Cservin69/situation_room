//! Records-on-the-workstation DTOs (Session 22).
//!
//! Wire shapes for the records that a plan's recipes have produced.
//! Mirrors [`situation_room_storage::RecordsByPlan`] one-for-one,
//! plus per-record-type DTOs sitting on top of a shared
//! [`EnvelopeDto`]. The frontend renders these inside the
//! six-bucket panel grid so the operator can see the actual data
//! the recipes extracted, not just the expectation list.
//!
//! ## Why typed envelope, opaque content
//!
//! Same trade-off as [`super::types_export::RecipeDto`]: the scalar
//! fields the frontend uses for *layout* (id, dedup_key, the
//! envelope's provenance and topics, the per-record-type "kind"
//! discriminator) want strong types so the TS code can rely on
//! field names. The structured `content` field — `metric` /
//! `value` / `unit` for an Observation, `event_type` / `headline`
//! for an Event, etc. — is opaque on the wire (`unknown` in TS)
//! because the per-type rendering needs are still being shaped.
//!
//! The frontend's `formatRecordSummary(record)` helper knows the
//! per-type content shape informally and pulls top-line summary
//! fields by JSON path lookup. When per-type rendering needs
//! stabilize (and the prompt-author-vs-content inversion the
//! Session 21 live run surfaced is better understood), a future
//! session can split each content type into a strong DTO and
//! migrate the frontend off the JSON-path helper. The decision
//! today is to keep this surface narrow and ship the records
//! visible.
//!
//! ## Provenance threading
//!
//! Every record's `envelope.provenance.source_id` carries the
//! recipe-stamped string `"{src}#recipe:{recipe_uuid}@v{version}"`
//! per `pipeline::recipe_apply::build_record`. The DTO surfaces a
//! pre-parsed `recipe_id: Option<String>` next to the raw
//! `source_id` so the frontend can join visually to the matching
//! recipe card without re-implementing the regex per usage. If the
//! provenance string doesn't match the expected shape (a legacy
//! row, a record produced by some path other than recipe_apply),
//! `recipe_id` is `None` rather than a synthesized fallback —
//! honest about the shape rather than guessing.
//!
//! ## DTO surface bounds
//!
//! `RecordsByPlanDto` is per-type Vecs (mirroring storage's
//! `RecordsByPlan`) rather than a single tagged-union vec. The
//! frontend's bucket-panel structure already partitions by record
//! type; collapsing into one vec just to re-partition on the
//! frontend would be a wasted round-trip.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use situation_room_core::{
    Assertion, Document, Entity, Envelope, Event, Observation, Provenance, Relation, Subjects,
};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// EnvelopeDto and friends
// ---------------------------------------------------------------------------

/// Wire shape for [`situation_room_core::Envelope`].
///
/// Carries provenance, subjects, audit timestamps, confidence. The
/// `subjects.places` and `subjects.time` fields are kept opaque
/// (`unknown` in TS) because place geometry and time-scope
/// shapes haven't stabilized for the rendering side yet — a Session
/// 22+ rendering pass that needs them can promote those to typed
/// DTOs without a wire break.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct EnvelopeDto {
    pub provenance: ProvenanceDto,
    pub subjects: SubjectsDto,
    pub tags: Vec<String>,
    /// Validity timestamp, in ISO 8601. `None` for records that
    /// don't carry one (Documents, Entities, the open valid window
    /// of an Observation).
    pub valid_at: Option<DateTime<Utc>>,
    /// When the system observed the record (the fetch timestamp,
    /// roughly). Always populated.
    pub observed_at: DateTime<Utc>,
    /// Confidence in [0.0, 1.0]. The vocab newtype clamps; the
    /// wire shape is a plain f32.
    pub confidence: f32,
}

impl EnvelopeDto {
    fn from_typed(env: Envelope) -> Self {
        Self {
            provenance: ProvenanceDto::from_typed(env.provenance),
            subjects: SubjectsDto::from_typed(env.subjects),
            tags: env.tags,
            valid_at: env.valid_at,
            observed_at: env.observed_at,
            confidence: env.confidence.value(),
        }
    }
}

/// Wire shape for [`situation_room_core::Provenance`].
///
/// `source_id` is the full provenance string —
/// `"{src}#recipe:{recipe_uuid}@v{version}"` for records produced
/// by a recipe. `recipe_id` is the parsed recipe-uuid sub-component
/// surfaced as a separate field so the frontend can join records
/// to recipe cards without parsing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct ProvenanceDto {
    /// The full provenance string as stamped by recipe_apply.
    pub source_id: String,
    /// The parsed recipe id, when the provenance string follows the
    /// recipe-stamped format. `None` for legacy records or records
    /// whose source_id has some other shape.
    ///
    /// Empty-string-as-absent wire convention: `""` means "no recipe
    /// id parseable from this provenance", which the frontend treats
    /// as "no link to any recipe card." Mirrors the
    /// `unit_hint` / `display` handling elsewhere in this crate.
    pub recipe_id: String,
    pub source_url: Option<String>,
    pub source_published_at: Option<DateTime<Utc>>,
    pub license: String,
    /// Derivation chain as opaque JSON. The full
    /// `Vec<DerivedFrom>` shape isn't rendered today (no records
    /// derive from others until ADR 0004 assertion promotion is
    /// implemented); typing it would be premature.
    #[ts(type = "unknown")]
    pub derived_from: serde_json::Value,
}

impl ProvenanceDto {
    fn from_typed(p: Provenance) -> Self {
        let recipe_id = parse_recipe_id(&p.source_id).unwrap_or_default();
        let derived_from = serde_json::to_value(&p.derived_from)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        Self {
            source_id: p.source_id,
            recipe_id,
            source_url: p.source_url,
            source_published_at: p.source_published_at,
            license: p.license,
            derived_from,
        }
    }
}

/// Parse the recipe uuid out of a provenance string.
///
/// The format produced by `pipeline::recipe_apply::build_record` is:
/// `"{src}#recipe:{recipe_uuid}@v{version}"`. This function returns
/// the uuid as a string, or `None` if the format doesn't match.
///
/// A naive `split` is fine here — the format is fixed and the
/// uuid is always a 36-char canonical UUID string. We don't pull in
/// `regex` for one path-known parse; the cost of getting the parse
/// wrong would be an empty `recipe_id` field (i.e. the chip just
/// doesn't link), not a security issue.
fn parse_recipe_id(source_id: &str) -> Option<String> {
    let after_marker = source_id.split("#recipe:").nth(1)?;
    let uuid_part = after_marker.split("@v").next()?;
    if uuid_part.len() == 36 && uuid_part.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Some(uuid_part.to_string())
    } else {
        None
    }
}

/// Wire shape for [`situation_room_core::Subjects`].
///
/// `entities` and `topics` are flat string vecs (vocab newtypes
/// serialize as plain strings). `places` and `time` are kept
/// opaque — the frontend doesn't render places or time-scopes on
/// the record card today, and typing the full enum hierarchy
/// isn't justified until it does.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct SubjectsDto {
    pub entities: Vec<String>,
    /// Place refs as opaque JSON. Each entry is one of the
    /// `PlaceRef` variants; rendering picks them apart on demand.
    #[ts(type = "unknown")]
    pub places: serde_json::Value,
    /// Time scope as opaque JSON, or `None`.
    #[ts(type = "unknown | null")]
    pub time: Option<serde_json::Value>,
    pub topics: Vec<String>,
}

impl SubjectsDto {
    fn from_typed(s: Subjects) -> Self {
        let entities = s.entities.into_iter().map(|e| e.as_str().to_string()).collect();
        let topics = s.topics.into_iter().map(|t| t.as_str().to_string()).collect();
        let places = serde_json::to_value(&s.places)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        let time = s
            .time
            .as_ref()
            .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null));
        Self {
            entities,
            places,
            time,
            topics,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-record-type DTOs
// ---------------------------------------------------------------------------

/// Wire shape for an Observation record.
///
/// `content` carries the `ObservationContent` shape (metric, value,
/// unit, period, …) as opaque JSON. The frontend's
/// `formatRecordSummary` helper knows the shape informally and
/// pulls top-line fields (`metric`, `value`, `unit`) by JSON
/// path. See module docs for the rationale.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct ObservationDto {
    pub id: String,
    pub dedup_key: Option<String>,
    pub envelope: EnvelopeDto,
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
}

impl ObservationDto {
    fn from_typed(o: Observation) -> Self {
        let content = serde_json::to_value(&o.content).unwrap_or(serde_json::Value::Null);
        Self {
            id: o.id.to_string(),
            dedup_key: o.dedup_key,
            envelope: EnvelopeDto::from_typed(o.envelope),
            content,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct EventDto {
    pub id: String,
    pub dedup_key: Option<String>,
    pub envelope: EnvelopeDto,
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
}

impl EventDto {
    fn from_typed(e: Event) -> Self {
        let content = serde_json::to_value(&e.content).unwrap_or(serde_json::Value::Null);
        Self {
            id: e.id.to_string(),
            dedup_key: e.dedup_key,
            envelope: EnvelopeDto::from_typed(e.envelope),
            content,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct EntityDto {
    pub id: String,
    /// Stable business identifier (the [`EntityId`] vocab type).
    pub entity_id: String,
    pub kind: String,
    pub canonical_name: String,
    /// Geometry as opaque JSON (lat/long, polygons, etc.).
    /// `None` when the entity has no spatial location.
    #[ts(type = "unknown | null")]
    pub geometry: Option<serde_json::Value>,
    pub envelope: EnvelopeDto,
}

impl EntityDto {
    fn from_typed(e: Entity) -> Self {
        let geometry = e
            .geometry
            .as_ref()
            .map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null));
        Self {
            id: e.id.to_string(),
            entity_id: e.entity_id.as_str().to_string(),
            kind: e.kind,
            canonical_name: e.canonical_name,
            geometry,
            envelope: EnvelopeDto::from_typed(e.envelope),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RelationDto {
    pub id: String,
    pub dedup_key: Option<String>,
    pub envelope: EnvelopeDto,
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
}

impl RelationDto {
    fn from_typed(r: Relation) -> Self {
        let content = serde_json::to_value(&r.content).unwrap_or(serde_json::Value::Null);
        Self {
            id: r.id.to_string(),
            dedup_key: r.dedup_key,
            envelope: EnvelopeDto::from_typed(r.envelope),
            content,
        }
    }
}

/// Wire shape for a Document record.
///
/// Fields are flat (no nested `content` blob): a Document's payload
/// IS its envelope plus title/kind/mime/body/published_at/author.
/// No JSON-path lookup needed for the top-line summary.
///
/// `body` may be large (extracted PDF text, full HTML article).
/// The frontend only renders the title in the card view; the body
/// is available for the expanded view if/when one is built.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct DocumentDto {
    pub id: String,
    pub dedup_key: Option<String>,
    pub title: Option<String>,
    pub kind: String,
    pub mime: String,
    pub body: String,
    pub published_at: Option<DateTime<Utc>>,
    pub author: Option<String>,
    pub envelope: EnvelopeDto,
}

impl DocumentDto {
    fn from_typed(d: Document) -> Self {
        Self {
            id: d.id.to_string(),
            dedup_key: d.dedup_key,
            title: d.title,
            kind: d.kind,
            mime: d.mime,
            body: d.body,
            published_at: d.published_at,
            author: d.author,
            envelope: EnvelopeDto::from_typed(d.envelope),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct AssertionDto {
    pub id: String,
    pub dedup_key: Option<String>,
    /// The entity making the claim (lifted from the `EntityId`
    /// vocab newtype to a plain string).
    pub claimant: String,
    /// Stance toward the content — `"asserted"`, `"denied"`,
    /// `"hedged"`, etc. (matches the [`Stance`] enum's
    /// snake_case wire form).
    pub stance: String,
    /// The asserted content as opaque JSON. The
    /// [`AssertedContent`] enum has an `asserted_kind`
    /// discriminator (renamed in Session 78 from `kind` to avoid a
    /// collision with `RelationContent.kind`) with per-variant
    /// payload; the frontend reads `content.asserted_kind` and
    /// dispatches.
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
    pub envelope: EnvelopeDto,
}

impl AssertionDto {
    fn from_typed(a: Assertion) -> Self {
        let content = serde_json::to_value(&a.content).unwrap_or(serde_json::Value::Null);
        let stance = serde_json::to_value(a.stance)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        Self {
            id: a.id.to_string(),
            dedup_key: a.dedup_key,
            claimant: a.claimant.as_str().to_string(),
            stance,
            content,
            envelope: EnvelopeDto::from_typed(a.envelope),
        }
    }
}

// ---------------------------------------------------------------------------
// RecordsByPlanDto — the bucketed wire shape
// ---------------------------------------------------------------------------

/// Wire shape for [`situation_room_storage::RecordsByPlan`].
///
/// Per-type Vecs, mirroring the storage shape. The frontend's
/// six-bucket panel grid maps one bucket to one Vec; rendering is
/// a straight iteration with no client-side partitioning.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../apps/desktop/src/lib/api/types/")]
pub struct RecordsByPlanDto {
    pub observations: Vec<ObservationDto>,
    pub events: Vec<EventDto>,
    pub entities: Vec<EntityDto>,
    pub relations: Vec<RelationDto>,
    pub documents: Vec<DocumentDto>,
    pub assertions: Vec<AssertionDto>,
}

impl RecordsByPlanDto {
    pub fn from_typed(r: situation_room_storage::RecordsByPlan) -> Self {
        Self {
            observations: r.observations.into_iter().map(ObservationDto::from_typed).collect(),
            events: r.events.into_iter().map(EventDto::from_typed).collect(),
            entities: r.entities.into_iter().map(EntityDto::from_typed).collect(),
            relations: r.relations.into_iter().map(RelationDto::from_typed).collect(),
            documents: r.documents.into_iter().map(DocumentDto::from_typed).collect(),
            assertions: r.assertions.into_iter().map(AssertionDto::from_typed).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, Topic, Unit};
    use situation_room_storage::RecordsByPlan;
    use uuid::Uuid;

    fn obs() -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!(
                    "gdelt#recipe:{}@v1",
                    "01950000-0000-7000-8000-000000000000"
                ),
                source_url: Some("https://gdelt.example/q".into()),
                source_published_at: None,
                license: "unknown".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("south_korea_election").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
            confidence: Confidence::ONE,
        };
        Observation::new(
            envelope,
            ObservationContent {
                metric: "voter_turnout".into(),
                value: 77.1,
                unit: Unit::new("%").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Instant,
                geometry: None,
            },
        )
    }

    #[test]
    fn parse_recipe_id_extracts_uuid_from_recipe_stamped_provenance() {
        let pid = "01950000-0000-7000-8000-000000000000";
        let provenance = format!("gdelt#recipe:{pid}@v1");
        assert_eq!(parse_recipe_id(&provenance).as_deref(), Some(pid));
    }

    #[test]
    fn parse_recipe_id_returns_none_for_legacy_provenance() {
        // No recipe substring (legacy records pre-recipe_apply).
        assert_eq!(parse_recipe_id("usgs_mcs"), None);
        // Recipe substring but not a valid UUID.
        assert_eq!(parse_recipe_id("x#recipe:not-a-uuid@v1"), None);
        // Truncated.
        assert_eq!(parse_recipe_id("x#recipe:01950000-0000-7000@v1"), None);
    }

    #[test]
    fn observation_dto_round_trips_via_from_typed() {
        let o = obs();
        let id = o.id.to_string();
        let provenance = o.envelope.provenance.source_id.clone();
        let dto = ObservationDto::from_typed(o);
        assert_eq!(dto.id, id);
        assert_eq!(dto.envelope.provenance.source_id, provenance);
        // Recipe id parsed out of provenance.
        assert_eq!(
            dto.envelope.provenance.recipe_id,
            "01950000-0000-7000-8000-000000000000"
        );
        // Topics serialized as a plain string array.
        assert_eq!(dto.envelope.subjects.topics, vec!["south_korea_election"]);
        // Content opaque but populated.
        assert!(dto.content.is_object());
        assert_eq!(
            dto.content.get("metric").and_then(|v| v.as_str()),
            Some("voter_turnout")
        );
    }

    #[test]
    fn records_by_plan_dto_buckets_by_type() {
        let mut bucket = RecordsByPlan::default();
        bucket.observations.push(obs());
        bucket.observations.push(obs());

        let dto = RecordsByPlanDto::from_typed(bucket);
        assert_eq!(dto.observations.len(), 2);
        assert!(dto.events.is_empty());
        assert!(dto.entities.is_empty());
        assert!(dto.relations.is_empty());
        assert!(dto.documents.is_empty());
        assert!(dto.assertions.is_empty());
    }

    #[test]
    fn records_by_plan_dto_serializes_with_per_type_vecs() {
        // Wire-shape guard: the frontend expects per-type fields,
        // not a single tagged-union vec.
        let dto = RecordsByPlanDto::from_typed(RecordsByPlan::default());
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"observations\":["));
        assert!(json.contains("\"events\":["));
        assert!(json.contains("\"entities\":["));
        assert!(json.contains("\"relations\":["));
        assert!(json.contains("\"documents\":["));
        assert!(json.contains("\"assertions\":["));
    }

    #[test]
    fn empty_recipe_id_round_trips_for_legacy_provenance() {
        // A record stamped with a non-recipe source_id should
        // surface `recipe_id` as the empty string, matching the
        // wire convention for "absent".
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "legacy_source".into(),
                source_url: None,
                source_published_at: None,
                license: "unknown".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let o = Observation::new(
            envelope,
            ObservationContent {
                metric: "x".into(),
                value: 1.0,
                unit: Unit::new("1").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Instant,
                geometry: None,
            },
        );
        let dto = ObservationDto::from_typed(o);
        assert_eq!(dto.envelope.provenance.recipe_id, "");
        assert_eq!(dto.envelope.provenance.source_id, "legacy_source");
    }

    #[test]
    fn entity_dto_lifts_entity_id_to_string() {
        // EntityId is a vocab newtype; the wire form is the inner
        // string. Guard the lift so a future refactor of the
        // newtype doesn't change the wire shape.
        use situation_room_core::EntityId;
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "test".into(),
                source_url: None,
                source_published_at: None,
                license: "unknown".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let entity = Entity {
            id: Uuid::now_v7(),
            entity_id: EntityId::new("mine:greenbushes").unwrap(),
            kind: "mine".into(),
            canonical_name: "Greenbushes".into(),
            geometry: None,
            envelope,
        };
        let dto = EntityDto::from_typed(entity);
        assert_eq!(dto.entity_id, "mine:greenbushes");
        assert_eq!(dto.kind, "mine");
    }
}
