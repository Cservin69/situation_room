//! Promote-pass history persistence — Session 86.
//!
//! Migration 0017 added the `promote_history` table — see that file's
//! header comment for the design rationale. This module is the typed
//! read/write surface.
//!
//! ## What lives here
//!
//! - [`PromoteHistoryRow`] — the write/read row shape.
//! - [`Store::insert_promote_history_entry`] — write site; called from
//!   the api crate's `record_last_promote_summary` after the in-memory
//!   ring buffer push. Side-effect: prunes rows beyond the cap.
//! - [`Store::load_recent_promote_history`] — boot-time read; the api
//!   crate seeds the in-memory ring buffer from this on `AppState::new`.
//!
//! ## Why both API and storage layers carry a cap
//!
//! The in-memory `PROMOTE_HISTORY_CAP` (api crate, today 20) governs
//! the runtime ring's size. This module's `insert` writes through then
//! prunes to that same cap. Keeping the cap in one place would force a
//! cross-crate constant; today the api crate threads the desired cap
//! through every call site. If the two values ever drift the table
//! will hold up to the larger of the two between operator activity
//! and the next insert, which is harmless (the load path re-bounds at
//! read time).
//!
//! ## Trigger enum stays in the api crate
//!
//! `LastPromoteTrigger` lives in `crates/api/src/commands.rs` because
//! it's part of the Tauri command surface. Persisting it here as a
//! free-form TEXT keeps the storage layer from needing a dependency
//! on the api crate. The closed-vocabulary contract is enforced at the
//! call sites (the strings come from `LastPromoteTrigger::as_str()`),
//! not by the column.

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// Row shape
// ---------------------------------------------------------------------------

/// One persisted promote-pass summary row. Mirrors the in-memory
/// `LastPromoteSummary` shape from the api crate, with `report`
/// serialized as JSON text rather than the typed struct (storage
/// shouldn't depend on the pipeline's record-schema crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteHistoryRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub at: DateTime<Utc>,
    /// Closed vocab via [`crate::promote_history::TRIGGER_STRINGS`].
    /// Today: `"auto_after_fetch"` | `"manual"`. The api crate's
    /// `LastPromoteTrigger::as_str()` is the authoritative source.
    pub trigger: String,
    /// `PromoteReport` serialized via `serde_json::to_string`. The api
    /// crate decodes via `serde_json::from_str` on load.
    pub report_json: String,
}

/// Closed-vocabulary trigger strings the storage layer accepts.
///
/// Kept in lockstep with `LastPromoteTrigger::as_str()` in the api
/// crate; a lockstep unit test in that crate verifies the two match.
/// Storage rejects rows with a trigger outside this set at load time —
/// same posture migration 0016 takes for `outcome_kind`.
pub const TRIGGER_STRINGS: &[&str] = &["auto_after_fetch", "manual"];

// ---------------------------------------------------------------------------
// Store impls
// ---------------------------------------------------------------------------

impl Store {
    /// Insert one promote-history row, then prune the table to the
    /// most recent `cap` rows (ordered by `at DESC`). The prune step
    /// is what keeps the on-disk row count bounded; a million-pass
    /// operator session that never queries the strip still ends with
    /// `cap` rows on disk.
    ///
    /// `cap` should match the api crate's `PROMOTE_HISTORY_CAP`. If
    /// the caller passes 0 we treat it as "no persistence" — the
    /// insert + prune still run, but the prune leaves an empty table.
    /// This is a defensive posture: a misconfigured caller should
    /// degrade to empty, not unbounded.
    pub fn insert_promote_history_entry(
        &self,
        row: &PromoteHistoryRow,
        cap: usize,
    ) -> Result<()> {
        // Defence-in-depth: refuse unknown trigger strings at the
        // boundary. The api crate already only writes the two closed
        // strings, but a future call site or a hand-edit would
        // otherwise sneak through.
        if !TRIGGER_STRINGS.contains(&row.trigger.as_str()) {
            return Err(StorageError::Other(format!(
                "promote_history: unknown trigger {:?}; expected one of {:?}",
                row.trigger, TRIGGER_STRINGS
            )));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO promote_history (id, plan_id, recorded_at, trigger, report)
             VALUES (?, ?, ?, ?, ?)",
            params![row.id, row.plan_id, row.at, row.trigger, row.report_json],
        )
        .map_err(StorageError::DuckDb)?;

        // Prune. DuckDB's DELETE doesn't natively support `ORDER BY +
        // LIMIT` on the deletion target — express the prune as "delete
        // rows whose `recorded_at` is older than the cap-th newest row."
        //
        // Note the OFFSET arithmetic: to keep the newest `cap` rows we
        // want the cutoff row at 0-indexed position `cap - 1` (the
        // last row that should survive). A row whose `recorded_at` is
        // strictly less than the cutoff's `recorded_at` is older than
        // the cap-th newest and must be deleted.
        //
        // When there are fewer than `cap` rows the subquery returns
        // NULL and the WHERE clause evaluates to false, so nothing is
        // deleted — exactly the no-op we want at low fill.
        //
        // cap == 0 is a defensive case (caller misconfiguration): no
        // rows should survive, so we issue an unconditional DELETE
        // rather than try to express "OFFSET -1" in SQL.
        if cap == 0 {
            conn.execute("DELETE FROM promote_history", [])
                .map_err(StorageError::DuckDb)?;
        } else {
            conn.execute(
                "DELETE FROM promote_history
                 WHERE recorded_at < (
                     SELECT recorded_at FROM promote_history
                     ORDER BY recorded_at DESC
                     OFFSET ? LIMIT 1
                 )",
                params![(cap - 1) as i64],
            )
            .map_err(StorageError::DuckDb)?;
        }

        Ok(())
    }

    /// Load the most recent `limit` promote-history rows, ordered
    /// newest-first.
    ///
    /// Returns an empty vec when the table is empty (the common cold-
    /// boot case). The api crate seeds its in-memory ring buffer from
    /// this on `AppState::new`; the boot-time read is O(limit), and
    /// the column-index on `at DESC` keeps the cost flat regardless
    /// of historical-row count.
    pub fn load_recent_promote_history(&self, limit: usize) -> Result<Vec<PromoteHistoryRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, plan_id, recorded_at, trigger, report
                 FROM promote_history
                 ORDER BY recorded_at DESC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(PromoteHistoryRow {
                    id: row.get(0)?,
                    plan_id: row.get(1)?,
                    at: row.get(2)?,
                    trigger: row.get(3)?,
                    report_json: row.get(4)?,
                })
            })
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::with_capacity(limit);
        for row in rows {
            let row = row.map_err(StorageError::DuckDb)?;
            // Defence-in-depth: reject unknown trigger strings on
            // read. Same posture as the insert-side check. A hand-
            // edited row with a typoed trigger is a hard error rather
            // than a silent skip.
            if !TRIGGER_STRINGS.contains(&row.trigger.as_str()) {
                return Err(StorageError::Other(format!(
                    "promote_history: unknown trigger {:?} on row {:?}",
                    row.trigger, row.id
                )));
            }
            out.push(row);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row_at(plan: Uuid, at: DateTime<Utc>, trigger: &str) -> PromoteHistoryRow {
        PromoteHistoryRow {
            id: Uuid::now_v7(),
            plan_id: plan,
            at,
            trigger: trigger.into(),
            report_json: r#"{"assertions_considered":1,"groups_promoted":1,"skipped_already_promoted":0,"observations_emitted":1,"events_emitted":0,"relations_emitted":0,"entity_attributes_emitted":0,"insert_failures":0,"authoritative_promoted":0}"#.into(),
        }
    }

    #[test]
    fn round_trip_one_entry() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = Uuid::now_v7();
        let r = row_at(plan, Utc::now(), "auto_after_fetch");
        store.insert_promote_history_entry(&r, 20).unwrap();
        let loaded = store.load_recent_promote_history(20).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plan_id, plan);
        assert_eq!(loaded[0].trigger, "auto_after_fetch");
    }

    #[test]
    fn load_returns_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = Uuid::now_v7();
        let t0 = Utc::now();
        let r_old = row_at(plan, t0 - chrono::Duration::seconds(100), "manual");
        let r_mid = row_at(plan, t0 - chrono::Duration::seconds(50), "auto_after_fetch");
        let r_new = row_at(plan, t0, "manual");
        // Insert in non-monotonic order to confirm the ORDER BY does the work.
        store.insert_promote_history_entry(&r_mid, 20).unwrap();
        store.insert_promote_history_entry(&r_new, 20).unwrap();
        store.insert_promote_history_entry(&r_old, 20).unwrap();
        let loaded = store.load_recent_promote_history(20).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].id, r_new.id);
        assert_eq!(loaded[1].id, r_mid.id);
        assert_eq!(loaded[2].id, r_old.id);
    }

    #[test]
    fn insert_prunes_to_cap() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = Uuid::now_v7();
        let t0 = Utc::now();
        let cap = 3usize;
        // Insert 5 rows with strictly increasing timestamps.
        for i in 0..5 {
            let r = row_at(
                plan,
                t0 + chrono::Duration::seconds(i as i64),
                "auto_after_fetch",
            );
            store.insert_promote_history_entry(&r, cap).unwrap();
        }
        let loaded = store.load_recent_promote_history(20).unwrap();
        // Only the 3 newest survive.
        assert_eq!(loaded.len(), cap);
        // Order newest-first.
        let secs: Vec<i64> = loaded
            .iter()
            .map(|r| (r.at - t0).num_seconds())
            .collect();
        assert_eq!(secs, vec![4, 3, 2]);
    }

    #[test]
    fn load_respects_limit() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = Uuid::now_v7();
        let t0 = Utc::now();
        for i in 0..10 {
            let r = row_at(
                plan,
                t0 + chrono::Duration::seconds(i as i64),
                "auto_after_fetch",
            );
            store.insert_promote_history_entry(&r, 20).unwrap();
        }
        let loaded = store.load_recent_promote_history(5).unwrap();
        assert_eq!(loaded.len(), 5);
    }

    #[test]
    fn insert_rejects_unknown_trigger() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let plan = Uuid::now_v7();
        let bad = row_at(plan, Utc::now(), "auto_at_dawn"); // not in TRIGGER_STRINGS
        let e = store
            .insert_promote_history_entry(&bad, 20)
            .expect_err("unknown trigger must error");
        let msg = e.to_string();
        assert!(
            msg.contains("auto_at_dawn"),
            "error should name the bad value, got {msg}"
        );
    }

    #[test]
    fn load_on_empty_table_returns_empty_vec() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let loaded = store.load_recent_promote_history(20).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn insert_with_cap_zero_leaves_empty_table() {
        // Defensive: cap == 0 is a misconfiguration (caller should
        // pass PROMOTE_HISTORY_CAP, ≥ 1). The doc-comment promises
        // "the prune leaves an empty table" in this case; verify the
        // contract holds.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let r = row_at(Uuid::now_v7(), Utc::now(), "manual");
        store.insert_promote_history_entry(&r, 0).unwrap();
        let loaded = store.load_recent_promote_history(20).unwrap();
        assert!(loaded.is_empty(), "cap=0 must leave the table empty after insert+prune");
    }

    #[test]
    fn trigger_strings_match_lockstep_api_set() {
        // Lockstep guard against the api crate's
        // `LastPromoteTrigger::as_str()`. Adding a new trigger variant
        // there without also updating TRIGGER_STRINGS here would let
        // an insert from the new variant pass through, but the row
        // would fail load — exactly the false-positive the lockstep
        // check guards against.
        //
        // We can't depend on the api crate from storage, so the test
        // enumerates the two closed strings explicitly. The api
        // crate has a mirror test asserting the same set.
        let expected = ["auto_after_fetch", "manual"];
        assert_eq!(TRIGGER_STRINGS, &expected);
    }
}
