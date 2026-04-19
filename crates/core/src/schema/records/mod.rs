//! The six record types.
//!
//! See `docs/adr/0003-six-record-types.md`.

pub mod observation;
pub mod event;
pub mod entity;
pub mod relation;
pub mod document;
pub mod assertion;
pub mod geometry;

use serde::{Deserialize, Serialize};

/// The unified record enum — every piece of data in Stockpile is one of these.
///
/// Phase 2 will replace the unit variants with the real payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Record {
    Observation(observation::Observation),
    Event(event::Event),
    Entity(entity::Entity),
    Relation(relation::Relation),
    Document(document::Document),
    Assertion(assertion::Assertion),
}
