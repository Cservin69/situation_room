//! Research plan storage.
//!
//! Plans are the Level-1 output of the research function (ADR 0007)
//! and are stored alongside records but not *as* records. The typed
//! `ResearchPlan` lives in `stockpile-pipeline`; storage accepts the
//! plan as pre-serialized `serde_json::Value` columns plus the scalar
//! fields we index on. Keeping the typed plan out of storage means
//! storage stays the record-persistence layer and doesn't acquire a
//! reverse dependency on pipeline.
//!
//! A typed helper sits in `stockpile_pipeline::research_plans_store`
//! that does the serialization and calls these functions. Callers
//! should prefer that helper over invoking these methods directly.

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Columns a plan must provide to storage. The full plan shape lives
/// in the JSON columns; the scalar columns are the parts we index on
/// or want to filter without parsing JSON.
#[derive(Debug, Clone)]
pub struct ResearchPlanRow {
    pub id: Uuid,
    pub topic: String,
    pub interpretation: String,
    /// JSON array of topic strings (each one validates as `Topic`).
    pub topic_tags_json: String,
    /// JSON array of geographic scope strings (ISO codes or regions).
    pub geographic_scope_json: String,
    pub historical_window_days: u32,
    /// JSON-encoded `RecordExpectations`.
    pub expectations_json: String,
    pub created_at: DateTime<Utc>,
    pub classified_by: String,
}

/// A plan row as it comes back out of storage. Same shape as
/// [`ResearchPlanRow`]; the typed helper reassembles `ResearchPlan`.
#[derive(Debug, Clone)]
pub struct StoredResearchPlan {
    pub id: Uuid,
    pub topic: String,
    pub interpretation: String,
    pub topic_tags_json: String,
    pub geographic_scope_json: String,
    pub historical_window_days: u32,
    pub expectations_json: String,
    pub created_at: DateTime<Utc>,
    pub classified_by: String,
}

impl Store {
    /// Insert a plan. Errors on a PRIMARY KEY conflict — plans are
    /// immutable once written; the LLM running classification again
    /// produces a fresh `id` (UUIDv7), not a conflict.
    pub fn insert_research_plan(&self, p: &ResearchPlanRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO research_plans (
                id, topic, interpretation, topic_tags, geographic_scope,
                historical_window_days, expectations, created_at, classified_by
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                p.id,
                p.topic,
                p.interpretation,
                p.topic_tags_json,
                p.geographic_scope_json,
                p.historical_window_days as i64,
                p.expectations_json,
                p.created_at,
                p.classified_by,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Fetch a plan by id. Returns `Ok(None)` if not present.
    pub fn get_research_plan(&self, id: Uuid) -> Result<Option<StoredResearchPlan>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, topic, interpretation, topic_tags, geographic_scope,
                        historical_window_days, expectations, created_at, classified_by
                 FROM research_plans WHERE id = ?",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt.query(params![id]).map_err(StorageError::DuckDb)?;
        if let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            Ok(Some(row_to_stored(row)?))
        } else {
            Ok(None)
        }
    }

    /// List the most recently created plans, newest first. Useful for
    /// "show me my recent research" UI listings.
    pub fn recent_research_plans(&self, limit: usize) -> Result<Vec<StoredResearchPlan>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, topic, interpretation, topic_tags, geographic_scope,
                        historical_window_days, expectations, created_at, classified_by
                 FROM research_plans
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map(params![limit as i64], row_to_stored_owned)
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(StorageError::DuckDb)?);
        }
        Ok(out)
    }

    /// Count plans. Small helper for smoke tests and UI badges.
    pub fn count_research_plans(&self) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM research_plans", [], |r| r.get(0))
            .map_err(StorageError::DuckDb)?;
        Ok(count as u64)
    }
}

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredResearchPlan> {
    Ok(StoredResearchPlan {
        id: row.get(0).map_err(StorageError::DuckDb)?,
        topic: row.get(1).map_err(StorageError::DuckDb)?,
        interpretation: row.get(2).map_err(StorageError::DuckDb)?,
        topic_tags_json: row.get(3).map_err(StorageError::DuckDb)?,
        geographic_scope_json: row.get(4).map_err(StorageError::DuckDb)?,
        historical_window_days: {
            let v: i64 = row.get(5).map_err(StorageError::DuckDb)?;
            v as u32
        },
        expectations_json: row.get(6).map_err(StorageError::DuckDb)?,
        created_at: row.get(7).map_err(StorageError::DuckDb)?,
        classified_by: row.get(8).map_err(StorageError::DuckDb)?,
    })
}

/// Variant for `query_map` which expects a closure returning
/// `duckdb::Result<T>` rather than `crate::Result<T>`. Wraps the
/// scalar reads in DuckDb's own error so the iterator's error type
/// stays consistent.
fn row_to_stored_owned(row: &duckdb::Row<'_>) -> duckdb::Result<StoredResearchPlan> {
    Ok(StoredResearchPlan {
        id: row.get(0)?,
        topic: row.get(1)?,
        interpretation: row.get(2)?,
        topic_tags_json: row.get(3)?,
        geographic_scope_json: row.get(4)?,
        historical_window_days: {
            let v: i64 = row.get(5)?;
            v as u32
        },
        expectations_json: row.get(6)?,
        created_at: row.get(7)?,
        classified_by: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_row() -> ResearchPlanRow {
        ResearchPlanRow {
            id: Uuid::now_v7(),
            topic: "lithium supply chain".into(),
            interpretation: "Lithium production, refining, trade flows.".into(),
            topic_tags_json: r#"["lithium","battery_supply_chain"]"#.into(),
            geographic_scope_json: r#"["AU","CL","CN"]"#.into(),
            historical_window_days: 730,
            expectations_json: r#"{"observation_metrics":[],"event_types":[],"entity_kinds":[],"relation_kinds":[],"document_sources":[],"assertion_guidance":null}"#.into(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 27, 12, 0, 0).unwrap(),
            classified_by: "xai".into(),
        }
    }

    #[test]
    fn insert_and_get_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let row = sample_row();
        store.insert_research_plan(&row).unwrap();

        let got = store.get_research_plan(row.id).unwrap().unwrap();
        assert_eq!(got.id, row.id);
        assert_eq!(got.topic, row.topic);
        assert_eq!(got.interpretation, row.interpretation);
        assert_eq!(got.topic_tags_json, row.topic_tags_json);
        assert_eq!(got.geographic_scope_json, row.geographic_scope_json);
        assert_eq!(got.historical_window_days, row.historical_window_days);
        assert_eq!(got.expectations_json, row.expectations_json);
        assert_eq!(got.classified_by, row.classified_by);
    }

    #[test]
    fn get_returns_none_when_missing() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let got = store.get_research_plan(Uuid::now_v7()).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn recent_research_plans_orders_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut older = sample_row();
        older.id = Uuid::now_v7();
        older.created_at = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        older.topic = "older".into();

        let mut newer = sample_row();
        newer.id = Uuid::now_v7();
        newer.created_at = Utc.with_ymd_and_hms(2026, 4, 27, 0, 0, 0).unwrap();
        newer.topic = "newer".into();

        store.insert_research_plan(&older).unwrap();
        store.insert_research_plan(&newer).unwrap();

        let recent = store.recent_research_plans(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].topic, "newer");
        assert_eq!(recent[1].topic, "older");
    }

    #[test]
    fn count_research_plans_increments() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        assert_eq!(store.count_research_plans().unwrap(), 0);
        let mut row = sample_row();
        store.insert_research_plan(&row).unwrap();
        assert_eq!(store.count_research_plans().unwrap(), 1);

        row.id = Uuid::now_v7();
        store.insert_research_plan(&row).unwrap();
        assert_eq!(store.count_research_plans().unwrap(), 2);
    }
}
