//! Queries that span record types.
//!
//! Two cross-record queries live here:
//!
//! - [`Store::topics_in_use`] — the Level-1 classifier injection
//!   (ADR 0007). The classifier is shown the topic strings already in
//!   use across past sessions so related queries converge on shared
//!   topic strings.
//! - [`Store::records_for_plan`] (Session 22) — given a plan, return
//!   every record produced by any of that plan's recipes, bucketed by
//!   record type. Drives the records-on-the-workstation rendering.
//!
//! Both queries scan junction or substring data and return owned
//! typed values rather than streaming — the volumes are bounded
//! (topics in the low hundreds; records per plan in the low
//! thousands) and the API surface is simpler.

use duckdb::{params, params_from_iter};
use situation_room_core::{
    AssertedContent, Assertion, Document, Entity, EntityId, Event, EventContent, Observation,
    ObservationContent, Relation, RelationContent, Stance, Topic,
};
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeRow};
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// topics_in_use — Level-1 classifier injection (Session 4)
// ---------------------------------------------------------------------------

/// One row from `topics_in_use`: a topic string and its usage count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicUsage {
    pub topic: Topic,
    pub count: u64,
}

impl Store {
    /// Return the top-N topic strings in use, sorted by usage count
    /// descending. Counts are the number of record-topic junction
    /// rows — a record tagged with two topics contributes one to
    /// each.
    ///
    /// Passed to the Level-1 classifier as hygiene context: "here are
    /// the topics this system has used before; prefer them when a
    /// new query is plausibly about the same subject." See ADR 0007.
    pub fn topics_in_use(&self, limit: usize) -> Result<Vec<TopicUsage>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT topic, COUNT(*) AS n
                 FROM record_subjects_topics
                 GROUP BY topic
                 ORDER BY n DESC, topic ASC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::new();
        for r in rows {
            let (topic_s, count) = r.map_err(StorageError::DuckDb)?;
            let topic = Topic::new(topic_s)
                .map_err(|e| StorageError::Other(format!("topic round-trip: {e}")))?;
            out.push(TopicUsage {
                topic,
                count: count as u64,
            });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// records_for_plan — Session 22, the records-rendering join
// ---------------------------------------------------------------------------

/// A plan's records, bucketed by the six record types.
///
/// Returned by [`Store::records_for_plan`]. Empty `Vec`s for buckets
/// the plan's recipes didn't populate; never an error for an empty
/// bucket.
#[derive(Debug, Clone, Default)]
pub struct RecordsByPlan {
    pub observations: Vec<Observation>,
    pub events: Vec<Event>,
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    pub documents: Vec<Document>,
    pub assertions: Vec<Assertion>,
}

impl Store {
    /// List every record produced by any recipe attached to a plan.
    ///
    /// ## Architectural note (read this before extending)
    ///
    /// Records are *not* keyed to a plan in the schema. They're keyed
    /// to topics (via `envelope.subjects.topics`) and to provenance
    /// (via `envelope.provenance.source_id`, formatted as
    /// `"{src}#recipe:{recipe_uuid}@v{version}"` by
    /// `pipeline::recipe_apply::build_record`). A record produced by
    /// recipe A for plan X *could* satisfy expectations from a
    /// different plan Y if Y's expectations match the same topic. The
    /// plan-scoped query is a UI convenience — "what did this plan's
    /// fetch runs produce" — not a schema constraint.
    ///
    /// ## How the join works
    ///
    /// 1. List recipes for the plan (one indexed query against the
    ///    `(plan_id, source_id)` index from migration v3).
    /// 2. For each of the six record-type tables, filter rows whose
    ///    `source_id` substring contains any of those recipe ids.
    ///    The provenance format is fixed, so a substring `LIKE` is
    ///    unambiguous: `'%#recipe:<uuid>@v%'`.
    ///
    /// Empty plan / plan with no recipes / recipes with no records:
    /// all return an empty [`RecordsByPlan`], never an error. This
    /// is deliberate — the UI distinguishes "no expectations" from
    /// "no records yet" by other means (the plan's expectations
    /// themselves), so the query just answers "what records exist".
    ///
    /// ## Performance
    ///
    /// Six SELECTs (one per table), each with `N` OR'd `LIKE` clauses
    /// where `N` is the number of recipes for the plan. `N` is
    /// typically ≤10 in practice. Without an index on `source_id`
    /// (DuckDB doesn't index TEXT-substring matches by default), each
    /// query is a full scan of the per-type table. For the volumes
    /// this product targets (low thousands per type), full scans are
    /// fine. If volumes grow into the hundreds of thousands, a
    /// dedicated `record_recipe_id` column populated at insert time
    /// would replace the substring match — that's an additive
    /// migration when the time comes.
    pub fn records_for_plan(&self, plan_id: Uuid) -> Result<RecordsByPlan> {
        // Step 1: collect recipe ids. Reuse the existing indexed query.
        let recipes = self.recipes_for_plan(plan_id)?;
        if recipes.is_empty() {
            return Ok(RecordsByPlan::default());
        }

        // Build the LIKE patterns once — same vector reused across
        // all six per-table queries via params_from_iter.
        //
        // Pattern: `%#recipe:<uuid>@v%`. The leading `%` matches the
        // source-name prefix (`gdelt#`, `usgs_mcs#`, etc.); the
        // trailing `%` matches the version digits.
        let patterns: Vec<String> = recipes
            .iter()
            .map(|r| format!("%#recipe:{}@v%", r.id))
            .collect();
        let where_clause = build_or_likes("source_id", patterns.len());

        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // Per-table queries. Each returns the per-table SELECT plus the
        // envelope columns; reconstruct_envelope handles the
        // junction-table reads.

        let observations = list_observations(&conn, &where_clause, &patterns)?;
        let events = list_events(&conn, &where_clause, &patterns)?;
        let entities = list_entities(&conn, &where_clause, &patterns)?;
        let relations = list_relations(&conn, &where_clause, &patterns)?;
        let documents = list_documents(&conn, &where_clause, &patterns)?;
        let assertions = list_assertions(&conn, &where_clause, &patterns)?;

        Ok(RecordsByPlan {
            observations,
            events,
            entities,
            relations,
            documents,
            assertions,
        })
    }
}

/// Build a `(col LIKE ? OR col LIKE ? OR …)` clause for `n` patterns.
/// Empty `n` would produce `()` — invalid SQL — so callers must
/// guard against the zero case (we do, via the early return in
/// `records_for_plan`).
fn build_or_likes(col: &str, n: usize) -> String {
    let parts: Vec<String> = (0..n).map(|_| format!("{col} LIKE ?")).collect();
    format!("({})", parts.join(" OR "))
}

// ---------------------------------------------------------------------------
// Per-table list helpers
// ---------------------------------------------------------------------------
//
// Each helper mirrors the corresponding `get_*` function's column
// list and row-decoding pattern. They're verbose by design: the
// per-record-type schemas differ enough that a generic helper would
// hurt readability. The shape is uniform enough for a review pass:
// SELECT, query, decode, reconstruct envelope, push.

fn list_observations(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Observation>> {
    let sql = format!(
        "SELECT id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
         FROM observations
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let dedup_key: Option<String> = row.get(1).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(2).map_err(StorageError::DuckDb)?,
            source_url: row.get(3).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(4).map_err(StorageError::DuckDb)?,
            license: row.get(5).map_err(StorageError::DuckDb)?,
            tags_json: row.get(6).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(7).map_err(StorageError::DuckDb)?,
            observed_at: row.get(8).map_err(StorageError::DuckDb)?,
            valid_at: row.get(9).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(10).map_err(StorageError::DuckDb)?,
        };
        let content_json: String = row.get(11).map_err(StorageError::DuckDb)?;
        let content: ObservationContent = serde_json::from_str(&content_json)?;
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        out.push(Observation {
            id: row_id,
            dedup_key,
            envelope,
            content,
        });
    }
    Ok(out)
}

fn list_events(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Event>> {
    let sql = format!(
        "SELECT id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
         FROM events
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let dedup_key: Option<String> = row.get(1).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(2).map_err(StorageError::DuckDb)?,
            source_url: row.get(3).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(4).map_err(StorageError::DuckDb)?,
            license: row.get(5).map_err(StorageError::DuckDb)?,
            tags_json: row.get(6).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(7).map_err(StorageError::DuckDb)?,
            observed_at: row.get(8).map_err(StorageError::DuckDb)?,
            valid_at: row.get(9).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(10).map_err(StorageError::DuckDb)?,
        };
        let content_json: String = row.get(11).map_err(StorageError::DuckDb)?;
        let content: EventContent = serde_json::from_str(&content_json)?;
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        out.push(Event {
            id: row_id,
            dedup_key,
            envelope,
            content,
        });
    }
    Ok(out)
}

fn list_entities(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Entity>> {
    let sql = format!(
        "SELECT id, entity_id, kind, canonical_name, geometry,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence
         FROM entities
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let entity_id_s: String = row.get(1).map_err(StorageError::DuckDb)?;
        let kind: String = row.get(2).map_err(StorageError::DuckDb)?;
        let canonical_name: String = row.get(3).map_err(StorageError::DuckDb)?;
        let geometry_json: Option<String> = row.get(4).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(5).map_err(StorageError::DuckDb)?,
            source_url: row.get(6).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(7).map_err(StorageError::DuckDb)?,
            license: row.get(8).map_err(StorageError::DuckDb)?,
            tags_json: row.get(9).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(10).map_err(StorageError::DuckDb)?,
            observed_at: row.get(11).map_err(StorageError::DuckDb)?,
            valid_at: row.get(12).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(13).map_err(StorageError::DuckDb)?,
        };
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        let entity_id = EntityId::new(entity_id_s)
            .map_err(|e| StorageError::Other(format!("entity_id round-trip: {e}")))?;
        let geometry = match geometry_json {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        };
        out.push(Entity {
            id: row_id,
            entity_id,
            kind,
            canonical_name,
            geometry,
            envelope,
        });
    }
    Ok(out)
}

fn list_relations(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Relation>> {
    let sql = format!(
        "SELECT id, dedup_key, source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                content
         FROM relations
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let dedup_key: Option<String> = row.get(1).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(2).map_err(StorageError::DuckDb)?,
            source_url: row.get(3).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(4).map_err(StorageError::DuckDb)?,
            license: row.get(5).map_err(StorageError::DuckDb)?,
            tags_json: row.get(6).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(7).map_err(StorageError::DuckDb)?,
            observed_at: row.get(8).map_err(StorageError::DuckDb)?,
            valid_at: row.get(9).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(10).map_err(StorageError::DuckDb)?,
        };
        let content_json: String = row.get(11).map_err(StorageError::DuckDb)?;
        let content: RelationContent = serde_json::from_str(&content_json)?;
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        out.push(Relation {
            id: row_id,
            dedup_key,
            envelope,
            content,
        });
    }
    Ok(out)
}

fn list_documents(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Document>> {
    let sql = format!(
        "SELECT id, dedup_key, title, doc_kind, mime, body, published_at, author,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence
         FROM documents
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let dedup_key: Option<String> = row.get(1).map_err(StorageError::DuckDb)?;
        let title: Option<String> = row.get(2).map_err(StorageError::DuckDb)?;
        let kind: String = row.get(3).map_err(StorageError::DuckDb)?;
        let mime: String = row.get(4).map_err(StorageError::DuckDb)?;
        let body: String = row.get(5).map_err(StorageError::DuckDb)?;
        let published_at: Option<chrono::DateTime<chrono::Utc>> =
            row.get(6).map_err(StorageError::DuckDb)?;
        let author: Option<String> = row.get(7).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(8).map_err(StorageError::DuckDb)?,
            source_url: row.get(9).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(10).map_err(StorageError::DuckDb)?,
            license: row.get(11).map_err(StorageError::DuckDb)?,
            tags_json: row.get(12).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(13).map_err(StorageError::DuckDb)?,
            observed_at: row.get(14).map_err(StorageError::DuckDb)?,
            valid_at: row.get(15).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(16).map_err(StorageError::DuckDb)?,
        };
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        out.push(Document {
            id: row_id,
            dedup_key,
            title,
            kind,
            mime,
            body,
            published_at,
            author,
            envelope,
        });
    }
    Ok(out)
}

fn list_assertions(
    conn: &duckdb::Connection,
    where_clause: &str,
    patterns: &[String],
) -> Result<Vec<Assertion>> {
    let sql = format!(
        "SELECT id, dedup_key, claimant, stance, content_kind, content,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence
         FROM assertions
         WHERE {where_clause}
         ORDER BY observed_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
    let mut rows = stmt
        .query(params_from_iter(patterns.iter()))
        .map_err(StorageError::DuckDb)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
        let row_id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
        let dedup_key: Option<String> = row.get(1).map_err(StorageError::DuckDb)?;
        let claimant_s: String = row.get(2).map_err(StorageError::DuckDb)?;
        let stance_s: String = row.get(3).map_err(StorageError::DuckDb)?;
        let _content_kind: String = row.get(4).map_err(StorageError::DuckDb)?;
        let content_json: String = row.get(5).map_err(StorageError::DuckDb)?;
        let raw = EnvelopeRow {
            source_id: row.get(6).map_err(StorageError::DuckDb)?,
            source_url: row.get(7).map_err(StorageError::DuckDb)?,
            source_published_at: row.get(8).map_err(StorageError::DuckDb)?,
            license: row.get(9).map_err(StorageError::DuckDb)?,
            tags_json: row.get(10).map_err(StorageError::DuckDb)?,
            subject_time_json: row.get(11).map_err(StorageError::DuckDb)?,
            observed_at: row.get(12).map_err(StorageError::DuckDb)?,
            valid_at: row.get(13).map_err(StorageError::DuckDb)?,
            confidence_f: row.get(14).map_err(StorageError::DuckDb)?,
        };
        let envelope = reconstruct_envelope(conn, row_id, raw)?;
        let claimant = EntityId::new(claimant_s)
            .map_err(|e| StorageError::Other(format!("claimant round-trip: {e}")))?;
        let stance: Stance = serde_json::from_value(serde_json::Value::String(stance_s))
            .map_err(StorageError::Serde)?;
        let content: AssertedContent = serde_json::from_str(&content_json)?;
        out.push(Assertion {
            id: row_id,
            dedup_key,
            claimant,
            stance,
            content,
            envelope,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Store;
    use crate::recipes::{AuthoredFrom, RecipeRow};
    use chrono::Utc;
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, Topic, Unit};
    use situation_room_core::Observation;

    // -----------------------------------------------------------------
    // topics_in_use tests (carry-forward from Session 4)
    // -----------------------------------------------------------------

    fn obs_with_topics(topics: Vec<Topic>) -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "test".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics,
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Observation::new(
            envelope,
            ObservationContent {
                metric: "m".into(),
                value: 1.0,
                unit: Unit::new("1").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Instant,
                geometry: None,
            },
        )
    }

    #[test]
    fn topics_in_use_empty_store_returns_empty() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let topics = store.topics_in_use(100).unwrap();
        assert!(topics.is_empty());
    }

    #[test]
    fn topics_in_use_ranks_by_frequency() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let li = Topic::new("Li").unwrap();
        let semi = Topic::new("semiconductors").unwrap();
        let cu = Topic::new("Cu").unwrap();

        for _ in 0..3 {
            store
                .insert_observation(&obs_with_topics(vec![li.clone()]))
                .unwrap();
        }
        for _ in 0..2 {
            store
                .insert_observation(&obs_with_topics(vec![semi.clone()]))
                .unwrap();
        }
        store
            .insert_observation(&obs_with_topics(vec![cu.clone()]))
            .unwrap();

        let topics = store.topics_in_use(10).unwrap();
        assert_eq!(topics.len(), 3);
        assert_eq!(topics[0].topic, li);
        assert_eq!(topics[0].count, 3);
        assert_eq!(topics[1].topic, semi);
        assert_eq!(topics[1].count, 2);
        assert_eq!(topics[2].topic, cu);
        assert_eq!(topics[2].count, 1);
    }

    #[test]
    fn topics_in_use_respects_limit() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        for t in ["Li", "Cu", "Ni"] {
            store
                .insert_observation(&obs_with_topics(vec![Topic::new(t).unwrap()]))
                .unwrap();
        }

        let topics = store.topics_in_use(2).unwrap();
        assert_eq!(topics.len(), 2);
    }

    #[test]
    fn topics_in_use_counts_multiple_topics_per_record() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let li = Topic::new("Li").unwrap();
        let semi = Topic::new("semiconductors").unwrap();
        store
            .insert_observation(&obs_with_topics(vec![li.clone(), semi.clone()]))
            .unwrap();

        let topics = store.topics_in_use(10).unwrap();
        assert_eq!(topics.len(), 2);
        assert_eq!(topics[0].count, 1);
        assert_eq!(topics[1].count, 1);
    }

    // -----------------------------------------------------------------
    // records_for_plan tests (Session 22)
    // -----------------------------------------------------------------

    /// Insert a recipe row tied to `plan_id` and return its id, so
    /// downstream tests can stamp records with a provenance string
    /// that points at it.
    fn insert_test_recipe(store: &Store, plan_id: Uuid, source_id: &str) -> Uuid {
        let id = Uuid::now_v7();
        let row = RecipeRow {
            id,
            dedup_key: Some(format!("{source_id}-{plan_id}")),
            plan_id,
            source_id: source_id.into(),
            source_url: format!("https://example.com/{source_id}"),
            extraction_json: r#"{"mode":"json_path","path":"$"}"#.into(),
            produces_json: "[]".into(),
            authored_at: Utc::now(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            authored_from: AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: this fixture predates iteration; scalar
            // recipe semantics are what its callers expect.
            iterator: None,
        };
        store.insert_recipe(&row).unwrap();
        id
    }

    /// Build an observation whose provenance points at `recipe_id`
    /// in the format `recipe_apply::build_record` produces.
    fn obs_for_recipe(source: &str, recipe_id: Uuid, version: u32) -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!("{source}#recipe:{recipe_id}@v{version}"),
                source_url: Some(format!("https://example.com/{source}")),
                source_published_at: None,
                license: "unknown".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("test_topic").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Observation::new(
            envelope,
            ObservationContent {
                metric: "production".into(),
                value: 100.0,
                unit: Unit::new("t").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Annual,
                geometry: None,
            },
        )
    }

    #[test]
    fn records_for_plan_empty_when_no_recipes() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let result = store.records_for_plan(Uuid::now_v7()).unwrap();
        assert!(result.observations.is_empty());
        assert!(result.events.is_empty());
        assert!(result.entities.is_empty());
        assert!(result.relations.is_empty());
        assert!(result.documents.is_empty());
        assert!(result.assertions.is_empty());
    }

    #[test]
    fn records_for_plan_returns_observation_via_recipe_join() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let recipe_id = insert_test_recipe(&store, plan_id, "gdelt");
        let obs = obs_for_recipe("gdelt", recipe_id, 1);
        store.insert_observation(&obs).unwrap();

        let result = store.records_for_plan(plan_id).unwrap();
        assert_eq!(result.observations.len(), 1);
        assert_eq!(result.observations[0].id, obs.id);
        assert_eq!(result.observations[0].content.metric, "production");
        // Envelope is reconstructed including subjects from the
        // junction tables, not just the flat columns.
        assert_eq!(result.observations[0].envelope.subjects.topics.len(), 1);
        assert_eq!(
            result.observations[0].envelope.subjects.topics[0].as_str(),
            "test_topic"
        );
    }

    #[test]
    fn records_for_plan_isolates_records_to_owning_plan() {
        // Two plans, one recipe each, one observation each. The query
        // for plan A must return only A's record, not B's, even though
        // both records sit in the same observations table.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();
        let recipe_a = insert_test_recipe(&store, plan_a, "gdelt");
        let recipe_b = insert_test_recipe(&store, plan_b, "rss_feeds");

        let obs_a = obs_for_recipe("gdelt", recipe_a, 1);
        let obs_b = obs_for_recipe("rss_feeds", recipe_b, 1);
        store.insert_observation(&obs_a).unwrap();
        store.insert_observation(&obs_b).unwrap();

        let result_a = store.records_for_plan(plan_a).unwrap();
        assert_eq!(result_a.observations.len(), 1);
        assert_eq!(result_a.observations[0].id, obs_a.id);

        let result_b = store.records_for_plan(plan_b).unwrap();
        assert_eq!(result_b.observations.len(), 1);
        assert_eq!(result_b.observations[0].id, obs_b.id);
    }

    #[test]
    fn records_for_plan_aggregates_across_multiple_recipes() {
        // Plan with two bound recipes; each recipe produces one
        // observation. Listing the plan should surface both.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let recipe_1 = insert_test_recipe(&store, plan_id, "gdelt");
        let recipe_2 = insert_test_recipe(&store, plan_id, "rss_feeds");

        let obs_1 = obs_for_recipe("gdelt", recipe_1, 1);
        let obs_2 = obs_for_recipe("rss_feeds", recipe_2, 1);
        store.insert_observation(&obs_1).unwrap();
        store.insert_observation(&obs_2).unwrap();

        let result = store.records_for_plan(plan_id).unwrap();
        assert_eq!(result.observations.len(), 2);
    }

    #[test]
    fn records_for_plan_skips_records_with_legacy_provenance() {
        // Records whose source_id pre-dates the recipe-stamping format
        // (no `#recipe:` substring) should not surface in the listing
        // even if they share a topic with the plan. The plan-scoped
        // query is recipe-routed; topic-routed queries are a
        // different surface.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let _recipe_id = insert_test_recipe(&store, plan_id, "gdelt");

        // Insert an observation with a plain (no recipe-substring)
        // source_id. This is the legacy-shape case from before
        // recipe_apply existed.
        let legacy = obs_with_topics(vec![Topic::new("test_topic").unwrap()]);
        store.insert_observation(&legacy).unwrap();

        let result = store.records_for_plan(plan_id).unwrap();
        assert!(result.observations.is_empty());
    }

    #[test]
    fn records_for_plan_carries_provenance_string_through_intact() {
        // The frontend's "click record → highlight matching recipe"
        // affordance reads the recipe id back out of the provenance
        // string. Guard the round-trip.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let recipe_id = insert_test_recipe(&store, plan_id, "gdelt");
        let obs = obs_for_recipe("gdelt", recipe_id, 7);
        store.insert_observation(&obs).unwrap();

        let result = store.records_for_plan(plan_id).unwrap();
        assert_eq!(result.observations.len(), 1);
        assert_eq!(
            result.observations[0].envelope.provenance.source_id,
            format!("gdelt#recipe:{recipe_id}@v7")
        );
    }
}
