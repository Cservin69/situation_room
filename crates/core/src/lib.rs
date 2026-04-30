//! # situation_room-core
//!
//! The schema. This crate defines:
//!
//! - The six record types: [`Observation`], [`Event`], [`Entity`],
//!   [`Relation`], [`Document`], [`Assertion`] (in [`schema::records`]).
//! - The [`Envelope`] that every record carries (provenance, subjects,
//!   tags, timestamps, confidence).
//! - The bare content types ([`ObservationContent`], [`EventContent`],
//!   [`RelationContent`], [`EntityAttributeContent`]) that both full
//!   records and [`Assertion`]s reference, so claims and measurements
//!   share one shape.
//! - [`Geometry`] as an optional field on Entity/Event/Observation
//!   (NOT a top-level record type — see ADR 0003).
//! - Controlled vocabularies in [`vocab`]: countries, entities, event
//!   types, units, currencies, topics, stance, confidence.
//!
//! `situation_room-core` has zero dependencies on other workspace crates by
//! design: if you want to know what situation_room *is*, you read this crate
//! and you have the full picture.
//!
//! ## Design note: "general by default"
//!
//! situation_room does not hardcode any domain — there is no first-class
//! "commodity", "sector", or "technology" concept in the schema. The
//! six record types are the universal schema. Domain categorization
//! happens through free-form [`Topic`] tags in
//! [`Subjects`](schema::envelope::Subjects), populated by the LLM research
//! planner at classification time. Commodities and sectors are equally
//! first-class — they're both topics. See [`vocab`] for the rationale.
//!
//! ## Quick tour
//!
//! ```ignore
//! use situation_room_core::{
//!     schema::{
//!         content::{ObservationContent, ObservationPeriod},
//!         envelope::{Envelope, Provenance, Subjects},
//!         Observation, Record,
//!     },
//!     vocab::{Confidence, Topic, Unit},
//! };
//!
//! let envelope = Envelope {
//!     provenance: Provenance {
//!         source_id: "usgs_mcs".into(),
//!         source_url: None,
//!         source_published_at: None,
//!         license: "public_domain".into(),
//!         derived_from: vec![],
//!     },
//!     subjects: Subjects::topic(Topic::new("Li")?),
//!     tags: vec![],
//!     valid_at: None,
//!     observed_at: chrono::Utc::now(),
//!     confidence: Confidence::ONE,
//! };
//!
//! let obs = Observation::new(envelope, ObservationContent {
//!     metric: "production".into(),
//!     value: 142_000.0,
//!     unit: Unit::new("t")?,
//!     value_uncertainty: None,
//!     currency: None,
//!     period: ObservationPeriod::Annual,
//!     geometry: None,
//! });
//!
//! let rec: Record = obs.into();
//! ```

pub mod error;
pub mod schema;
pub mod vocab;

// Top-level re-exports for the most common types.
pub use error::CoreError;
pub use schema::{
    // Content types
    AssertedContent, EntityAttributeContent, EventContent, ObservationContent, RelationContent,
    // Envelope + parts
    Envelope, PlaceRef, Provenance, Subjects, TimeScope,
    // Geometry
    Geometry,
    // Record types + enum
    Assertion, Document, Entity, Event, Observation, Record, RecordType, Relation,
};
pub use vocab::{
    Confidence, CountryCode, Currency, EntityId, EventType, Stance, Topic, Unit,
};
