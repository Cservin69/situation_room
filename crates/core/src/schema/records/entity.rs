//! `Entity` — a thing that persists: a company, mine, vessel, agency, etc.
//!
//! The entity record itself is mostly identity + kind + canonical name +
//! optional geometry. Attributes (legal name variants, headquarters,
//! employee counts, etc.) are stored as separate
//! [`EntityAttributeContent`](crate::schema::content::EntityAttributeContent)
//! records tied to this entity's `entity_id`, so attributes can change
//! over time without re-creating the entity.

use crate::schema::envelope::Envelope;
use crate::schema::geometry::Geometry;
use crate::vocab::EntityId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    pub id: Uuid,

    /// Stable business identifier. Distinct from the `id` (UUID): the
    /// `entity_id` is what other records reference (e.g. a Relation's
    /// `from`/`to`, an Observation's entity subject). The UUID is the
    /// storage-layer primary key.
    pub entity_id: EntityId,

    /// Kind of entity. Controlled vocabulary, lowercase snake_case.
    /// Examples: `company`, `mine`, `vessel`, `port`, `person`,
    /// `government_agency`, `refinery`, `smelter`, `contract`.
    pub kind: String,

    /// Canonical display name (e.g. "Sociedad Química y Minera de Chile").
    pub canonical_name: String,

    /// Optional location. For a mine, the pit coordinates. For a company,
    /// its headquarters. For a vessel, its current position (updated by
    /// a stream of Observations on the same entity_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,

    pub envelope: Envelope,
}

impl Entity {
    pub fn new(
        entity_id: EntityId,
        kind: impl Into<String>,
        canonical_name: impl Into<String>,
        envelope: Envelope,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            entity_id,
            kind: kind.into(),
            canonical_name: canonical_name.into(),
            geometry: None,
            envelope,
        }
    }

    pub fn with_geometry(mut self, g: Geometry) -> Self {
        self.geometry = Some(g);
        self
    }
}
