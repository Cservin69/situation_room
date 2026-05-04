//! Bare content types.
//!
//! These are the *claim shapes* — what an Observation actually observes,
//! what an Event actually describes, etc. They are used in two places:
//!
//! 1. Inside the full record types ([`Observation`], [`Event`], etc. in
//!    [`records`](super::records)), wrapped with an [`Envelope`](super::envelope::Envelope) and a UUID.
//! 2. Inside [`Assertion`](super::records::assertion::Assertion)s as the *content being claimed*, without an ID
//!    or envelope of their own (the Assertion's envelope applies).
//!
//! This single-source-of-truth pattern means a price observation's shape
//! is defined once, whether that price is directly measured (an
//! Observation) or extracted from an article as a reported figure (an
//! Assertion with `AssertedContent::Observation`).
//!
//! When an Assertion is promoted to an Observation, the promotion code
//! copies the `ObservationContent` out and wraps it in a new full
//! `Observation` with a fresh envelope and UUID. The content survives
//! unchanged; the metadata layer changes.

use crate::schema::geometry::Geometry;
use crate::vocab::{CountryCode, Currency, EntityId, EventType, Topic, Unit};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ObservationContent
// ---------------------------------------------------------------------------

/// The content of an observation: a measurement of a metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ObservationContent {
    /// What is being measured. Examples: "price", "production",
    /// "warehouse_stock", "capex", "fab_utilization". Lowercase snake_case.
    pub metric: String,

    /// The measured value.
    pub value: f64,

    /// UCUM-style unit (e.g. "t", "kt/yr", "USD/t", "%", "1").
    pub unit: Unit,

    /// Optional symmetric uncertainty bound (absolute, same unit as value).
    /// `value ± value_uncertainty`. `None` means the source reported a
    /// point estimate with no uncertainty.
    pub value_uncertainty: Option<f64>,

    /// If the value is a price or monetary amount, the currency.
    /// Independent of `unit` so a source can report `unit = "USD/t"`
    /// *and* leave `currency = Some("USD")` redundantly, which is fine.
    pub currency: Option<Currency>,

    /// For stock/flow measurements, the period the measurement covers.
    /// Examples: `"daily"`, `"monthly"`, `"quarterly"`, `"annual"`,
    /// or `"instant"` for a point-in-time measurement like a spot price.
    pub period: ObservationPeriod,

    /// Optional geometry if the measurement is spatially located
    /// (e.g. a satellite radiance reading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationPeriod {
    /// Point-in-time measurement (prices, inventory levels).
    Instant,
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Annual,
    /// Custom period — valid_at on the envelope should give the end,
    /// the field here is the period duration in ISO-8601 format.
    Custom(String),
}

// ---------------------------------------------------------------------------
// EventContent
// ---------------------------------------------------------------------------

/// The content of a discrete dated event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct EventContent {
    /// Controlled event type from `config/vocab/event_types.toml`.
    pub event_type: EventType,

    /// Human-readable one-line description. The UI uses this in the
    /// events feed. Should be a complete English sentence.
    pub headline: String,

    /// Actors involved in the event. For an export restriction: the
    /// country imposing it. For an M&A: acquirer and target.
    #[serde(default)]
    pub actors: Vec<EntityId>,

    /// Expected market direction: `supply_positive` (new supply coming
    /// online), `supply_negative` (supply lost), `demand_positive`,
    /// `demand_negative`, `context` (neither direction, background info).
    /// Informs anomaly detection rollups.
    pub direction: Option<EventDirection>,

    /// If the event has a magnitude (e.g. "50kt production lost to the
    /// strike"), this embeds an ObservationContent describing it.
    /// Keeps magnitudes in the ObservationContent schema so downstream
    /// code doesn't special-case event quantities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnitude: Option<ObservationContent>,

    /// Geographic location of the event, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventDirection {
    SupplyPositive,
    SupplyNegative,
    DemandPositive,
    DemandNegative,
    Context,
}

// ---------------------------------------------------------------------------
// RelationContent
// ---------------------------------------------------------------------------

/// The content of a directed edge between two entities.
/// Examples: "A owns 49% of B", "country X exports 140kt to country Y",
/// "company C has a supply contract with company D."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct RelationContent {
    /// Kind of relation. Lowercase snake_case, extensible vocabulary.
    /// Examples: `ownership`, `trade_flow`, `supply_contract`,
    /// `board_seat`, `sanctions_designation`.
    pub kind: String,

    /// Source of the edge. For "A owns B", this is A.
    pub from: EntityId,

    /// Target of the edge.
    pub to: EntityId,

    /// Optional magnitude (ownership %, flow volume, contract value).
    /// Using the same shape as ObservationContent for consistency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnitude: Option<ObservationContent>,

    /// Optional validity window end. Some relations are point-in-time
    /// (a one-time sanctions designation), others have duration
    /// (a 5-year supply contract). When set, the envelope's `valid_at`
    /// is the start of the window and this is the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// EntityAttributeContent
// ---------------------------------------------------------------------------

/// The content of one attribute of an entity at a point in time.
///
/// Entity attributes are stored as a stream of these: an entity's state
/// at time T is the aggregation of its attribute records up to T.
/// This keeps the schema open — adding a new attribute kind doesn't
/// require a schema change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityAttributeContent {
    /// Which entity this attribute applies to.
    pub entity_id: EntityId,

    /// Attribute key. Lowercase snake_case.
    /// Examples: `legal_name`, `headquarters_country`, `ticker`,
    /// `employee_count`, `primary_commodity`.
    pub key: String,

    /// Attribute value as a typed enum to preserve semantic types.
    pub value: AttributeValue,
}

/// Typed attribute value. We use an enum instead of `serde_json::Value`
/// because we want the schema to express which type kinds are legal —
/// attributes can't be arbitrary JSON, they must be one of these shapes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AttributeValue {
    Text(String),
    Number {
        value: f64,
        unit: Option<Unit>,
    },
    Country(CountryCode),
    /// A tag / categorical value drawn from the open [`Topic`] namespace.
    /// Use this for attributes like "primary commodity", "sector",
    /// "technology area" — anything that was previously a commodity
    /// reference is now a Topic.
    Topic(Topic),
    Entity(EntityId),
    Boolean(bool),
    /// For attributes that are lists (e.g. "subsidiaries").
    EntityList(Vec<EntityId>),
    TopicList(Vec<Topic>),
}

// ---------------------------------------------------------------------------
// AssertedContent — what an Assertion can wrap
// ---------------------------------------------------------------------------

/// The content shapes an Assertion can claim.
///
/// Note this is a **subset** of the record types — you can't assert an
/// Entity (an entity either exists or it doesn't; assertions about
/// entities are about their *attributes*, not the entity itself), and
/// you can't assert a Document (a document is raw content, not a claim).
/// You can't assert an Assertion (no meta-claims).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertedContent {
    Observation(ObservationContent),
    Event(EventContent),
    Relation(RelationContent),
    EntityAttribute(EntityAttributeContent),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_content_roundtrip() {
        let c = ObservationContent {
            metric: "production".into(),
            value: 142_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: Some(5000.0),
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: ObservationContent = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn asserted_content_discriminates() {
        let obs = AssertedContent::Observation(ObservationContent {
            metric: "price".into(),
            value: 11800.0,
            unit: Unit::new("USD/t").unwrap(),
            value_uncertainty: None,
            currency: Some(Currency::new("USD").unwrap()),
            period: ObservationPeriod::Instant,
            geometry: None,
        });
        let json = serde_json::to_string(&obs).unwrap();
        // Tagged-union serialization — the "kind" field is present
        assert!(json.contains("\"kind\":\"observation\""));
        let back: AssertedContent = serde_json::from_str(&json).unwrap();
        assert_eq!(obs, back);
    }

    #[test]
    fn attribute_value_preserves_type() {
        let v = AttributeValue::Country(CountryCode::new("CL").unwrap());
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("country"));
        let back: AttributeValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    // ---------------------------------------------------------------
    // Track B (Session 28, ADR 0007 amendment 4)
    //
    // The recipe-author prompt now embeds JSON Schemas for the three
    // authorable record types (Observation, Event, Relation) via the
    // `{{TARGET_RECORD_SCHEMA}}` placeholder. The schemars-derived
    // schema is the wire-truth the LLM is authoring against; the
    // prompt no longer relies on prose alone for type expectations.
    //
    // These tests pin the derive: if `schema_for!` ever fails to
    // compile or returns garbage, recipe authoring loses its
    // schema-aware framing. They are deliberately structural (no
    // exact-shape assertions on the schemars output, which can
    // change between minor versions) — we only assert that the
    // schema is non-empty and serializes to valid JSON.
    //
    // The pipeline crate's `target_record_schemas()` helper is the
    // production consumer; see
    // `crates/pipeline/src/recipe_author.rs::tests::
    // target_record_schemas_emits_all_three_record_types`.
    // ---------------------------------------------------------------

    #[test]
    fn observation_content_emits_json_schema() {
        let schema = schemars::schema_for!(ObservationContent);
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("metric"));
        assert!(s.contains("value"));
        assert!(s.contains("unit"));
    }

    #[test]
    fn event_content_emits_json_schema() {
        let schema = schemars::schema_for!(EventContent);
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("event_type"));
        assert!(s.contains("headline"));
    }

    #[test]
    fn relation_content_emits_json_schema() {
        let schema = schemars::schema_for!(RelationContent);
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("kind"));
        assert!(s.contains("from"));
        assert!(s.contains("to"));
    }
}
