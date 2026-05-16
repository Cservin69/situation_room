//! Assertion storage: insert + get.
//!
//! Assertions have `claimant` + `stance` + `content_kind` (the
//! AssertedContent discriminator) + JSON content, alongside the
//! standard envelope columns.

use duckdb::params;
use situation_room_core::schema::content::AssertedContent;
use situation_room_core::vocab::{EntityId, Stance};
use situation_room_core::Assertion;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

/// Discriminator for the content_kind column. Mirrors the serde tag
/// produced by `AssertedContent` (snake_case).
fn content_kind_of(content: &AssertedContent) -> &'static str {
    match content {
        AssertedContent::Observation(_) => "observation",
        AssertedContent::Event(_) => "event",
        AssertedContent::Relation(_) => "relation",
        AssertedContent::EntityAttribute(_) => "entity_attribute",
    }
}

impl Store {
    pub fn insert_assertion(&self, a: &Assertion) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&a.envelope)?;
        let content_json = serde_json::to_string(&a.content)?;
        let stance_str = serde_json::to_value(a.stance)?
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let content_kind = content_kind_of(&a.content);

        tx.execute(
            "INSERT INTO assertions (
                id, dedup_key, claimant, stance, content_kind, content,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                selector_path, raw_bytes_excerpt
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                a.id,
                a.dedup_key,
                a.claimant.as_str(),
                stance_str,
                content_kind,
                content_json,
                cols.source_id,
                cols.source_url,
                cols.source_published_at,
                cols.license,
                cols.tags_json,
                cols.subject_time_json,
                cols.observed_at,
                cols.valid_at,
                cols.confidence,
                cols.selector_path,
                cols.raw_bytes_excerpt,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        crate::envelope_io::insert_subjects_and_derivation(
            &tx,
            a.id,
            "assertion",
            &a.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    /// Session 82 (ADR 0021 idempotency surface). Sibling of
    /// `observation_exists_by_dedup_key` etc. The promote stage's
    /// EntityAttribute pathway emits a consensus-/authoritative-
    /// stamped `Assertion` (rather than a fresh observation/event/
    /// relation); this is the equivalent existence check.
    pub fn assertion_exists_by_dedup_key(&self, dedup_key: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assertions WHERE dedup_key = ?",
                params![dedup_key],
                |r| r.get(0),
            )
            .map_err(StorageError::DuckDb)?;
        Ok(count > 0)
    }

    pub fn get_assertion(&self, id: Uuid) -> Result<Assertion> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        #[allow(clippy::type_complexity)]
        let (row_id, dedup_key, claimant_s, stance_s, _content_kind, content_json, raw): (
            Uuid,
            Option<String>,
            String,
            String,
            String,
            String,
            EnvelopeRow,
        ) = conn
            .query_row(
                "SELECT id, dedup_key, claimant, stance, content_kind, content,
                        source_id, source_url, source_published_at,
                        license, tags, subject_time, observed_at, valid_at, confidence,
                        selector_path, raw_bytes_excerpt
                 FROM assertions
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
                        EnvelopeRow {
                            source_id: r.get(6)?,
                            source_url: r.get(7)?,
                            source_published_at: r.get(8)?,
                            license: r.get(9)?,
                            tags_json: r.get(10)?,
                            subject_time_json: r.get(11)?,
                            observed_at: r.get(12)?,
                            valid_at: r.get(13)?,
                            confidence_f: r.get(14)?,
                            selector_path: r.get(15)?,
                            raw_bytes_excerpt: r.get(16)?,
                        },
                    ))
                },
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("assertion {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let claimant = EntityId::new(claimant_s)
            .map_err(|e| StorageError::Other(format!("claimant round-trip: {e}")))?;
        let stance: Stance =
            serde_json::from_value(serde_json::Value::String(stance_s)).map_err(StorageError::Serde)?;
        let content: AssertedContent = serde_json::from_str(&content_json)?;

        Ok(Assertion {
            id: row_id,
            dedup_key,
            claimant,
            stance,
            content,
            envelope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, Topic, Unit};

    fn sample_assertion() -> Assertion {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "reuters".into(),
                source_url: Some("https://reuters.com/article/xyz".into()),
                source_published_at: None,
                license: "fair_use".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::new(0.7).unwrap(),
        };
        let content = AssertedContent::Observation(ObservationContent {
            metric: "production".into(),
            value: 142_000.0,
            unit: Unit::new("t").unwrap(),
            value_uncertainty: None,
            currency: None,
            period: ObservationPeriod::Annual,
            geometry: None,
        });
        Assertion::new(
            EntityId::new("usgs").unwrap(),
            Stance::Asserted,
            content,
            envelope,
        )
    }

    #[test]
    fn assertion_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let a = sample_assertion();
        store.insert_assertion(&a).unwrap();

        let back = store.get_assertion(a.id).unwrap();
        assert_eq!(back.id, a.id);
        assert_eq!(back.claimant, a.claimant);
        assert_eq!(back.stance, a.stance);
        assert_eq!(back.content, a.content);
    }
}
