//! Shared helpers for record envelope and subject persistence.
//!
//! Every record type has the same envelope shape (provenance, subjects,
//! tags, timestamps, confidence). Rather than duplicate the envelope
//! serialization and subject-junction writes across six modules, those
//! operations live here.
//!
//! Per-record-type modules ([`super::observations`], [`super::events`],
//! etc.) still own their own insert/get flow — the schema is different
//! per type and the `content` column shape varies. This module just
//! handles what's common.

use duckdb::{params, Connection, Transaction};
use situation_room_core::schema::envelope::{
    DerivationRole, DerivedFrom, Envelope, PlaceRef, Provenance, Subjects, TimeScope,
};
use situation_room_core::vocab::{Confidence, EntityId, Topic};
use uuid::Uuid;

use crate::{Result, StorageError};

/// Serializable fragments of the envelope that need to go into the
/// flat record-table columns. The caller uses these directly in its
/// INSERT parameters.
pub(crate) struct EnvelopeColumns {
    pub source_id: String,
    pub source_url: Option<String>,
    pub source_published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub license: String,
    pub tags_json: String,
    pub subject_time_json: Option<String>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub valid_at: Option<chrono::DateTime<chrono::Utc>>,
    pub confidence: f32,
    /// Session 87: per-record selector trace. `None` for promoted /
    /// derived / LLM-synthesized rows.
    pub selector_path: Option<String>,
    /// Session 87: short UTF-8 excerpt of the leaf bytes. `None` for
    /// non-recipe-derived rows.
    pub raw_bytes_excerpt: Option<String>,
}

impl EnvelopeColumns {
    pub(crate) fn from_envelope(env: &Envelope) -> Result<Self> {
        let tags_json = serde_json::to_string(&env.tags)?;
        let subject_time_json = match &env.subjects.time {
            Some(t) => Some(serde_json::to_string(t)?),
            None => None,
        };
        Ok(Self {
            source_id: env.provenance.source_id.clone(),
            source_url: env.provenance.source_url.clone(),
            source_published_at: env.provenance.source_published_at,
            license: env.provenance.license.clone(),
            tags_json,
            subject_time_json,
            observed_at: env.observed_at,
            valid_at: env.valid_at,
            confidence: env.confidence.value(),
            selector_path: env.provenance.selector_path.clone(),
            raw_bytes_excerpt: env.provenance.raw_bytes_excerpt.clone(),
        })
    }
}

/// Write the multi-valued subject dimensions and derivation chain.
/// Call after the main record row has been inserted in the same
/// transaction. `record_type` is the [`situation_room_core::RecordType`]
/// string form — `"observation"`, `"event"`, etc.
pub(crate) fn insert_subjects_and_derivation(
    tx: &Transaction,
    record_id: Uuid,
    record_type: &str,
    env: &Envelope,
) -> Result<()> {
    // Entities
    for e in &env.subjects.entities {
        tx.execute(
            "INSERT INTO record_subjects_entities (record_id, record_type, entity_id)
             VALUES (?, ?, ?)",
            params![record_id, record_type, e.as_str()],
        )
        .map_err(StorageError::DuckDb)?;
    }

    // Places — discriminate on the serde `kind` tag
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
            params![record_id, record_type, kind, value_json],
        )
        .map_err(StorageError::DuckDb)?;
    }

    // Topics
    for t in &env.subjects.topics {
        tx.execute(
            "INSERT INTO record_subjects_topics (record_id, record_type, topic)
             VALUES (?, ?, ?)",
            params![record_id, record_type, t.as_str()],
        )
        .map_err(StorageError::DuckDb)?;
    }

    // Derivation chain
    for d in &env.provenance.derived_from {
        let role = serde_json::to_value(d.role)?
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        tx.execute(
            "INSERT INTO record_derived_from (child_id, child_type, parent_id, parent_type, role)
             VALUES (?, ?, ?, ?, ?)",
            params![record_id, record_type, d.record_id, "unknown", role],
        )
        .map_err(StorageError::DuckDb)?;
    }

    Ok(())
}

/// Raw envelope columns as they come back from a record-table SELECT.
/// Order matches the common prefix: caller's SELECT should list them
/// in this order.
pub(crate) struct EnvelopeRow {
    pub source_id: String,
    pub source_url: Option<String>,
    pub source_published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub license: String,
    pub tags_json: String,
    pub subject_time_json: Option<String>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub valid_at: Option<chrono::DateTime<chrono::Utc>>,
    pub confidence_f: f64,
    /// Session 87: selector_path column. NULL → `None`.
    pub selector_path: Option<String>,
    /// Session 87: raw_bytes_excerpt column. NULL → `None`.
    pub raw_bytes_excerpt: Option<String>,
}

/// Read subjects + derivation for a record and reconstruct the full
/// [`Envelope`] from the raw columns plus the junction-table rows.
pub(crate) fn reconstruct_envelope(
    conn: &Connection,
    record_id: Uuid,
    raw: EnvelopeRow,
) -> Result<Envelope> {
    let tags: Vec<String> = serde_json::from_str(&raw.tags_json)?;
    let time: Option<TimeScope> = match raw.subject_time_json {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    };

    // Entities
    let entities: Vec<EntityId> = {
        let mut stmt = conn
            .prepare(
                "SELECT entity_id FROM record_subjects_entities
                 WHERE record_id = ?",
            )
            .map_err(StorageError::DuckDb)?;
        let rows = stmt
            .query_map(params![record_id], |r| r.get::<_, String>(0))
            .map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(StorageError::DuckDb)?;
            out.push(
                EntityId::new(s)
                    .map_err(|e| StorageError::Other(format!("entity_id round-trip: {e}")))?,
            );
        }
        out
    };

    // Places
    let places: Vec<PlaceRef> = {
        let mut stmt = conn
            .prepare(
                "SELECT place_value FROM record_subjects_places
                 WHERE record_id = ?",
            )
            .map_err(StorageError::DuckDb)?;
        let rows = stmt
            .query_map(params![record_id], |r| r.get::<_, String>(0))
            .map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(StorageError::DuckDb)?;
            out.push(serde_json::from_str(&s)?);
        }
        out
    };

    // Topics
    let topics: Vec<Topic> = {
        let mut stmt = conn
            .prepare(
                "SELECT topic FROM record_subjects_topics
                 WHERE record_id = ?",
            )
            .map_err(StorageError::DuckDb)?;
        let rows = stmt
            .query_map(params![record_id], |r| r.get::<_, String>(0))
            .map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(StorageError::DuckDb)?;
            out.push(
                Topic::new(s)
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
            .query_map(params![record_id], |r| {
                Ok((r.get::<_, Uuid>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        for r in rows {
            let (pid, role_s) = r.map_err(StorageError::DuckDb)?;
            let role: DerivationRole =
                serde_json::from_value(serde_json::Value::String(role_s))
                    .map_err(StorageError::Serde)?;
            out.push(DerivedFrom {
                record_id: pid,
                role,
            });
        }
        out
    };

    Ok(Envelope {
        provenance: Provenance {
            source_id: raw.source_id,
            source_url: raw.source_url,
            source_published_at: raw.source_published_at,
            license: raw.license,
            derived_from,
            selector_path: raw.selector_path,
            raw_bytes_excerpt: raw.raw_bytes_excerpt,
        },
        subjects: Subjects {
            entities,
            places,
            time,
            topics,
        },
        tags,
        valid_at: raw.valid_at,
        observed_at: raw.observed_at,
        confidence: Confidence::clamp(raw.confidence_f as f32),
    })
}
