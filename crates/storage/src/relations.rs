//! Relation storage: insert + get.

use duckdb::params;
use situation_room_core::Relation;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

impl Store {
    pub fn insert_relation(&self, rel: &Relation) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&rel.envelope)?;
        let content_json = serde_json::to_string(&rel.content)?;

        tx.execute(
            "INSERT INTO relations (
                id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                rel.id,
                rel.dedup_key,
                cols.source_id,
                cols.source_url,
                cols.source_published_at,
                cols.license,
                cols.tags_json,
                cols.subject_time_json,
                cols.observed_at,
                cols.valid_at,
                cols.confidence,
                content_json,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        crate::envelope_io::insert_subjects_and_derivation(
            &tx,
            rel.id,
            "relation",
            &rel.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    pub fn get_relation(&self, id: Uuid) -> Result<Relation> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        #[allow(clippy::type_complexity)]
        let (row_id, dedup_key, raw, content_json): (
            Uuid,
            Option<String>,
            EnvelopeRow,
            String,
        ) = conn
            .query_row(
                "SELECT id, dedup_key, source_id, source_url, source_published_at,
                        license, tags, subject_time, observed_at, valid_at, confidence,
                        content
                 FROM relations
                 WHERE id = ?",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        EnvelopeRow {
                            source_id: r.get(2)?,
                            source_url: r.get(3)?,
                            source_published_at: r.get(4)?,
                            license: r.get(5)?,
                            tags_json: r.get(6)?,
                            subject_time_json: r.get(7)?,
                            observed_at: r.get(8)?,
                            valid_at: r.get(9)?,
                            confidence_f: r.get(10)?,
                        },
                        r.get(11)?,
                    ))
                },
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("relation {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let content: situation_room_core::RelationContent = serde_json::from_str(&content_json)?;

        Ok(Relation {
            id: row_id,
            dedup_key,
            envelope,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use situation_room_core::schema::content::RelationContent;
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, EntityId, Topic};

    fn sample_relation() -> Relation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "sec_edgar".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![
                    EntityId::new("albemarle").unwrap(),
                    EntityId::new("liontown_resources").unwrap(),
                ],
                places: vec![],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let content = RelationContent {
            kind: "supply_contract".into(),
            from: EntityId::new("liontown_resources").unwrap(),
            to: EntityId::new("albemarle").unwrap(),
            magnitude: None,
            valid_until: None,
        };
        Relation::new(envelope, content)
    }

    #[test]
    fn relation_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let rel = sample_relation();
        store.insert_relation(&rel).unwrap();

        let back = store.get_relation(rel.id).unwrap();
        assert_eq!(back.id, rel.id);
        assert_eq!(back.content, rel.content);
        assert_eq!(back.envelope.subjects.entities.len(), 2);
    }
}
