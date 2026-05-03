//! Recipe-feedback storage — ADR 0013.
//!
//! Persists per-(plan_id, source_id) operator notes that the
//! recipe-author prompt consumes the next time Level-2 authoring
//! runs for that pair. The note describes what was wrong with the
//! prior recipe ("fetched the search-form skeleton instead of the
//! listing endpoint", "JSON-path matched a metadata field rather
//! than the data field"), and the LLM uses it to author a different
//! recipe instead of repeating the mistake.
//!
//! ## Why per-(plan, source), not per-recipe-id
//!
//! Recipes rotate. Re-authoring a failed recipe produces a new row
//! sharing the same `dedup_key` (`{plan_id}:{source_id}`). Feedback
//! keyed by `recipe_id` would be silently abandoned at the moment
//! the next attempt is triggered — exactly the moment we want it
//! to be visible. Keying by `(plan_id, source_id)` survives the
//! rotation. See ADR 0013 §"The keying choice".
//!
//! ## Why overwrite, not history
//!
//! `set_plan_rejection` (the closest existing precedent) overwrites
//! `rejection_reason` on every call. The recipe-feedback channel
//! mirrors that shape: the latest note is the operator's current
//! correction, and a history would either need summarization (a
//! curated derived artifact) or rendering as a list (which dilutes
//! the LLM's focus per ADR 0012's minority report). If a future
//! consumer earns it, a sibling `recipe_feedback_history` table can
//! be added additively.
//!
//! ## Why a separate module
//!
//! Recipes (`crates/storage/src/recipes.rs`) and feedback are
//! independently versioned: a recipe row carries `version` and
//! re-authoring inserts a new row, while feedback carries no
//! version and a re-set replaces. Smearing the two lifecycles into
//! one module would muddy the read paths.
//!
//! ## Discipline carry-overs
//!
//! - **Lock-and-execute** pattern matches `recipes.rs` and
//!   `fetch_runs.rs`: take the connection mutex, run one
//!   `conn.execute(...)`, drop the lock. No long-held locks across
//!   await points (we have none anyway — DuckDB calls are sync).
//! - **Manual row iteration** matches the rest of the storage crate:
//!   the row mapper returns `crate::Result` so a malformed row
//!   surfaces as a typed `StorageError`, not a stringly-typed
//!   `duckdb::Error`. `query_map` would force the closure to return
//!   `duckdb::Result`.

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Columns the storage layer writes when an operator flags a recipe.
/// Mirrors `RecipeRow` / `FetchRunRow` in style: a flat struct that
/// the typed pipeline-layer helper assembles before calling.
#[derive(Debug, Clone)]
pub struct RecipeFeedbackRow {
    pub plan_id: Uuid,
    pub source_id: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

/// A recipe-feedback row as it comes back out of storage. Same shape
/// as [`RecipeFeedbackRow`]; kept separate so a future read-only
/// projection (e.g. summary fields) can diverge without rewriting
/// every call site.
#[derive(Debug, Clone)]
pub struct StoredRecipeFeedback {
    pub plan_id: Uuid,
    pub source_id: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

impl Store {
    /// Upsert one feedback note for a (plan, source) pair.
    ///
    /// Idempotent in the sense that calling it twice with the same
    /// row succeeds; calling it with a different `note` overwrites
    /// the prior value. `created_at` is taken from the row — the
    /// caller stamps it (typically `Utc::now()` at the api boundary).
    ///
    /// Implementation: `INSERT INTO ... ON CONFLICT (plan_id,
    /// source_id) DO UPDATE SET note = excluded.note, created_at =
    /// excluded.created_at`. DuckDB 1.x supports the SQL-standard
    /// `ON CONFLICT` upsert form; using it keeps the operation
    /// atomic without explicit transaction management.
    pub fn set_recipe_feedback(&self, r: &RecipeFeedbackRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO recipe_feedback (plan_id, source_id, note, created_at)
                 VALUES (?, ?, ?, ?)
             ON CONFLICT (plan_id, source_id) DO UPDATE
                 SET note = excluded.note,
                     created_at = excluded.created_at",
            params![r.plan_id, r.source_id, r.note, r.created_at],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Remove the feedback note for a (plan, source) pair, if any.
    ///
    /// Idempotent: a delete against a non-existent row succeeds and
    /// returns `Ok(())`. Distinct from `set_recipe_feedback` with
    /// an empty note — the api command layer collapses
    /// `Option<String> note = None` (or an empty-after-trim string)
    /// into a call to this method, so the on-disk shape is "row
    /// present iff the operator wants the LLM to see the note."
    pub fn clear_recipe_feedback(&self, plan_id: Uuid, source_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "DELETE FROM recipe_feedback WHERE plan_id = ? AND source_id = ?",
            params![plan_id, source_id],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Fetch the feedback note for one (plan, source) pair, if any.
    ///
    /// The fetch executor's `author_one` calls this immediately
    /// before assembling the recipe-author `AuthoringContext`, so
    /// the LLM sees the note via the v1.8 prompt's
    /// `{{RECIPE_FEEDBACK}}` placeholder. Returns `Ok(None)` when
    /// there is no feedback for the pair — the common case.
    pub fn recipe_feedback_for_source(
        &self,
        plan_id: Uuid,
        source_id: &str,
    ) -> Result<Option<StoredRecipeFeedback>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT plan_id, source_id, note, created_at
                 FROM recipe_feedback
                 WHERE plan_id = ? AND source_id = ?",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![plan_id, source_id])
            .map_err(StorageError::DuckDb)?;
        if let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            Ok(Some(row_to_stored(row)?))
        } else {
            Ok(None)
        }
    }

    /// List every feedback note for a plan, newest first.
    ///
    /// Drives the recipe-inspection panel's indicator chips: the
    /// frontend calls `list_recipe_feedback_for_plan` on plan
    /// selection, then the chip beside each recipe lights up if a
    /// note exists for the recipe's `source_id`. Order is by
    /// `created_at DESC` for a stable display when the panel
    /// decides to show notes side by side.
    pub fn recipe_feedback_for_plan(
        &self,
        plan_id: Uuid,
    ) -> Result<Vec<StoredRecipeFeedback>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT plan_id, source_id, note, created_at
                 FROM recipe_feedback
                 WHERE plan_id = ?
                 ORDER BY created_at DESC",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![plan_id])
            .map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            out.push(row_to_stored(row)?);
        }
        Ok(out)
    }
}

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredRecipeFeedback> {
    Ok(StoredRecipeFeedback {
        plan_id: row.get(0).map_err(StorageError::DuckDb)?,
        source_id: row.get(1).map_err(StorageError::DuckDb)?,
        note: row.get(2).map_err(StorageError::DuckDb)?,
        created_at: row.get(3).map_err(StorageError::DuckDb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn sample_row(plan_id: Uuid, source_id: &str, note: &str) -> RecipeFeedbackRow {
        RecipeFeedbackRow {
            plan_id,
            source_id: source_id.to_string(),
            note: note.to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 2, 10, 0, 0).unwrap(),
        }
    }

    #[test]
    fn set_then_get_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let row = sample_row(plan_id, "world_bank_indicators", "wrong API endpoint");
        store.set_recipe_feedback(&row).unwrap();

        let got = store
            .recipe_feedback_for_source(plan_id, "world_bank_indicators")
            .unwrap()
            .expect("row should exist");
        assert_eq!(got.plan_id, plan_id);
        assert_eq!(got.source_id, "world_bank_indicators");
        assert_eq!(got.note, "wrong API endpoint");
    }

    #[test]
    fn set_overwrites_existing_note() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let mut first = sample_row(plan_id, "imf_weo", "wrong indicator code");
        store.set_recipe_feedback(&first).unwrap();

        first.note = "actually the path matched a metadata field".to_string();
        first.created_at = Utc.with_ymd_and_hms(2026, 5, 2, 11, 0, 0).unwrap();
        store.set_recipe_feedback(&first).unwrap();

        let got = store
            .recipe_feedback_for_source(plan_id, "imf_weo")
            .unwrap()
            .unwrap();
        assert_eq!(got.note, "actually the path matched a metadata field");
        assert_eq!(got.created_at.hour(), 11);
    }

    #[test]
    fn clear_deletes_existing_row() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        store
            .set_recipe_feedback(&sample_row(plan_id, "usgs_mcs", "wrong year"))
            .unwrap();
        store.clear_recipe_feedback(plan_id, "usgs_mcs").unwrap();

        let got = store
            .recipe_feedback_for_source(plan_id, "usgs_mcs")
            .unwrap();
        assert!(got.is_none(), "expected no row after clear");
    }

    #[test]
    fn clear_is_idempotent_on_missing_row() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // No row written first — clear should still succeed.
        let plan_id = Uuid::now_v7();
        store.clear_recipe_feedback(plan_id, "never_set").unwrap();
    }

    #[test]
    fn get_returns_none_for_unset_pair() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let got = store
            .recipe_feedback_for_source(plan_id, "unknown_source")
            .unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn for_plan_lists_per_plan_only() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();

        store
            .set_recipe_feedback(&sample_row(plan_a, "src_one", "note for plan A src one"))
            .unwrap();
        store
            .set_recipe_feedback(&sample_row(plan_a, "src_two", "note for plan A src two"))
            .unwrap();
        store
            .set_recipe_feedback(&sample_row(plan_b, "src_one", "note for plan B src one"))
            .unwrap();

        let a_notes = store.recipe_feedback_for_plan(plan_a).unwrap();
        let b_notes = store.recipe_feedback_for_plan(plan_b).unwrap();

        assert_eq!(a_notes.len(), 2);
        assert_eq!(b_notes.len(), 1);
        assert!(a_notes.iter().all(|n| n.plan_id == plan_a));
        assert!(b_notes.iter().all(|n| n.plan_id == plan_b));
    }

    #[test]
    fn for_plan_orders_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut older = sample_row(plan_id, "src_older", "older");
        older.created_at = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        store.set_recipe_feedback(&older).unwrap();

        let mut newer = sample_row(plan_id, "src_newer", "newer");
        newer.created_at = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        store.set_recipe_feedback(&newer).unwrap();

        let listed = store.recipe_feedback_for_plan(plan_id).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].source_id, "src_newer");
        assert_eq!(listed[1].source_id, "src_older");
    }
}
