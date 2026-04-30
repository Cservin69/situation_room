//! Observation storage: insert + get.
//!
//! See [`super::envelope_io`] for the shared envelope / subject writes.

use duckdb::params;
use situation_room_core::Observation;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

impl Store {
    /// Insert an Observation. Writes to `observations` and all relevant
    /// junction tables in a single transaction.
    pub fn insert_observation(&self, obs: &Observation) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&obs.envelope)?;
        let content_json = serde_json::to_string(&obs.content)?;

        tx.execute(
            "INSERT INTO observations (
                id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                obs.id,
                obs.dedup_key,
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
            obs.id,
            "observation",
            &obs.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    /// Fetch an Observation by id.
    pub fn get_observation(&self, id: Uuid) -> Result<Observation> {
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
                 FROM observations
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
                    StorageError::NotFound(format!("observation {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let content: situation_room_core::ObservationContent = serde_json::from_str(&content_json)?;

        Ok(Observation {
            id: row_id,
            dedup_key,
            envelope,
            content,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{
        DerivationRole, DerivedFrom, Envelope, PlaceRef, Provenance, Subjects,
    };
    use situation_room_core::vocab::{Confidence, CountryCode, EntityId, Topic, Unit};

    fn sample_observation() -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "usgs_mcs".into(),
                source_url: Some(
                    "https://pubs.usgs.gov/periodicals/mcs2025/mcs2025-lithium.pdf".into(),
                ),
                source_published_at: Some(Utc.with_ymd_and_hms(2025, 1, 31, 0, 0, 0).unwrap()),
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![EntityId::new("sqm").unwrap()],
                places: vec![PlaceRef::Country(CountryCode::new("CL").unwrap())],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec!["source_tier:authoritative".into()],
            valid_at: Some(Utc.with_ymd_and_hms(2024, 12, 31, 0, 0, 0).unwrap()),
            observed_at: Utc.with_ymd_and_hms(2025, 2, 1, 10, 0, 0).unwrap(),
            confidence: Confidence::ONE,
        };
        let content = ObservationContent {
            metric: "production".into(),
            value: 44_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: Some(1000.0),
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        };
        Observation::new(envelope, content).with_dedup_key("usgs:Li:production:CL:2024")
    }

    #[test]
    fn migrations_apply_cleanly_on_fresh_store() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
    }

    #[test]
    fn observation_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let obs = sample_observation();
        store.insert_observation(&obs).unwrap();

        let back = store.get_observation(obs.id).unwrap();
        assert_eq!(back.id, obs.id);
        assert_eq!(back.dedup_key, obs.dedup_key);
        assert_eq!(back.content, obs.content);
        assert_eq!(
            back.envelope.provenance.source_id,
            obs.envelope.provenance.source_id
        );
        assert_eq!(back.envelope.subjects.topics, obs.envelope.subjects.topics);
        assert_eq!(back.envelope.subjects.entities, obs.envelope.subjects.entities);
        assert_eq!(back.envelope.subjects.places, obs.envelope.subjects.places);
        assert_eq!(back.envelope.tags, obs.envelope.tags);
        assert_eq!(back.envelope.observed_at, obs.envelope.observed_at);
        assert_eq!(back.envelope.valid_at, obs.envelope.valid_at);
    }

    #[test]
    fn observation_with_derivation_chain_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut obs = sample_observation();
        let parent_id = uuid::Uuid::now_v7();
        obs.envelope.provenance.derived_from.push(DerivedFrom {
            record_id: parent_id,
            role: DerivationRole::Promotion,
        });

        store.insert_observation(&obs).unwrap();
        let back = store.get_observation(obs.id).unwrap();

        assert_eq!(back.envelope.provenance.derived_from.len(), 1);
        assert_eq!(back.envelope.provenance.derived_from[0].record_id, parent_id);
        assert_eq!(
            back.envelope.provenance.derived_from[0].role,
            DerivationRole::Promotion
        );
    }

    #[test]
    fn missing_observation_returns_not_found() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let missing = uuid::Uuid::now_v7();
        let err = store.get_observation(missing).unwrap_err();
        match err {
            StorageError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
