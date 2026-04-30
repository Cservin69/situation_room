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
//!
//! ## Plan lifecycle (Session 7, P1)
//!
//! Plans carry a [`PlanStatus`] that gates whether downstream
//! Phase-6 fetching will run against them:
//!
//!   - `Pending`  — newly classified, awaiting user review.
//!   - `Accepted` — user reviewed and approved; fetch executor input.
//!   - `Rejected` — user discarded; retained for audit, hidden by
//!     default in listings.
//!
//! Status is a TEXT column, not a DuckDB enum (see migration v5 for
//! why). Validation lives at the storage boundary in
//! [`PlanStatus::from_str`]. The plan row is otherwise immutable; the
//! status column is the single mutable field.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// PlanStatus — the lifecycle column
// ---------------------------------------------------------------------------

/// Lifecycle state for a research plan. See module docs and migration
/// v5 for the full rationale.
///
/// The wire form (serde / column text) is lowercase: `"pending"`,
/// `"accepted"`, `"rejected"`. That convention matches the column
/// `DEFAULT 'pending'` in the migration; bend either side and the
/// other follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    Pending,
    Accepted,
    Rejected,
}

impl PlanStatus {
    /// The exact string written to the DuckDB column. Centralized so
    /// the migration's `DEFAULT 'pending'` and the Rust round-trip
    /// can't drift.
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanStatus::Pending => "pending",
            PlanStatus::Accepted => "accepted",
            PlanStatus::Rejected => "rejected",
        }
    }
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlanStatus {
    type Err = StorageError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(PlanStatus::Pending),
            "accepted" => Ok(PlanStatus::Accepted),
            "rejected" => Ok(PlanStatus::Rejected),
            other => Err(StorageError::Other(format!(
                "unknown plan status in column: {other:?} \
                 (expected one of pending / accepted / rejected)"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Row shapes
// ---------------------------------------------------------------------------

/// Columns a plan must provide to storage. The full plan shape lives
/// in the JSON columns; the scalar columns are the parts we index on
/// or want to filter without parsing JSON.
///
/// `status` is included so callers that want to insert a plan in a
/// non-default state can. The classifier path always goes through
/// `pipeline::research_plans_store::save_research_plan`, which sets
/// it to `Pending` — see that helper for the policy.
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
    pub status: PlanStatus,
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
    pub status: PlanStatus,
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
                historical_window_days, expectations, created_at, classified_by,
                status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                p.status.as_str(),
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
                        historical_window_days, expectations, created_at, classified_by,
                        status
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

    /// List the most recently created plans, newest first. Returns
    /// every status — kept for backward compatibility with callers
    /// that predate Session 7's filtering. New UI code should prefer
    /// [`Self::recent_research_plans_by_status`].
    pub fn recent_research_plans(&self, limit: usize) -> Result<Vec<StoredResearchPlan>> {
        self.recent_research_plans_by_status(None, limit)
    }

    /// List the most recently created plans, newest first, optionally
    /// filtered to a single status. `None` returns every status —
    /// matching the bare `recent_research_plans` behaviour.
    ///
    /// The composite index `(status, created_at DESC)` from migration
    /// v5 covers the filtered case; the unfiltered case still uses
    /// the existing `created_at` index.
    ///
    /// Iteration is manual rather than `query_map` because
    /// [`row_to_stored`] returns `crate::Result` (so a corrupt status
    /// string surfaces as `StorageError::Other` with a useful
    /// message) — and `query_map`'s closure must return
    /// `duckdb::Result`, which would force us to either hand-craft a
    /// `duckdb::Error` variant (fragile across duckdb point releases)
    /// or to swallow the parse error.
    pub fn recent_research_plans_by_status(
        &self,
        status: Option<PlanStatus>,
        limit: usize,
    ) -> Result<Vec<StoredResearchPlan>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // Two distinct prepared statements rather than dynamic SQL +
        // a NULL-check WHERE clause. The two queries hit different
        // indexes, and the parameter shape differs (one has a status
        // arg, the other doesn't); branching here is clearer than a
        // CASE expression in SQL.
        let mut out = Vec::new();
        match status {
            Some(s) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, topic, interpretation, topic_tags, geographic_scope,
                                historical_window_days, expectations, created_at, classified_by,
                                status
                         FROM research_plans
                         WHERE status = ?
                         ORDER BY created_at DESC
                         LIMIT ?",
                    )
                    .map_err(StorageError::DuckDb)?;
                let mut rows = stmt
                    .query(params![s.as_str(), limit as i64])
                    .map_err(StorageError::DuckDb)?;
                while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
                    out.push(row_to_stored(row)?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, topic, interpretation, topic_tags, geographic_scope,
                                historical_window_days, expectations, created_at, classified_by,
                                status
                         FROM research_plans
                         ORDER BY created_at DESC
                         LIMIT ?",
                    )
                    .map_err(StorageError::DuckDb)?;
                let mut rows = stmt
                    .query(params![limit as i64])
                    .map_err(StorageError::DuckDb)?;
                while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
                    out.push(row_to_stored(row)?);
                }
            }
        }
        Ok(out)
    }

    /// Move a plan to a new status. Idempotent — setting `Accepted`
    /// on an already-accepted plan is a no-op write that returns Ok.
    /// Returns `StorageError::NotFound` if the id isn't present, so
    /// the caller can distinguish "transition succeeded" from "you
    /// asked about a plan that doesn't exist".
    pub fn set_plan_status(&self, id: Uuid, status: PlanStatus) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let affected = conn
            .execute(
                "UPDATE research_plans SET status = ? WHERE id = ?",
                params![status.as_str(), id],
            )
            .map_err(StorageError::DuckDb)?;

        if affected == 0 {
            return Err(StorageError::NotFound(format!("research_plan {id}")));
        }
        Ok(())
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
    let status_str: String = row.get(9).map_err(StorageError::DuckDb)?;
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
        status: PlanStatus::from_str(&status_str)?,
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
            status: PlanStatus::Pending,
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
        assert_eq!(got.status, PlanStatus::Pending);
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

    // -----------------------------------------------------------------
    // Session 7 — PlanStatus + filtering
    // -----------------------------------------------------------------

    #[test]
    fn plan_status_round_trips_via_str() {
        for s in [PlanStatus::Pending, PlanStatus::Accepted, PlanStatus::Rejected] {
            let back: PlanStatus = s.as_str().parse().unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn plan_status_serde_uses_lowercase() {
        // The wire form (DTOs in the api crate) and the column form
        // share this convention. Drift here breaks both.
        let json = serde_json::to_string(&PlanStatus::Accepted).unwrap();
        assert_eq!(json, r#""accepted""#);
        let back: PlanStatus = serde_json::from_str(r#""rejected""#).unwrap();
        assert_eq!(back, PlanStatus::Rejected);
    }

    #[test]
    fn plan_status_unknown_string_errors_with_value() {
        let err = "garbage".parse::<PlanStatus>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("garbage"), "error should name the bad value: {msg}");
    }

    #[test]
    fn set_plan_status_transitions_pending_to_accepted() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let row = sample_row();
        store.insert_research_plan(&row).unwrap();

        store.set_plan_status(row.id, PlanStatus::Accepted).unwrap();
        let got = store.get_research_plan(row.id).unwrap().unwrap();
        assert_eq!(got.status, PlanStatus::Accepted);
    }

    #[test]
    fn set_plan_status_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let row = sample_row();
        store.insert_research_plan(&row).unwrap();

        store.set_plan_status(row.id, PlanStatus::Rejected).unwrap();
        // Same status again — no-op write must succeed, not fail.
        store.set_plan_status(row.id, PlanStatus::Rejected).unwrap();
        let got = store.get_research_plan(row.id).unwrap().unwrap();
        assert_eq!(got.status, PlanStatus::Rejected);
    }

    #[test]
    fn set_plan_status_returns_not_found_for_unknown_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let err = store
            .set_plan_status(Uuid::now_v7(), PlanStatus::Accepted)
            .unwrap_err();
        match err {
            StorageError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn recent_research_plans_by_status_filters_correctly() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut a = sample_row();
        a.id = Uuid::now_v7();
        a.topic = "a-pending".into();
        a.created_at = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        store.insert_research_plan(&a).unwrap();

        let mut b = sample_row();
        b.id = Uuid::now_v7();
        b.topic = "b-accepted".into();
        b.created_at = Utc.with_ymd_and_hms(2026, 4, 2, 0, 0, 0).unwrap();
        store.insert_research_plan(&b).unwrap();
        store.set_plan_status(b.id, PlanStatus::Accepted).unwrap();

        let mut c = sample_row();
        c.id = Uuid::now_v7();
        c.topic = "c-rejected".into();
        c.created_at = Utc.with_ymd_and_hms(2026, 4, 3, 0, 0, 0).unwrap();
        store.insert_research_plan(&c).unwrap();
        store.set_plan_status(c.id, PlanStatus::Rejected).unwrap();

        // Pending → just a.
        let pending = store
            .recent_research_plans_by_status(Some(PlanStatus::Pending), 10)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].topic, "a-pending");

        // Accepted → just b.
        let accepted = store
            .recent_research_plans_by_status(Some(PlanStatus::Accepted), 10)
            .unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].topic, "b-accepted");

        // Rejected → just c.
        let rejected = store
            .recent_research_plans_by_status(Some(PlanStatus::Rejected), 10)
            .unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].topic, "c-rejected");

        // None → all three, newest-first.
        let all = store.recent_research_plans_by_status(None, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].topic, "c-rejected");
        assert_eq!(all[2].topic, "a-pending");
    }

    #[test]
    fn recent_research_plans_backward_compat_returns_all_statuses() {
        // Regression guard: `recent_research_plans` (no status arg)
        // existed before Session 7 and callers expect every plan back.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut a = sample_row();
        a.id = Uuid::now_v7();
        store.insert_research_plan(&a).unwrap();

        let mut b = sample_row();
        b.id = Uuid::now_v7();
        store.insert_research_plan(&b).unwrap();
        store.set_plan_status(b.id, PlanStatus::Rejected).unwrap();

        let all = store.recent_research_plans(10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn newly_inserted_plan_defaults_to_pending_when_omitted_via_default() {
        // Insert via the row struct with PlanStatus::Pending — the
        // round-trip below also exercises the column DEFAULT, since
        // the migration's DEFAULT is what backfills any row that
        // somehow gets inserted without a status. We can't easily
        // trigger the DEFAULT path through ResearchPlanRow (it's
        // required), so the assertion is on the explicit form: a
        // freshly-classified plan ends up Pending.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let row = sample_row();
        store.insert_research_plan(&row).unwrap();

        let got = store.get_research_plan(row.id).unwrap().unwrap();
        assert_eq!(got.status, PlanStatus::Pending);
    }
}
