//! Schema: the six record types, the envelope they all carry, the shared
//! content types, and the [`Record`] enum that ties them together.
//!
//! See `docs/adr/0003-six-record-types.md` for the rationale behind the
//! six-record-types design, and `docs/adr/0004-assertion-promotion.md`
//! for how Assertions flow through the pipeline.

pub mod content;
pub mod envelope;
pub mod geometry;
pub mod records;

// Re-exports for ergonomic import by consumers.
pub use content::{
    AssertedContent, AttributeValue, EntityAttributeContent, EventContent, EventDirection,
    ObservationContent, ObservationPeriod, RelationContent,
};
pub use envelope::{
    DerivationRole, DerivedFrom, Envelope, PlaceRef, Provenance, Subjects, TimeScope,
};
pub use geometry::{Geometry, LineStringGeom, MultiPolygonGeom, PointGeom, PolygonGeom, Position};
pub use records::{Assertion, Document, Entity, Event, Observation, Record, RecordType, Relation};
