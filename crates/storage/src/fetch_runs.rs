//! Fetch-run storage.
//!
//! A fetch run is one invocation of the Phase-6 fetch executor against
//! an accepted plan. See SESSION 8 handoff and migration v6.
//!
//! ## Lifecycle
//!
//! The executor opens a run row at the start of work
//! ([`Store::insert_fetch_run`]) with `finished_at = None` and the
//! counters at zero, then updates it on completion
//! ([`Store::update_fetch_run`]) with the final counters and any
//! top-level error summary. The persisted row is a coarse audit
//! summary — the per-recipe outcome detail rides on the
//! `FetchReportDto` returned synchronously to the caller.
//!
//! Per-recipe persistence is intentionally **not** in scope for
//! Session 8: the failure-mode taxonomy isn't well enough understood
//! yet, and the synchronous report covers the UI's needs. When
//! per-recipe history matters (re-authoring on failure, longer-term
//! freshness tracking), a `recipe_outcomes` child table earns its
//! weight then.
//!
//! ## Why a `Vec<…>` and not a streaming iterator
//!
//! Recent-fetch listings are bounded by `limit`; the storage method
//! returns a fully-materialised `Vec` for the same reason
//! [`Store::recent_research_plans_by_status`] does — manual iteration
//! lets the row-mapper return `crate::Result` (so a malformed row
//! surfaces as a typed `StorageError::Other`, not a stringly-typed
//! `duckdb::Error`).

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Columns a fetch run carries when written. The same shape comes
/// back out via [`StoredFetchRun`].
#[derive(Debug, Clone)]
pub struct FetchRunRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub started_at: DateTime<Utc>,
    /// `None` while in flight; populated on completion.
    pub finished_at: Option<DateTime<Utc>>,
    pub recipes_attempted: u32,
    pub recipes_succeeded: u32,
    pub records_produced: u32,
    pub error_summary: Option<String>,
}

/// A fetch-run row as it comes back out of storage. Same shape as
/// [`FetchRunRow`].
#[derive(Debug, Clone)]
pub struct StoredFetchRun {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub recipes_attempted: u32,
    pub recipes_succeeded: u32,
    pub records_produced: u32,
    pub error_summary: Option<String>,
}

impl Store {
    /// Open a new fetch run. Conventionally called by the executor at
    /// the start of work, with `finished_at = None` and the counters
    /// at zero. Errors on a primary-key conflict — the caller
    /// generates a fresh UUIDv7 per run.
    pub fn insert_fetch_run(&self, r: &FetchRunRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO fetch_runs (
                id, plan_id, started_at, finished_at,
                recipes_attempted, recipes_succeeded, records_produced,
                error_summary
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.id,
                r.plan_id,
                r.started_at,
                r.finished_at,
                r.recipes_attempted as i64,
                r.recipes_succeeded as i64,
                r.records_produced as i64,
                r.error_summary,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Close a fetch run with final counters. Idempotent in the sense
    /// that calling it twice with the same final values is a no-op
    /// write that succeeds; it is *not* a partial-update helper —
    /// every column gets overwritten, so callers that only have a
    /// counter delta in hand should compute the new totals before
    /// calling.
    ///
    /// Returns [`StorageError::NotFound`] if `id` doesn't exist so
    /// the caller can distinguish "you asked about a run that never
    /// opened" from "the close write succeeded".
    pub fn update_fetch_run(&self, r: &FetchRunRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let affected = conn
            .execute(
                "UPDATE fetch_runs
                 SET finished_at        = ?,
                     recipes_attempted  = ?,
                     recipes_succeeded  = ?,
                     records_produced   = ?,
                     error_summary      = ?
                 WHERE id = ?",
                params![
                    r.finished_at,
                    r.recipes_attempted as i64,
                    r.recipes_succeeded as i64,
                    r.records_produced as i64,
                    r.error_summary,
                    r.id,
                ],
            )
            .map_err(StorageError::DuckDb)?;

        if affected == 0 {
            return Err(StorageError::NotFound(format!("fetch_run {}", r.id)));
        }
        Ok(())
    }

    /// List the most recent fetch runs for a plan, newest first.
    /// `limit` is taken at face value — the api crate is responsible
    /// for clamping to a sane range before passing it down.
    pub fn recent_fetch_runs_for_plan(
        &self,
        plan_id: Uuid,
        limit: usize,
    ) -> Result<Vec<StoredFetchRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, plan_id, started_at, finished_at,
                        recipes_attempted, recipes_succeeded, records_produced,
                        error_summary
                 FROM fetch_runs
                 WHERE plan_id = ?
                 ORDER BY started_at DESC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![plan_id, limit as i64])
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            out.push(row_to_stored(row)?);
        }
        Ok(out)
    }
}

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredFetchRun> {
    Ok(StoredFetchRun {
        id: row.get(0).map_err(StorageError::DuckDb)?,
        plan_id: row.get(1).map_err(StorageError::DuckDb)?,
        started_at: row.get(2).map_err(StorageError::DuckDb)?,
        finished_at: row.get(3).map_err(StorageError::DuckDb)?,
        recipes_attempted: {
            let v: i64 = row.get(4).map_err(StorageError::DuckDb)?;
            v as u32
        },
        recipes_succeeded: {
            let v: i64 = row.get(5).map_err(StorageError::DuckDb)?;
            v as u32
        },
        records_produced: {
            let v: i64 = row.get(6).map_err(StorageError::DuckDb)?;
            v as u32
        },
        error_summary: row.get(7).map_err(StorageError::DuckDb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_open(plan_id: Uuid) -> FetchRunRow {
        FetchRunRow {
            id: Uuid::now_v7(),
            plan_id,
            started_at: Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 0).unwrap(),
            finished_at: None,
            recipes_attempted: 0,
            recipes_succeeded: 0,
            records_produced: 0,
            error_summary: None,
        }
    }

    #[test]
    fn fetch_run_round_trips_open_then_close() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let mut run = sample_open(plan_id);
        store.insert_fetch_run(&run).unwrap();

        run.finished_at = Some(Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 5).unwrap());
        run.recipes_attempted = 3;
        run.recipes_succeeded = 2;
        run.records_produced = 4;
        store.update_fetch_run(&run).unwrap();

        let recent = store.recent_fetch_runs_for_plan(plan_id, 10).unwrap();
        assert_eq!(recent.len(), 1);
        let got = &recent[0];
        assert_eq!(got.id, run.id);
        assert_eq!(got.plan_id, plan_id);
        assert_eq!(got.recipes_attempted, 3);
        assert_eq!(got.recipes_succeeded, 2);
        assert_eq!(got.records_produced, 4);
        assert!(got.finished_at.is_some());
        assert!(got.error_summary.is_none());
    }

    #[test]
    fn update_fetch_run_returns_not_found_for_unknown_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut run = sample_open(Uuid::now_v7());
        run.finished_at = Some(Utc::now());
        let err = store.update_fetch_run(&run).unwrap_err();
        match err {
            StorageError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn recent_fetch_runs_for_plan_orders_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut older = sample_open(plan_id);
        older.started_at = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        store.insert_fetch_run(&older).unwrap();

        let mut newer = sample_open(plan_id);
        newer.started_at = Utc.with_ymd_and_hms(2026, 4, 27, 0, 0, 0).unwrap();
        store.insert_fetch_run(&newer).unwrap();

        let recent = store.recent_fetch_runs_for_plan(plan_id, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, newer.id);
        assert_eq!(recent[1].id, older.id);
    }

    #[test]
    fn recent_fetch_runs_for_plan_filters_by_plan() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();

        store.insert_fetch_run(&sample_open(plan_a)).unwrap();
        store.insert_fetch_run(&sample_open(plan_a)).unwrap();
        store.insert_fetch_run(&sample_open(plan_b)).unwrap();

        let a = store.recent_fetch_runs_for_plan(plan_a, 10).unwrap();
        let b = store.recent_fetch_runs_for_plan(plan_b, 10).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
        assert!(a.iter().all(|r| r.plan_id == plan_a));
        assert!(b.iter().all(|r| r.plan_id == plan_b));
    }

    #[test]
    fn fetch_run_round_trips_with_error_summary() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let mut run = sample_open(plan_id);
        store.insert_fetch_run(&run).unwrap();

        run.finished_at = Some(Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 1).unwrap());
        run.error_summary = Some("authoring failed wholesale: provider error".into());
        store.update_fetch_run(&run).unwrap();

        let recent = store.recent_fetch_runs_for_plan(plan_id, 10).unwrap();
        assert_eq!(recent[0].error_summary.as_deref(), Some("authoring failed wholesale: provider error"));
    }
}
