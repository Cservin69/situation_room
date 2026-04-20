//! Observation storage: insert + get.
//!
//! Envelope columns (`source_id`, `source_url`, `observed_at`,
//! `valid_at`, `confidence`, `license`, `source_published_at`) are
//! stored flat for columnar query performance. The `content` column
//! holds the [`ObservationContent`] as JSON; it's varied per metric and
//! isn't on the hot filter path.
//!
//! `tags` and `subject_time` are stored as JSON columns — small,
//! always 0-or-1 items in aggregate, and junction tables would be
//! overkill. The three multi-valued subject dimensions (entities,
//! places, topics) go to dedicated junction tables.
//!
//! ## What this module covers in Phase 2e
//!
//! Insert one Observation, retrieve it by id, round-trip with full
//! envelope fidelity including subjects and derivation chain. Other
//! record types follow the same template; they'll land in subsequent
//! passes once the shape here is proven.

use duckdb::params;
use stockpile_core::schema::envelope::DerivedFrom;
use stockpile_core::Observation;
use uuid::Uuid;

use crate::connection::Store;
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

        let env = &obs.envelope;
        let prov = &env.provenance;

        let tags_json = serde_json::to_string(&env.tags)?;
        let subject_time_json = match &env.subjects.time {
            Some(t) => Some(serde_json::to_string(t)?),
            None => None,
        };
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
                prov.source_id,
                prov.source_url,
                prov.source_published_at,
                prov.license,
                tags_json,
                subject_time_json,
                env.observed_at,
                env.valid_at,
                env.confidence.value(),
                content_json,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        // Subjects: entities
        for e in &env.subjects.entities {
            tx.execute(
                "INSERT INTO record_subjects_entities (record_id, record_type, entity_id)
                 VALUES (?, ?, ?)",
                params![obs.id, "observation", e.as_str()],
            )
            .map_err(StorageError::DuckDb)?;
        }

        // Subjects: places — store kind + JSON value
        for p in &env.subjects.places {
            let json = serde_json::to_value(p)?;
            let kind = json
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let value_json = serde_json::to_string(&json)?;
            tx.execute(
                "INSERT INTO record_subjects_places (record_id, record_type, place_kind, place_value)
                 VALUES (?, ?, ?, ?)",
                params![obs.id, "observation", kind, value_json],
            )
            .map_err(StorageError::DuckDb)?;
        }

        // Subjects: topics
        for t in &env.subjects.topics {
            tx.execute(
                "INSERT INTO record_subjects_topics (record_id, record_type, topic)
                 VALUES (?, ?, ?)",
                params![obs.id, "observation", t.as_str()],
            )
            .map_err(StorageError::DuckDb)?;
        }

        // Derivation chain
        for d in &prov.derived_from {
            let role = serde_json::to_value(&d.role)?
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            tx.execute(
                "INSERT INTO record_derived_from (child_id, child_type, parent_id, parent_type, role)
                 VALUES (?, ?, ?, ?, ?)",
                params![obs.id, "observation", d.record_id, "unknown", role],
            )
            .map_err(StorageError::DuckDb)?;
        }

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    /// Fetch an Observation by id. Reconstructs the full envelope
    /// including subjects and derivation chain from the junction
    /// tables.
    pub fn get_observation(&self, id: Uuid) -> Result<Observation> {
        use stockpile_core::schema::envelope::{
            DerivationRole, Envelope, PlaceRef, Provenance, Subjects, TimeScope,
        };
        use stockpile_core::vocab::{Confidence, EntityId, Topic};

        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // Main row
        #[allow(clippy::type_complexity)]
        let row: (
            Uuid,                                             // id
            Option<String>,                                   // dedup_key
            String,                                           // source_id
            Option<String>,                                   // source_url
            Option<chrono::DateTime<chrono::Utc>>,            // source_published_at
            String,                                           // license
            String,                                           // tags_json
            Option<String>,                                   // subject_time_json
            chrono::DateTime<chrono::Utc>,                    // observed_at
            Option<chrono::DateTime<chrono::Utc>>,            // valid_at
            f64,                                              // confidence
            String,                                           // content_json
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
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
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

        let (
            row_id,
            dedup_key,
            source_id,
            source_url,
            source_published_at,
            license,
            tags_json,
            subject_time_json,
            observed_at,
            valid_at,
            confidence_f,
            content_json,
        ) = row;

        let tags: Vec<String> = serde_json::from_str(&tags_json)?;
        let time: Option<TimeScope> = match subject_time_json {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        };
        let content: stockpile_core::ObservationContent = serde_json::from_str(&content_json)?;

        // Subjects: entities
        let entities: Vec<EntityId> = {
            let mut stmt = conn
                .prepare(
                    "SELECT entity_id FROM record_subjects_entities
                     WHERE record_id = ?",
                )
                .map_err(StorageError::DuckDb)?;
            let rows = stmt
                .query_map(params![id], |r| r.get::<_, String>(0))
                .map_err(StorageError::DuckDb)?;
            let mut out = Vec::new();
            for r in rows {
                let s = r.map_err(StorageError::DuckDb)?;
                out.push(
                    EntityId::new(s.clone())
                        .map_err(|e| StorageError::Other(format!("entity_id round-trip: {e}")))?,
                );
            }
            out
        };

        // Subjects: places
        let places: Vec<PlaceRef> = {
            let mut stmt = conn
                .prepare(
                    "SELECT place_value FROM record_subjects_places
                     WHERE record_id = ?",
                )
                .map_err(StorageError::DuckDb)?;
            let rows = stmt
                .query_map(params![id], |r| r.get::<_, String>(0))
                .map_err(StorageError::DuckDb)?;
            let mut out = Vec::new();
            for r in rows {
                let s = r.map_err(StorageError::DuckDb)?;
                out.push(serde_json::from_str(&s)?);
            }
            out
        };

        // Subjects: topics
        let topics: Vec<Topic> = {
            let mut stmt = conn
                .prepare(
                    "SELECT topic FROM record_subjects_topics
                     WHERE record_id = ?",
                )
                .map_err(StorageError::DuckDb)?;
            let rows = stmt
                .query_map(params![id], |r| r.get::<_, String>(0))
                .map_err(StorageError::DuckDb)?;
            let mut out = Vec::new();
            for r in rows {
                let s = r.map_err(StorageError::DuckDb)?;
                out.push(
                    Topic::new(s.clone())
                        .map_err(|e| StorageError::Other(format!("topic round-trip: {e}")))?,
                );
            }
            out
        };

        // Derivation chain
        let derived_from: Vec<DerivedFrom> = {
            let mut stmt = conn
                .prepare(
                    "SELECT parent_id, role FROM record_derived_from
                     WHERE child_id = ?",
                )
                .map_err(StorageError::DuckDb)?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, Uuid>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(StorageError::DuckDb)?;
            let mut out = Vec::new();
            for r in rows {
                let (pid, role_s) = r.map_err(StorageError::DuckDb)?;
                // Reconstruct DerivationRole from its serde name.
                let role: DerivationRole = serde_json::from_value(
                    serde_json::Value::String(role_s.clone()),
                )
                .map_err(StorageError::Serde)?;
                out.push(DerivedFrom {
                    record_id: pid,
                    role,
                });
            }
            out
        };

        let envelope = Envelope {
            provenance: Provenance {
                source_id,
                source_url,
                source_published_at,
                license,
                derived_from,
            },
            subjects: Subjects {
                entities,
                places,
                time,
                topics,
            },
            tags,
            valid_at,
            observed_at,
            confidence: Confidence::clamp(confidence_f as f32),
        };

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
    use stockpile_core::schema::content::{ObservationContent, ObservationPeriod};
    use stockpile_core::schema::envelope::{
        DerivationRole, DerivedFrom, Envelope, PlaceRef, Provenance, Subjects,
    };
    use stockpile_core::vocab::{Confidence, CountryCode, EntityId, Topic, Unit};

    fn sample_observation() -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "usgs_mcs".into(),
                source_url: Some("https://pubs.usgs.gov/periodicals/mcs2025/mcs2025-lithium.pdf".into()),
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
        // Second call is a no-op.
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
        assert_eq!(
            back.envelope.subjects.entities,
            obs.envelope.subjects.entities
        );
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
