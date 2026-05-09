//! Per-(run, recipe-or-source) outcome rows — Session 46.
//!
//! Migration 0016 added the `fetch_run_outcomes` table — see that
//! file's header comment for the design rationale. This module is
//! the typed read/write surface.
//!
//! ## What lives here
//!
//! - [`FetchRunOutcomeKind`] — closed enum mirroring the
//!   `RecipeOutcomeDto::kind` strings the IPC boundary already
//!   carries. New variants are an ADR-level decision.
//! - [`FetchRunOutcomeRow`] / [`StoredFetchRunOutcome`] — the
//!   write/read row shapes (mirror pattern, same as `RecipeRow` /
//!   `StoredRecipe`).
//! - [`Store::insert_fetch_run_outcome`] — write site; called by the
//!   fetch executor at run completion, once per outcome.
//! - [`Store::recipe_outcomes_history_for_plan`] — the heatmap's
//!   primary read. Returns one [`RecipeOutcomeHistoryEntry`] per
//!   distinct (recipe_id, source_id) pair seen across the plan's
//!   recent runs, with that pair's per-run outcome rows ordered by
//!   recording time. Limit caps the *runs* dimension; rows below the
//!   cap fall off the front of the heatmap.
//!
//! ## Why store the kind as a string column rather than an integer
//!
//! DuckDB's enum support in the Rust crate is uneven. Storing the
//! kind as a free-form TEXT column with strict Rust-side parsing is
//! the same posture migration 0010 (`authored_from`) takes for the
//! same reason: the closed-vocabulary contract is enforced in Rust,
//! the column rejects nothing on its own. An unknown column value is
//! a hard error at load time, mirroring `AuthoredFrom::from_str`.
//!
//! ## Reading discipline
//!
//! The history query `LEFT JOIN`s `recipes` on `recipe_id` so the
//! row carries a stable `(recipe_id_or_none, source_id)` pair even
//! for `Declined` outcomes (which have no recipe). The frontend's
//! heatmap groups by that pair: a row for each recipe the executor
//! authored, plus a synthetic row per source the LLM declined. Same
//! grouping the live `FetchReport` panel uses for its outcomes
//! list.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// Closed enum for the outcome kind column
// ---------------------------------------------------------------------------

/// Closed vocabulary for the outcome a recipe or source produced
/// during a single fetch run. Mirrors the `RecipeOutcomeDto::kind`
/// strings the IPC boundary already carries — same wire form, same
/// closed set, no parallel taxonomy.
///
/// ## Why no `Unknown` fallback variant
///
/// Rows are written by the executor at run completion with one of
/// these six values; there is no pre-migration shape to back-coerce
/// (the table is fresh in 0016). An unrecognised string in the
/// column is therefore a genuine inconsistency — a hand-edit or a
/// future variant added without updating this code — and the load
/// path returns a hard error. Same posture `AuthoredFrom::from_str`
/// takes for the recipe authoring provenance enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchRunOutcomeKind {
    /// Recipe ran end-to-end and produced ≥ 0 records.
    /// `records_produced` is populated.
    Succeeded,
    /// Executor declined to run the recipe (e.g. extraction mode not
    /// yet enabled). `message` carries the reason.
    Skipped,
    /// Recipe ran and broke at a named stage. `failure_stage` and
    /// `message` are populated.
    Failed,
    /// Source returned 429 in a way the inline-retry path surfaced
    /// rather than waited through. `retry_after_seconds` may be
    /// populated when the server provided one.
    RateLimited,
    /// LLM declined to author a recipe for this source. No
    /// `recipe_id`. `message` carries the LLM's verbatim reason.
    Declined,
    /// Pre-Session-37 plan whose `preferred_source_ids` cannot be
    /// authored against. No `recipe_id`, no `message` (the kind
    /// itself names the remediation: re-classify).
    LegacyPlanCannotAuthor,
}

impl FetchRunOutcomeKind {
    /// The exact string written to the DuckDB column. Centralised so
    /// the migration's wire form and the Rust round-trip can't drift.
    /// Matches the `RecipeOutcomeDto::kind` JSON form one-for-one.
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchRunOutcomeKind::Succeeded => "succeeded",
            FetchRunOutcomeKind::Skipped => "skipped",
            FetchRunOutcomeKind::Failed => "failed",
            FetchRunOutcomeKind::RateLimited => "rate_limited",
            FetchRunOutcomeKind::Declined => "declined",
            FetchRunOutcomeKind::LegacyPlanCannotAuthor => "legacy_plan_cannot_author",
        }
    }
}

impl fmt::Display for FetchRunOutcomeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FetchRunOutcomeKind {
    type Err = StorageError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "succeeded" => Ok(FetchRunOutcomeKind::Succeeded),
            "skipped" => Ok(FetchRunOutcomeKind::Skipped),
            "failed" => Ok(FetchRunOutcomeKind::Failed),
            "rate_limited" => Ok(FetchRunOutcomeKind::RateLimited),
            "declined" => Ok(FetchRunOutcomeKind::Declined),
            "legacy_plan_cannot_author" => Ok(FetchRunOutcomeKind::LegacyPlanCannotAuthor),
            other => Err(StorageError::Other(format!(
                "unknown outcome_kind in column: {other:?} (expected one of \
                 succeeded / skipped / failed / rate_limited / declined / \
                 legacy_plan_cannot_author)"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Row shapes
// ---------------------------------------------------------------------------

/// Columns the executor writes per outcome at run completion.
///
/// `recipe_id` is `None` for `Declined` and `LegacyPlanCannotAuthor`
/// outcomes (no recipe was authored). The other variant-specific
/// columns (`records_produced`, `retry_after_seconds`,
/// `failure_stage`, `message`) are populated only when their
/// outcome kind expects them — see the [`FetchRunOutcomeKind`] doc
/// for the per-variant payload table.
#[derive(Debug, Clone)]
pub struct FetchRunOutcomeRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub plan_id: Uuid,
    pub recipe_id: Option<Uuid>,
    pub source_id: String,
    pub outcome_kind: FetchRunOutcomeKind,
    pub records_produced: Option<u32>,
    pub retry_after_seconds: Option<u64>,
    pub failure_stage: Option<String>,
    pub message: Option<String>,
    pub attempted_at: DateTime<Utc>,
}

/// A row as it comes back out of storage. Same shape as
/// [`FetchRunOutcomeRow`].
#[derive(Debug, Clone)]
pub struct StoredFetchRunOutcome {
    pub id: Uuid,
    pub run_id: Uuid,
    pub plan_id: Uuid,
    pub recipe_id: Option<Uuid>,
    pub source_id: String,
    pub outcome_kind: FetchRunOutcomeKind,
    pub records_produced: Option<u32>,
    pub retry_after_seconds: Option<u64>,
    pub failure_stage: Option<String>,
    pub message: Option<String>,
    pub attempted_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// History grouping (heatmap-shaped read)
// ---------------------------------------------------------------------------

/// One per-run outcome cell inside a [`RecipeOutcomeHistoryEntry`].
///
/// The frontend's heatmap renders one column per `run_id` ordered by
/// `attempted_at`; this row is the cell colour-and-detail tuple.
#[derive(Debug, Clone)]
pub struct RecipeOutcomeHistoryRunRow {
    pub run_id: Uuid,
    pub attempted_at: DateTime<Utc>,
    pub outcome_kind: FetchRunOutcomeKind,
    pub records_produced: Option<u32>,
    pub retry_after_seconds: Option<u64>,
    pub failure_stage: Option<String>,
    pub message: Option<String>,
}

/// One row in the heatmap: a (recipe_or_source) plus its outcomes
/// across the plan's recent runs.
///
/// The natural key is `(recipe_id, source_id)`:
/// - For `Succeeded` / `Skipped` / `Failed` / `RateLimited`:
///   `recipe_id = Some(uuid)`; rows for the same recipe across runs
///   group together.
/// - For `Declined` / `LegacyPlanCannotAuthor`:
///   `recipe_id = None`; rows group by `source_id` (the only stable
///   identifier the LLM-decline path produces).
///
/// `runs` is ordered oldest-first so the frontend can render runs
/// left-to-right without re-sorting; the storage SQL handles the
/// ordering.
#[derive(Debug, Clone)]
pub struct RecipeOutcomeHistoryEntry {
    pub recipe_id: Option<Uuid>,
    pub source_id: String,
    pub runs: Vec<RecipeOutcomeHistoryRunRow>,
}

// ---------------------------------------------------------------------------
// Store impls
// ---------------------------------------------------------------------------

impl Store {
    /// Insert one outcome row. Errors on a primary-key conflict — the
    /// caller mints a fresh UUIDv7 per row.
    pub fn insert_fetch_run_outcome(&self, r: &FetchRunOutcomeRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO fetch_run_outcomes (
                id, run_id, plan_id, recipe_id, source_id,
                outcome_kind, records_produced, retry_after_seconds,
                failure_stage, message, attempted_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.id,
                r.run_id,
                r.plan_id,
                r.recipe_id,
                r.source_id,
                r.outcome_kind.as_str(),
                r.records_produced.map(|v| v as i64),
                r.retry_after_seconds.map(|v| v as i64),
                r.failure_stage,
                r.message,
                r.attempted_at,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Fetch every outcome row for a plan, newest first. Pure list —
    /// no grouping. Useful for tests, the per-plan history query
    /// composes on top of this.
    pub fn fetch_run_outcomes_for_plan(
        &self,
        plan_id: Uuid,
    ) -> Result<Vec<StoredFetchRunOutcome>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, run_id, plan_id, recipe_id, source_id,
                        outcome_kind, records_produced, retry_after_seconds,
                        failure_stage, message, attempted_at
                 FROM fetch_run_outcomes
                 WHERE plan_id = ?
                 ORDER BY attempted_at DESC",
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

    /// The heatmap's primary read.
    ///
    /// Returns one [`RecipeOutcomeHistoryEntry`] per distinct
    /// `(recipe_id, source_id)` pair the plan has seen, with the
    /// pair's per-run outcome rows ordered oldest-first within the
    /// entry (so the frontend renders runs left-to-right).
    ///
    /// `run_limit` caps the **runs** dimension: only outcomes from the
    /// `run_limit` most recent runs are included. Outcomes from older
    /// runs are dropped entirely (their cells aren't shown in the
    /// heatmap), but the recipe rows themselves are preserved if they
    /// have any cell in the kept window.
    ///
    /// ## Why limit on runs, not rows
    ///
    /// A pathological plan with many recipes against many runs could
    /// produce N × M rows. Limiting on rows would clip arbitrary
    /// recipes from the heatmap, breaking the "show me whether this
    /// source is consistently flaky" use case. Limiting on runs clips
    /// the time axis, which the heatmap renders as columns — natural
    /// from the user's perspective.
    pub fn recipe_outcomes_history_for_plan(
        &self,
        plan_id: Uuid,
        run_limit: usize,
    ) -> Result<Vec<RecipeOutcomeHistoryEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // First: identify the `run_limit` most recent run_ids the
        // plan has outcomes for. We can't trust `fetch_runs.started_at`
        // alone because pre-Session-46 runs land in `fetch_runs`
        // without per-outcome rows here; the heatmap's runs dimension
        // is the runs that *have outcome data*, not every run that
        // ever opened.
        let recent_run_ids: Vec<(Uuid, DateTime<Utc>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT run_id, MAX(attempted_at) AS run_recorded_at
                     FROM fetch_run_outcomes
                     WHERE plan_id = ?
                     GROUP BY run_id
                     ORDER BY run_recorded_at DESC
                     LIMIT ?",
                )
                .map_err(StorageError::DuckDb)?;
            let mut rows = stmt
                .query(params![plan_id, run_limit as i64])
                .map_err(StorageError::DuckDb)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
                let id: Uuid = row.get(0).map_err(StorageError::DuckDb)?;
                let at: DateTime<Utc> = row.get(1).map_err(StorageError::DuckDb)?;
                out.push((id, at));
            }
            out
        };

        if recent_run_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Second: pull every outcome row whose run_id is in the
        // recent set. DuckDB doesn't accept slice-as-parameter; we
        // build the IN clause manually with one ? per id. The id
        // count is bounded by `run_limit`, which the caller clamps.
        let placeholders = (0..recent_run_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT recipe_id, source_id, run_id, attempted_at,
                    outcome_kind, records_produced, retry_after_seconds,
                    failure_stage, message
             FROM fetch_run_outcomes
             WHERE plan_id = ? AND run_id IN ({placeholders})
             ORDER BY attempted_at ASC"
        );

        // duckdb's `params!` macro doesn't accept a dynamic slice
        // length; route the uniform-typed bind list through
        // `params_from_iter` (the same pattern `queries.rs` uses for
        // its IN-clause record listings).
        let mut stmt = conn.prepare(&sql).map_err(StorageError::DuckDb)?;
        let mut bind: Vec<Uuid> = Vec::with_capacity(1 + recent_run_ids.len());
        bind.push(plan_id);
        for (id, _) in &recent_run_ids {
            bind.push(*id);
        }
        let mut rows = stmt
            .query(duckdb::params_from_iter(bind.iter()))
            .map_err(StorageError::DuckDb)?;

        // Group while we read. Insertion-ordered (we sorted ASC) so
        // the runs Vec naturally accumulates oldest-first.
        let mut entries: Vec<RecipeOutcomeHistoryEntry> = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            let recipe_id: Option<Uuid> = row.get(0).map_err(StorageError::DuckDb)?;
            let source_id: String = row.get(1).map_err(StorageError::DuckDb)?;
            let run_id: Uuid = row.get(2).map_err(StorageError::DuckDb)?;
            let attempted_at: DateTime<Utc> = row.get(3).map_err(StorageError::DuckDb)?;
            let kind_str: String = row.get(4).map_err(StorageError::DuckDb)?;
            let outcome_kind = FetchRunOutcomeKind::from_str(&kind_str)?;
            let records_produced: Option<i64> = row.get(5).map_err(StorageError::DuckDb)?;
            let retry_after_seconds: Option<i64> = row.get(6).map_err(StorageError::DuckDb)?;
            let failure_stage: Option<String> = row.get(7).map_err(StorageError::DuckDb)?;
            let message: Option<String> = row.get(8).map_err(StorageError::DuckDb)?;

            // Locate or create the entry for this (recipe_id,
            // source_id) pair. Linear scan is fine — bounded by
            // recipe count per plan (≤ a few dozen in practice).
            let cell = RecipeOutcomeHistoryRunRow {
                run_id,
                attempted_at,
                outcome_kind,
                records_produced: records_produced.map(|v| v as u32),
                retry_after_seconds: retry_after_seconds.map(|v| v as u64),
                failure_stage,
                message,
            };

            let idx = entries
                .iter()
                .position(|e| e.recipe_id == recipe_id && e.source_id == source_id);
            match idx {
                Some(i) => entries[i].runs.push(cell),
                None => entries.push(RecipeOutcomeHistoryEntry {
                    recipe_id,
                    source_id,
                    runs: vec![cell],
                }),
            }
        }

        Ok(entries)
    }
}

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredFetchRunOutcome> {
    let kind_str: String = row.get(5).map_err(StorageError::DuckDb)?;
    let outcome_kind = FetchRunOutcomeKind::from_str(&kind_str)?;
    let records_produced: Option<i64> = row.get(6).map_err(StorageError::DuckDb)?;
    let retry_after_seconds: Option<i64> = row.get(7).map_err(StorageError::DuckDb)?;
    Ok(StoredFetchRunOutcome {
        id: row.get(0).map_err(StorageError::DuckDb)?,
        run_id: row.get(1).map_err(StorageError::DuckDb)?,
        plan_id: row.get(2).map_err(StorageError::DuckDb)?,
        recipe_id: row.get(3).map_err(StorageError::DuckDb)?,
        source_id: row.get(4).map_err(StorageError::DuckDb)?,
        outcome_kind,
        records_produced: records_produced.map(|v| v as u32),
        retry_after_seconds: retry_after_seconds.map(|v| v as u64),
        failure_stage: row.get(8).map_err(StorageError::DuckDb)?,
        message: row.get(9).map_err(StorageError::DuckDb)?,
        attempted_at: row.get(10).map_err(StorageError::DuckDb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_succeeded(plan_id: Uuid, run_id: Uuid, recipe_id: Uuid) -> FetchRunOutcomeRow {
        FetchRunOutcomeRow {
            id: Uuid::now_v7(),
            run_id,
            plan_id,
            recipe_id: Some(recipe_id),
            source_id: "pubs.usgs.gov".into(),
            outcome_kind: FetchRunOutcomeKind::Succeeded,
            records_produced: Some(1),
            retry_after_seconds: None,
            failure_stage: None,
            message: None,
            attempted_at: Utc.with_ymd_and_hms(2026, 5, 9, 7, 6, 44).unwrap(),
        }
    }

    fn sample_declined(plan_id: Uuid, run_id: Uuid, source_id: &str) -> FetchRunOutcomeRow {
        FetchRunOutcomeRow {
            id: Uuid::now_v7(),
            run_id,
            plan_id,
            recipe_id: None,
            source_id: source_id.into(),
            outcome_kind: FetchRunOutcomeKind::Declined,
            records_produced: None,
            retry_after_seconds: None,
            failure_stage: None,
            message: Some("url proposer declined after 2 attempt(s)".into()),
            attempted_at: Utc.with_ymd_and_hms(2026, 5, 9, 7, 7, 23).unwrap(),
        }
    }

    fn sample_failed(
        plan_id: Uuid,
        run_id: Uuid,
        recipe_id: Uuid,
        stage: &str,
    ) -> FetchRunOutcomeRow {
        FetchRunOutcomeRow {
            id: Uuid::now_v7(),
            run_id,
            plan_id,
            recipe_id: Some(recipe_id),
            source_id: "pubs.usgs.gov".into(),
            outcome_kind: FetchRunOutcomeKind::Failed,
            records_produced: None,
            retry_after_seconds: None,
            failure_stage: Some(stage.into()),
            message: Some("extraction [pdf_table]: row out of range".into()),
            attempted_at: Utc.with_ymd_and_hms(2026, 5, 9, 7, 7, 30).unwrap(),
        }
    }

    // -- kind round-trip ----------------------------------------------------

    #[test]
    fn outcome_kind_strings_are_stable() {
        assert_eq!(FetchRunOutcomeKind::Succeeded.as_str(), "succeeded");
        assert_eq!(FetchRunOutcomeKind::Skipped.as_str(), "skipped");
        assert_eq!(FetchRunOutcomeKind::Failed.as_str(), "failed");
        assert_eq!(FetchRunOutcomeKind::RateLimited.as_str(), "rate_limited");
        assert_eq!(FetchRunOutcomeKind::Declined.as_str(), "declined");
        assert_eq!(
            FetchRunOutcomeKind::LegacyPlanCannotAuthor.as_str(),
            "legacy_plan_cannot_author"
        );
        for v in [
            FetchRunOutcomeKind::Succeeded,
            FetchRunOutcomeKind::Skipped,
            FetchRunOutcomeKind::Failed,
            FetchRunOutcomeKind::RateLimited,
            FetchRunOutcomeKind::Declined,
            FetchRunOutcomeKind::LegacyPlanCannotAuthor,
        ] {
            let parsed: FetchRunOutcomeKind = v.as_str().parse().unwrap();
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn outcome_kind_from_str_rejects_unknown_variant() {
        let err = FetchRunOutcomeKind::from_str("not_a_real_variant").unwrap_err();
        assert!(err.to_string().contains("not_a_real_variant"));
    }

    // -- insert + per-plan list --------------------------------------------

    #[test]
    fn outcome_round_trips_for_succeeded_recipe() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let recipe_id = Uuid::now_v7();
        let row = sample_succeeded(plan_id, run_id, recipe_id);
        store.insert_fetch_run_outcome(&row).unwrap();

        let got = store.fetch_run_outcomes_for_plan(plan_id).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].outcome_kind, FetchRunOutcomeKind::Succeeded);
        assert_eq!(got[0].recipe_id, Some(recipe_id));
        assert_eq!(got[0].source_id, "pubs.usgs.gov");
        assert_eq!(got[0].records_produced, Some(1));
        assert!(got[0].failure_stage.is_none());
        assert!(got[0].message.is_none());
    }

    #[test]
    fn outcome_round_trips_for_declined_no_recipe_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let row = sample_declined(plan_id, run_id, "www.sec.gov");
        store.insert_fetch_run_outcome(&row).unwrap();

        let got = store.fetch_run_outcomes_for_plan(plan_id).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].outcome_kind, FetchRunOutcomeKind::Declined);
        assert!(
            got[0].recipe_id.is_none(),
            "declined outcomes have no recipe_id"
        );
        assert_eq!(got[0].source_id, "www.sec.gov");
        assert!(got[0].message.as_deref().unwrap().contains("declined"));
    }

    #[test]
    fn fetch_run_outcomes_for_plan_orders_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let run_id_a = Uuid::now_v7();
        let run_id_b = Uuid::now_v7();

        let mut older = sample_succeeded(plan_id, run_id_a, Uuid::now_v7());
        older.attempted_at = Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap();
        store.insert_fetch_run_outcome(&older).unwrap();

        let mut newer = sample_succeeded(plan_id, run_id_b, Uuid::now_v7());
        newer.attempted_at = Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap();
        store.insert_fetch_run_outcome(&newer).unwrap();

        let got = store.fetch_run_outcomes_for_plan(plan_id).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].attempted_at > got[1].attempted_at);
    }

    #[test]
    fn fetch_run_outcomes_for_plan_filters_by_plan_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();

        store
            .insert_fetch_run_outcome(&sample_succeeded(plan_a, Uuid::now_v7(), Uuid::now_v7()))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_succeeded(plan_b, Uuid::now_v7(), Uuid::now_v7()))
            .unwrap();

        let for_a = store.fetch_run_outcomes_for_plan(plan_a).unwrap();
        let for_b = store.fetch_run_outcomes_for_plan(plan_b).unwrap();
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_a[0].plan_id, plan_a);
        assert_eq!(for_b[0].plan_id, plan_b);
    }

    // -- history grouping ---------------------------------------------------

    #[test]
    fn history_returns_empty_when_no_outcomes_recorded() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let entries = store
            .recipe_outcomes_history_for_plan(Uuid::now_v7(), 10)
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn history_groups_by_recipe_then_source_for_decline() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let recipe_id = Uuid::now_v7();

        store
            .insert_fetch_run_outcome(&sample_succeeded(plan_id, run_id, recipe_id))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_declined(plan_id, run_id, "www.sec.gov"))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_declined(plan_id, run_id, "www.worldbank.org"))
            .unwrap();

        let entries = store
            .recipe_outcomes_history_for_plan(plan_id, 10)
            .unwrap();
        assert_eq!(
            entries.len(),
            3,
            "one row for the recipe, one row per declined source"
        );
        // The recipe entry has Some(recipe_id); the decline entries
        // have None.
        let recipe_entries: Vec<_> = entries.iter().filter(|e| e.recipe_id.is_some()).collect();
        let decline_entries: Vec<_> = entries.iter().filter(|e| e.recipe_id.is_none()).collect();
        assert_eq!(recipe_entries.len(), 1);
        assert_eq!(decline_entries.len(), 2);
        assert_eq!(recipe_entries[0].recipe_id, Some(recipe_id));
        assert_eq!(recipe_entries[0].source_id, "pubs.usgs.gov");
    }

    #[test]
    fn history_orders_runs_within_entry_oldest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let recipe_id = Uuid::now_v7();
        let run_id_old = Uuid::now_v7();
        let run_id_new = Uuid::now_v7();

        let mut older = sample_succeeded(plan_id, run_id_old, recipe_id);
        older.attempted_at = Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap();
        store.insert_fetch_run_outcome(&older).unwrap();

        let mut newer = sample_failed(plan_id, run_id_new, recipe_id, "apply");
        newer.attempted_at = Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap();
        store.insert_fetch_run_outcome(&newer).unwrap();

        let entries = store
            .recipe_outcomes_history_for_plan(plan_id, 10)
            .unwrap();
        assert_eq!(entries.len(), 1, "same recipe across both runs");
        let entry = &entries[0];
        assert_eq!(entry.runs.len(), 2);
        assert!(
            entry.runs[0].attempted_at < entry.runs[1].attempted_at,
            "runs vec must be oldest-first; got {:?} then {:?}",
            entry.runs[0].attempted_at,
            entry.runs[1].attempted_at,
        );
        assert_eq!(entry.runs[0].outcome_kind, FetchRunOutcomeKind::Succeeded);
        assert_eq!(entry.runs[1].outcome_kind, FetchRunOutcomeKind::Failed);
        assert_eq!(entry.runs[1].failure_stage.as_deref(), Some("apply"));
    }

    #[test]
    fn history_clamps_runs_dimension_keeps_recent_runs_only() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let recipe_id = Uuid::now_v7();

        // 5 runs for the same recipe, oldest to newest.
        for day in 1..=5 {
            let run_id = Uuid::now_v7();
            let mut row = sample_succeeded(plan_id, run_id, recipe_id);
            row.attempted_at = Utc.with_ymd_and_hms(2026, 5, day, 10, 0, 0).unwrap();
            store.insert_fetch_run_outcome(&row).unwrap();
        }

        // run_limit = 3 → keep the 3 most recent runs only.
        let entries = store.recipe_outcomes_history_for_plan(plan_id, 3).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.runs.len(), 3, "runs dimension clipped to limit");
        // Oldest of the kept runs is day 3 (5 minus 3 + 1).
        let earliest = entry.runs.first().unwrap().attempted_at;
        assert!(
            earliest >= Utc.with_ymd_and_hms(2026, 5, 3, 0, 0, 0).unwrap(),
            "ran from day 3 onward; got {earliest:?}"
        );
    }

    #[test]
    fn history_carries_per_variant_payload_through() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();

        // RateLimited with retry-after.
        let limited = FetchRunOutcomeRow {
            id: Uuid::now_v7(),
            run_id,
            plan_id,
            recipe_id: Some(Uuid::now_v7()),
            source_id: "throttled.example.com".into(),
            outcome_kind: FetchRunOutcomeKind::RateLimited,
            records_produced: None,
            retry_after_seconds: Some(120),
            failure_stage: None,
            message: None,
            attempted_at: Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
        };
        store.insert_fetch_run_outcome(&limited).unwrap();

        let entries = store
            .recipe_outcomes_history_for_plan(plan_id, 10)
            .unwrap();
        assert_eq!(entries.len(), 1);
        let cell = &entries[0].runs[0];
        assert_eq!(cell.outcome_kind, FetchRunOutcomeKind::RateLimited);
        assert_eq!(cell.retry_after_seconds, Some(120));
    }
}
