//! The six record types and the [`Record`] enum that unifies them.
//!
//! See `docs/adr/0003-six-record-types.md` for the rationale.

pub mod observation;
pub mod event;
pub mod entity;
pub mod relation;
pub mod document;
pub mod assertion;

pub use assertion::Assertion;
pub use document::Document;
pub use entity::Entity;
pub use event::Event;
pub use observation::Observation;
pub use relation::Relation;

use crate::schema::envelope::Envelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The unified record type. Every piece of structured data in situation_room
/// is one of these six.
///
/// Serialization uses the `type` field as a discriminator so the JSON
/// shape matches what the frontend sees via ts-rs generation: every
/// record is `{ "type": "observation", "id": ..., ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Observation(Observation),
    Event(Event),
    Entity(Entity),
    Relation(Relation),
    Document(Document),
    Assertion(Assertion),
}

/// A standalone tag for the six record types.
///
/// Use this when you need to *refer to* a record type without having a
/// value of that type — e.g. in pipeline configuration, in recipe
/// production bindings (see `situation_room_pipeline::recipes`), or when
/// logging type-level decisions. For an actual record, use [`Record`].
///
/// The variant set is identical to [`Record`] and the serde
/// representation matches [`Record::kind`] — an independent string
/// tag serialized as `"observation"`, `"event"`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    Observation,
    Event,
    Entity,
    Relation,
    Document,
    Assertion,
}

impl RecordType {
    /// String form matching [`Record::kind`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Event => "event",
            Self::Entity => "entity",
            Self::Relation => "relation",
            Self::Document => "document",
            Self::Assertion => "assertion",
        }
    }
}

impl std::fmt::Display for RecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Record {
    /// Get the record's UUID without matching on the variant.
    pub fn id(&self) -> Uuid {
        match self {
            Self::Observation(r) => r.id,
            Self::Event(r) => r.id,
            Self::Entity(r) => r.id,
            Self::Relation(r) => r.id,
            Self::Document(r) => r.id,
            Self::Assertion(r) => r.id,
        }
    }

    /// The structural tag for this record's variant.
    ///
    /// Complements [`Record::kind`], which returns a `&'static str` for
    /// logging. This returns the enum form for typed comparisons.
    pub fn record_type(&self) -> RecordType {
        match self {
            Self::Observation(_) => RecordType::Observation,
            Self::Event(_) => RecordType::Event,
            Self::Entity(_) => RecordType::Entity,
            Self::Relation(_) => RecordType::Relation,
            Self::Document(_) => RecordType::Document,
            Self::Assertion(_) => RecordType::Assertion,
        }
    }

    /// Borrow the record's envelope without matching.
    pub fn envelope(&self) -> &Envelope {
        match self {
            Self::Observation(r) => &r.envelope,
            Self::Event(r) => &r.envelope,
            Self::Entity(r) => &r.envelope,
            Self::Relation(r) => &r.envelope,
            Self::Document(r) => &r.envelope,
            Self::Assertion(r) => &r.envelope,
        }
    }

    /// Identifier for the variant, for logging and routing.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Observation(_) => "observation",
            Self::Event(_) => "event",
            Self::Entity(_) => "entity",
            Self::Relation(_) => "relation",
            Self::Document(_) => "document",
            Self::Assertion(_) => "assertion",
        }
    }

    /// Dedup key for upsert logic. None for records that don't carry one.
    pub fn dedup_key(&self) -> Option<&str> {
        match self {
            Self::Observation(r) => r.dedup_key.as_deref(),
            Self::Event(r) => r.dedup_key.as_deref(),
            Self::Relation(r) => r.dedup_key.as_deref(),
            Self::Document(r) => r.dedup_key.as_deref(),
            Self::Assertion(r) => r.dedup_key.as_deref(),
            Self::Entity(_) => None, // entities dedup on entity_id, handled in storage layer
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers — lifting a record into the enum
// ---------------------------------------------------------------------------

impl From<Observation> for Record {
    fn from(r: Observation) -> Self { Self::Observation(r) }
}
impl From<Event> for Record {
    fn from(r: Event) -> Self { Self::Event(r) }
}
impl From<Entity> for Record {
    fn from(r: Entity) -> Self { Self::Entity(r) }
}
impl From<Relation> for Record {
    fn from(r: Relation) -> Self { Self::Relation(r) }
}
impl From<Document> for Record {
    fn from(r: Document) -> Self { Self::Document(r) }
}
impl From<Assertion> for Record {
    fn from(r: Assertion) -> Self { Self::Assertion(r) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::content::{AssertedContent, ObservationContent, ObservationPeriod};
    use crate::schema::envelope::{Provenance, Subjects};
    use crate::vocab::{Confidence, EntityId, Stance, Topic, Unit};
    use chrono::{TimeZone, Utc};

    fn test_envelope() -> Envelope {
        Envelope {
            provenance: Provenance {
                source_id: "usgs_mcs".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: Vec::new(),
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects::topic(Topic::new("Li").unwrap()),
            tags: vec![],
            valid_at: Some(Utc.with_ymd_and_hms(2025, 12, 31, 0, 0, 0).unwrap()),
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        }
    }

    fn test_observation_content() -> ObservationContent {
        ObservationContent {
            metric: "production".into(),
            value: 142_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: None,
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        }
    }

    #[test]
    fn observation_has_uuidv7_id() {
        let obs = Observation::new(test_envelope(), test_observation_content());
        // UUIDv7's version bits are 7
        assert_eq!(obs.id.get_version_num(), 7);
    }

    #[test]
    fn record_enum_roundtrips() {
        let obs = Observation::new(test_envelope(), test_observation_content());
        let rec: Record = obs.into();
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"type\":\"observation\""));
        let back: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn record_accessors_work_for_each_variant() {
        let obs = Observation::new(test_envelope(), test_observation_content());
        let rec: Record = obs.clone().into();
        assert_eq!(rec.id(), obs.id);
        assert_eq!(rec.kind(), "observation");
        assert_eq!(rec.envelope(), &obs.envelope);
    }

    #[test]
    fn assertion_wraps_observation_content() {
        let assertion = Assertion::new(
            EntityId::new("usgs").unwrap(),
            Stance::Asserted,
            AssertedContent::Observation(test_observation_content()),
            test_envelope(),
        );
        let rec: Record = assertion.clone().into();
        assert_eq!(rec.kind(), "assertion");
        // Roundtrip
        let json = serde_json::to_string(&rec).unwrap();
        let back: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn promotion_preserves_content() {
        // Simulate the promotion pipeline: take an Assertion's
        // ObservationContent and wrap it in a new Observation with a
        // distinct envelope (provenance updated to "derived").
        let assertion = Assertion::new(
            EntityId::new("usgs").unwrap(),
            Stance::Asserted,
            AssertedContent::Observation(test_observation_content()),
            test_envelope(),
        );
        let content = match assertion.content {
            AssertedContent::Observation(ref c) => c.clone(),
            _ => panic!("expected Observation content"),
        };
        let mut promoted_env = test_envelope();
        promoted_env.provenance.source_id = "derived".into();
        promoted_env.provenance.derived_from.push(
            crate::schema::envelope::DerivedFrom {
                record_id: assertion.id,
                role: crate::schema::envelope::DerivationRole::Promotion,
            },
        );
        let observation = Observation::new(promoted_env, content.clone());
        // Content survives intact
        assert_eq!(observation.content, content);
        // The new observation has a fresh UUID, not the assertion's
        assert_ne!(observation.id, assertion.id);
        // And its provenance records where it came from
        assert_eq!(observation.envelope.provenance.source_id, "derived");
        assert_eq!(
            observation.envelope.provenance.derived_from[0].record_id,
            assertion.id
        );
    }

    #[test]
    fn dedup_key_accessible_uniformly() {
        let obs = Observation::new(test_envelope(), test_observation_content())
            .with_dedup_key("usgs:Li:production:2025");
        let rec: Record = obs.into();
        assert_eq!(rec.dedup_key(), Some("usgs:Li:production:2025"));
    }
}
