//! Event storage: insert + get.

use duckdb::params;
use situation_room_core::Event;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

impl Store {
    pub fn insert_event(&self, ev: &Event) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&ev.envelope)?;
        let content_json = serde_json::to_string(&ev.content)?;

        tx.execute(
            "INSERT INTO events (
                id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                ev.id,
                ev.dedup_key,
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

        crate::envelope_io::insert_subjects_and_derivation(&tx, ev.id, "event", &ev.envelope)?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    /// Session 82 (ADR 0021 idempotency surface). Sibling of
    /// `observation_exists_by_dedup_key` / `relation_exists_by_dedup_key`.
    /// The promote stage queries this before issuing an insert so a
    /// re-run lands as `skipped_already_promoted`.
    pub fn event_exists_by_dedup_key(&self, dedup_key: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE dedup_key = ?",
                params![dedup_key],
                |r| r.get(0),
            )
            .map_err(StorageError::DuckDb)?;
        Ok(count > 0)
    }

    pub fn get_event(&self, id: Uuid) -> Result<Event> {
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
                 FROM events
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
                    StorageError::NotFound(format!("event {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let content: situation_room_core::EventContent = serde_json::from_str(&content_json)?;

        Ok(Event {
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
    use chrono::{TimeZone, Utc};
    use situation_room_core::schema::content::{EventContent, EventDirection};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, EntityId, EventType, Topic};

    fn sample_event() -> Event {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "reuters".into(),
                source_url: Some("https://reuters.com/article/xyz".into()),
                source_published_at: Some(Utc.with_ymd_and_hms(2025, 3, 10, 14, 0, 0).unwrap()),
                license: "fair_use".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![EntityId::new("sqm").unwrap()],
                places: vec![],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec!["direction:supply_negative".into()],
            valid_at: Some(Utc.with_ymd_and_hms(2025, 3, 10, 0, 0, 0).unwrap()),
            observed_at: Utc.with_ymd_and_hms(2025, 3, 10, 14, 5, 0).unwrap(),
            confidence: Confidence::new(0.8).unwrap(),
        };
        let content = EventContent {
            event_type: EventType::new("production_cut").unwrap(),
            headline: "SQM announces 10% production cut for Q2 2025.".into(),
            actors: vec![EntityId::new("sqm").unwrap()],
            direction: Some(EventDirection::SupplyNegative),
            magnitude: None,
            geometry: None,
        };
        Event::new(envelope, content)
    }

    #[test]
    fn event_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ev = sample_event();
        store.insert_event(&ev).unwrap();

        let back = store.get_event(ev.id).unwrap();
        assert_eq!(back.id, ev.id);
        assert_eq!(back.content, ev.content);
        assert_eq!(back.envelope.subjects.topics, ev.envelope.subjects.topics);
        assert_eq!(back.envelope.tags, ev.envelope.tags);
    }

    #[test]
    fn missing_event_returns_not_found() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let missing = uuid::Uuid::now_v7();
        let err = store.get_event(missing).unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }
}
