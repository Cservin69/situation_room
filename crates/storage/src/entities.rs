//! Entity storage: insert + get.
//!
//! Entities differ from the other record types: they have no `content`
//! JSON column — the business data is `entity_id` + `kind` +
//! `canonical_name` + optional `geometry`, stored as flat columns.
//! They also have no `dedup_key` — the `entity_id` is the business key
//! with a UNIQUE constraint.

use duckdb::params;
use stockpile_core::vocab::EntityId;
use stockpile_core::Entity;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

impl Store {
    pub fn insert_entity(&self, ent: &Entity) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&ent.envelope)?;
        let geometry_json = match &ent.geometry {
            Some(g) => Some(serde_json::to_string(g)?),
            None => None,
        };

        tx.execute(
            "INSERT INTO entities (
                id, entity_id, kind, canonical_name, geometry,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                ent.id,
                ent.entity_id.as_str(),
                ent.kind,
                ent.canonical_name,
                geometry_json,
                cols.source_id,
                cols.source_url,
                cols.source_published_at,
                cols.license,
                cols.tags_json,
                cols.subject_time_json,
                cols.observed_at,
                cols.valid_at,
                cols.confidence,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        crate::envelope_io::insert_subjects_and_derivation(
            &tx,
            ent.id,
            "entity",
            &ent.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    pub fn get_entity(&self, id: Uuid) -> Result<Entity> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        #[allow(clippy::type_complexity)]
        let (row_id, entity_id_s, kind, canonical_name, geometry_json, raw): (
            Uuid,
            String,
            String,
            String,
            Option<String>,
            EnvelopeRow,
        ) = conn
            .query_row(
                "SELECT id, entity_id, kind, canonical_name, geometry,
                        source_id, source_url, source_published_at,
                        license, tags, subject_time, observed_at, valid_at, confidence
                 FROM entities
                 WHERE id = ?",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        EnvelopeRow {
                            source_id: r.get(5)?,
                            source_url: r.get(6)?,
                            source_published_at: r.get(7)?,
                            license: r.get(8)?,
                            tags_json: r.get(9)?,
                            subject_time_json: r.get(10)?,
                            observed_at: r.get(11)?,
                            valid_at: r.get(12)?,
                            confidence_f: r.get(13)?,
                        },
                    ))
                },
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("entity {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let entity_id = EntityId::new(entity_id_s)
            .map_err(|e| StorageError::Other(format!("entity_id round-trip: {e}")))?;
        let geometry = match geometry_json {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        };

        Ok(Entity {
            id: row_id,
            entity_id,
            kind,
            canonical_name,
            geometry,
            envelope,
        })
    }

    /// Fetch an entity by its business key. Uses the unique index on
    /// `entities.entity_id`.
    pub fn get_entity_by_business_id(&self, entity_id: &EntityId) -> Result<Entity> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let row_id: Uuid = conn
            .query_row(
                "SELECT id FROM entities WHERE entity_id = ?",
                params![entity_id.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("entity {entity_id}"))
                }
                other => StorageError::DuckDb(other),
            })?;
        drop(conn); // Release before re-acquiring in get_entity
        self.get_entity(row_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use stockpile_core::schema::envelope::{Envelope, Provenance, Subjects};
    use stockpile_core::vocab::Confidence;

    fn sample_entity() -> Entity {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "curated".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects::default(),
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Entity::new(
            EntityId::new("sqm").unwrap(),
            "company",
            "Sociedad Química y Minera de Chile",
            envelope,
        )
    }

    #[test]
    fn entity_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent = sample_entity();
        store.insert_entity(&ent).unwrap();

        let back = store.get_entity(ent.id).unwrap();
        assert_eq!(back.id, ent.id);
        assert_eq!(back.entity_id, ent.entity_id);
        assert_eq!(back.kind, "company");
        assert_eq!(back.canonical_name, ent.canonical_name);
    }

    #[test]
    fn entity_lookup_by_business_id_works() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent = sample_entity();
        store.insert_entity(&ent).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("sqm").unwrap())
            .unwrap();
        assert_eq!(back.id, ent.id);
    }

    #[test]
    fn duplicate_entity_id_violates_unique_constraint() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent1 = sample_entity();
        store.insert_entity(&ent1).unwrap();

        // A second entity with the same business id but a fresh UUID
        // should fail on the unique index.
        let ent2 = sample_entity();
        let result = store.insert_entity(&ent2);
        assert!(result.is_err(), "expected unique-violation error");
    }
}
