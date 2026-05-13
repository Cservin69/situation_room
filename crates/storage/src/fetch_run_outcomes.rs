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

/// One per-nomination apply-stage failure entry surfaced to the
/// proposer's `prior_attempts` log on a subsequent run.
///
/// Session 53 Piece C. Pre-Session-53, `RecipeOutcome::Failed { stage:
/// Apply, .. }` rows were visible only in the FetchReport / Bucket
/// chronology — the propose-URL retry loop on the *next* run for the
/// same nomination saw a fresh empty `prior_attempts` and proposed
/// the same URL again. The pivot heuristic for an apply-stage shape
/// failure (string in numeric slot, missing required field) is the
/// same as `recipe author declined: no extractable structure` — try
/// a different path on the same host or pivot off-host — so the
/// proposer needs the row in its log to act on it.
///
/// Carries the recipe's `source_url` (which the proposer sees as
/// "URL already tried") and a head of the apply-stage error message
/// (the rationale the proposer reads to decide what shape of pivot
/// applies here).
#[derive(Debug, Clone)]
pub struct ApplyFailureForProposer {
    /// The URL the recipe was authored against. The proposer reads
    /// this as "do not retry this exact URL."
    pub source_url: String,
    /// The failure_stage the runtime recorded. Today's set is
    /// `apply` only (this query filters out `fetch` and `insert`).
    pub failure_stage: String,
    /// The runtime's apply-stage error message, head-truncated by
    /// the executor before being threaded into the proposer's
    /// prompt. The full string is on the `fetch_run_outcomes` row;
    /// callers truncate at proposer-input composition time so the
    /// truncation discipline lives next to the prompt-shaping
    /// code, not at the storage layer.
    pub message_head: String,
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

    /// Count of `Declined` outcomes for one nomination across all
    /// the plan's runs, irrespective of expectation target. Session
    /// 53 Piece F: the workstation reads this at propose-URL call
    /// time to decide whether to escalate the cheap-tier reasoning
    /// effort for stuck nominations.
    ///
    /// Counts both `nom:{nomination_id}` (nomination-level decline,
    /// e.g. URL proposer exhausted) and `nom:{nomination_id}:{bucket}:
    /// {index}` (per-target decline) — both shapes signal "this
    /// nomination didn't yield records on its last attempts." The
    /// escalation rule operates on the union: a nomination that's
    /// declined twice at the nomination level and once at the per-
    /// target level is just as stuck as one that declined three
    /// times at the same level.
    ///
    /// **Per-nomination, not per-source.** This count is keyed by
    /// `nomination_id`, which the L1 classifier emits per source-
    /// description; it is not keyed by URL host or publisher. The
    /// closed-vocabulary discipline forbids encoding per-host
    /// preferences in code; per-nomination escalation is a runtime
    /// feedback loop on observed attempts, not a source-routing
    /// rule. See `ReasoningEffort`'s doc-comment for the principle.
    pub fn decline_count_for_nomination(
        &self,
        plan_id: Uuid,
        nomination_id: Uuid,
    ) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // Match both bare `nom:{id}` and `nom:{id}:...` shapes via
        // a single LIKE with the trailing `%`. `:` is not a regex
        // metacharacter in SQL LIKE; the % handles either form.
        let pattern = format!("nom:{nomination_id}%");

        let mut stmt = conn
            .prepare(
                "SELECT COUNT(*)
                 FROM fetch_run_outcomes
                 WHERE plan_id = ?
                   AND outcome_kind = 'declined'
                   AND source_id LIKE ?",
            )
            .map_err(StorageError::DuckDb)?;

        let count: i64 = stmt
            .query_row(params![plan_id, pattern], |row| row.get(0))
            .map_err(StorageError::DuckDb)?;

        Ok(count.max(0) as usize)
    }

    /// Cross-run apply-stage failures for one nomination, ordered
    /// oldest-first and deduplicated by `source_url` (the proposer's
    /// natural identity for "URL already tried"). One entry per
    /// distinct URL the nomination authored against where the
    /// runtime's apply stage rejected the resulting record.
    ///
    /// Session 53 Piece C. The proposer's `prior_attempts` log is
    /// per-nomination; the `recipes.dedup_key` column already encodes
    /// `{plan_id}:{nomination_id}:{bucket}:{index}` (Session 47), so a
    /// `LIKE '{plan_id}:{nomination_id}:%'` filter pulls every recipe
    /// authored under this nomination across runs and across
    /// expectation siblings. Joining onto `fetch_run_outcomes`
    /// surfaces the apply-failures the proposer would otherwise
    /// re-propose into.
    ///
    /// `failure_stage = 'apply'` is the discriminating filter:
    /// `fetch` failures have already been recorded as proposer-
    /// visible prior attempts (the executor surfaces them via
    /// `format_prefetch_failure_for_proposer` within the same retry
    /// loop), and `insert` failures (DuckDB constraint violations
    /// after a successful apply) tell the proposer nothing
    /// actionable about the URL — those belong on the operator's
    /// log, not in the prior-attempts log.
    pub fn apply_failures_for_nomination(
        &self,
        plan_id: Uuid,
        nomination_id: Uuid,
    ) -> Result<Vec<ApplyFailureForProposer>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // The `dedup_key` LIKE pattern: {plan_id}:{nomination_id}:%
        // Both UUIDs serialize as their canonical hyphenated form, the
        // same form `dedup_key_for_recipe` uses in the executor.
        let dedup_prefix = format!("{plan_id}:{nomination_id}:%");

        let mut stmt = conn
            .prepare(
                "SELECT r.source_url, o.failure_stage, o.message, o.attempted_at
                 FROM fetch_run_outcomes o
                 JOIN recipes r ON r.id = o.recipe_id
                 WHERE o.plan_id = ?
                   AND o.outcome_kind = 'failed'
                   AND o.failure_stage = 'apply'
                   AND r.dedup_key LIKE ?
                 ORDER BY o.attempted_at DESC",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![plan_id, dedup_prefix])
            .map_err(StorageError::DuckDb)?;

        // Dedupe by source_url, keeping the most recent failure per
        // URL. The proposer cares "did the URL fail apply on the
        // last run?" — multiple identical entries for repeated runs
        // would dilute the prior-attempts list with no extra signal.
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut out_desc: Vec<ApplyFailureForProposer> = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            let url: String = row.get(0).map_err(StorageError::DuckDb)?;
            if !seen.insert(url.clone()) {
                continue;
            }
            let stage: String = row.get(1).map_err(StorageError::DuckDb)?;
            let message: Option<String> = row.get(2).map_err(StorageError::DuckDb)?;
            // We surface the message as `message_head`; truncation
            // (~120 chars per the Session 53 handoff) happens at the
            // proposer-input composition site so the prompt-shaping
            // discipline stays in one place. Empty-string fallback
            // when the stored message is NULL — apply failures
            // typically carry messages, but the wire shape is
            // tolerant.
            out_desc.push(ApplyFailureForProposer {
                source_url: url,
                failure_stage: stage,
                message_head: message.unwrap_or_default(),
            });
        }

        // Reverse to oldest-first so the proposer's prior_attempts
        // log reads in chronological order, matching the within-run
        // attempts the executor's prefetch-failure path emits.
        out_desc.reverse();
        Ok(out_desc)
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

    // -- apply_failures_for_nomination — Session 53 Piece C ---------------

    fn insert_recipe_for_nomination(
        store: &Store,
        id: Uuid,
        plan_id: Uuid,
        nomination_id: Uuid,
        bucket: &str,
        index: u32,
        source_id: &str,
        source_url: &str,
    ) {
        let row = crate::recipes::RecipeRow {
            id,
            dedup_key: Some(format!("{plan_id}:{nomination_id}:{bucket}:{index}")),
            plan_id,
            source_id: source_id.to_string(),
            source_url: source_url.to_string(),
            extraction_json: r#"{"mode":"css_select","selector":".v"}"#.to_string(),
            produces_json: "[]".to_string(),
            authored_at: Utc.with_ymd_and_hms(2026, 5, 9, 0, 0, 0).unwrap(),
            authored_by: "test".to_string(),
            version: 1,
            static_payload: None,
            authored_from: crate::recipes::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            iterator: None,
        };
        store.insert_recipe(&row).expect("insert recipe");
    }

    fn sample_apply_failure(
        plan_id: Uuid,
        run_id: Uuid,
        recipe_id: Uuid,
        message: &str,
        attempted_at: DateTime<Utc>,
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
            failure_stage: Some("apply".into()),
            message: Some(message.into()),
            attempted_at,
        }
    }

    #[test]
    fn apply_failures_returns_empty_when_no_runs_for_nomination() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let out = store
            .apply_failures_for_nomination(Uuid::now_v7(), Uuid::now_v7())
            .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn apply_failures_returns_apply_stage_for_matching_nomination() {
        // Two nominations on the same plan; one apply-failed, one
        // succeeded. The query must surface only the failed
        // nomination's URL — the proposer for nomination_b must not
        // see nomination_a's URL.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom_a = Uuid::now_v7();
        let nom_b = Uuid::now_v7();
        let run_id = Uuid::now_v7();

        let recipe_a = Uuid::now_v7();
        let recipe_b = Uuid::now_v7();
        insert_recipe_for_nomination(
            &store,
            recipe_a,
            plan_id,
            nom_a,
            "observation_metric",
            0,
            "pubs.usgs.gov",
            "https://pubs.usgs.gov/lithium-2024.pdf",
        );
        insert_recipe_for_nomination(
            &store,
            recipe_b,
            plan_id,
            nom_b,
            "observation_metric",
            0,
            "www.worldbank.org",
            "https://www.worldbank.org/pink-sheet.xls",
        );

        store
            .insert_fetch_run_outcome(&sample_apply_failure(
                plan_id,
                run_id,
                recipe_a,
                "observation content: invalid type: string \"Argentina\", expected f64",
                Utc.with_ymd_and_hms(2026, 5, 9, 18, 12, 0).unwrap(),
            ))
            .unwrap();

        let out = store
            .apply_failures_for_nomination(plan_id, nom_a)
            .unwrap();
        assert_eq!(out.len(), 1, "exactly one apply-failure for nom_a");
        assert_eq!(out[0].source_url, "https://pubs.usgs.gov/lithium-2024.pdf");
        assert_eq!(out[0].failure_stage, "apply");
        assert!(
            out[0].message_head.contains("expected f64"),
            "message_head must surface the runtime's apply error verbatim; got {}",
            out[0].message_head
        );

        let out_b = store
            .apply_failures_for_nomination(plan_id, nom_b)
            .unwrap();
        assert!(
            out_b.is_empty(),
            "nomination_b had no apply failures; query must not leak nom_a's row"
        );
    }

    #[test]
    fn apply_failures_dedupe_by_source_url_keeps_most_recent() {
        // Same recipe failed in two consecutive runs. The proposer
        // wants ONE prior_attempts entry per URL — repeated identical
        // entries dilute the prompt's signal without adding info.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom = Uuid::now_v7();
        let recipe = Uuid::now_v7();
        insert_recipe_for_nomination(
            &store,
            recipe,
            plan_id,
            nom,
            "observation_metric",
            0,
            "pubs.usgs.gov",
            "https://pubs.usgs.gov/lithium-2024.pdf",
        );

        store
            .insert_fetch_run_outcome(&sample_apply_failure(
                plan_id,
                Uuid::now_v7(),
                recipe,
                "first run apply failure",
                Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            ))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_apply_failure(
                plan_id,
                Uuid::now_v7(),
                recipe,
                "second run apply failure",
                Utc.with_ymd_and_hms(2026, 5, 9, 18, 12, 0).unwrap(),
            ))
            .unwrap();

        let out = store
            .apply_failures_for_nomination(plan_id, nom)
            .unwrap();
        assert_eq!(out.len(), 1, "deduped by source_url");
        assert_eq!(
            out[0].message_head, "second run apply failure",
            "kept the most recent failure per URL — that's the one the \
             proposer should pivot off"
        );
    }

    #[test]
    fn apply_failures_filters_out_fetch_and_insert_stages() {
        // The proposer-input composition only wants apply-stage
        // failures (the shape-mismatch class). Fetch-stage failures
        // are already surfaced via the within-run prefetch path;
        // insert-stage failures (storage constraint violations) tell
        // the proposer nothing about the URL.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom = Uuid::now_v7();
        let recipe = Uuid::now_v7();
        insert_recipe_for_nomination(
            &store,
            recipe,
            plan_id,
            nom,
            "observation_metric",
            0,
            "pubs.usgs.gov",
            "https://pubs.usgs.gov/lithium-2024.pdf",
        );

        let mut fetch_failure = sample_apply_failure(
            plan_id,
            Uuid::now_v7(),
            recipe,
            "fetch failed",
            Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
        );
        fetch_failure.failure_stage = Some("fetch".into());
        store.insert_fetch_run_outcome(&fetch_failure).unwrap();

        let mut insert_failure = sample_apply_failure(
            plan_id,
            Uuid::now_v7(),
            recipe,
            "duplicate dedup_key",
            Utc.with_ymd_and_hms(2026, 5, 9, 11, 0, 0).unwrap(),
        );
        insert_failure.failure_stage = Some("insert".into());
        store.insert_fetch_run_outcome(&insert_failure).unwrap();

        let out = store
            .apply_failures_for_nomination(plan_id, nom)
            .unwrap();
        assert!(
            out.is_empty(),
            "fetch and insert stages must not surface here; got {:?}",
            out.iter()
                .map(|f| (f.source_url.clone(), f.failure_stage.clone()))
                .collect::<Vec<_>>()
        );
    }

    // -- decline_count_for_nomination — Session 53 Piece F ---------------

    fn sample_declined_for_nom(
        plan_id: Uuid,
        run_id: Uuid,
        nomination_id: Uuid,
        suffix: Option<&str>,
        attempted_at: DateTime<Utc>,
    ) -> FetchRunOutcomeRow {
        let source_id = match suffix {
            Some(s) => format!("nom:{nomination_id}:{s}"),
            None => format!("nom:{nomination_id}"),
        };
        FetchRunOutcomeRow {
            id: Uuid::now_v7(),
            run_id,
            plan_id,
            recipe_id: None,
            source_id,
            outcome_kind: FetchRunOutcomeKind::Declined,
            records_produced: None,
            retry_after_seconds: None,
            failure_stage: None,
            message: Some("test decline".into()),
            attempted_at,
        }
    }

    #[test]
    fn decline_count_zero_when_no_declines_recorded() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let count = store
            .decline_count_for_nomination(Uuid::now_v7(), Uuid::now_v7())
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn decline_count_includes_nomination_level_and_per_target_declines() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom = Uuid::now_v7();

        // Three declines: one nomination-level, two per-target
        // (different bucket/index siblings).
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_id,
                Uuid::now_v7(),
                nom,
                None,
                Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            ))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_id,
                Uuid::now_v7(),
                nom,
                Some("observation_metric:0"),
                Utc.with_ymd_and_hms(2026, 5, 5, 10, 0, 0).unwrap(),
            ))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_id,
                Uuid::now_v7(),
                nom,
                Some("observation_metric:1"),
                Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
            ))
            .unwrap();

        let count = store.decline_count_for_nomination(plan_id, nom).unwrap();
        assert_eq!(
            count, 3,
            "both nomination-level and per-target declines must \
             count toward the escalation threshold"
        );
    }

    #[test]
    fn decline_count_filters_by_plan_and_nomination() {
        // A different nomination on the same plan, and a different
        // plan with the same nomination_id (UUIDs collide
        // hypothetically), must not contaminate the count.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();
        let nom_a = Uuid::now_v7();
        let nom_b = Uuid::now_v7();

        for _ in 0..2 {
            store
                .insert_fetch_run_outcome(&sample_declined_for_nom(
                    plan_a,
                    Uuid::now_v7(),
                    nom_a,
                    None,
                    Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
                ))
                .unwrap();
        }
        // Sibling nomination on the same plan.
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_a,
                Uuid::now_v7(),
                nom_b,
                None,
                Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
            ))
            .unwrap();
        // Same nom_a id on a different plan.
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_b,
                Uuid::now_v7(),
                nom_a,
                None,
                Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
            ))
            .unwrap();

        let nom_a_on_plan_a = store
            .decline_count_for_nomination(plan_a, nom_a)
            .unwrap();
        let nom_b_on_plan_a = store
            .decline_count_for_nomination(plan_a, nom_b)
            .unwrap();
        let nom_a_on_plan_b = store
            .decline_count_for_nomination(plan_b, nom_a)
            .unwrap();

        assert_eq!(nom_a_on_plan_a, 2);
        assert_eq!(nom_b_on_plan_a, 1);
        assert_eq!(nom_a_on_plan_b, 1);
    }

    #[test]
    fn decline_count_ignores_non_declined_outcomes() {
        // Succeeded / Failed / RateLimited outcomes are not declines
        // and must not inflate the count.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom = Uuid::now_v7();

        // One genuine decline.
        store
            .insert_fetch_run_outcome(&sample_declined_for_nom(
                plan_id,
                Uuid::now_v7(),
                nom,
                None,
                Utc.with_ymd_and_hms(2026, 5, 9, 10, 0, 0).unwrap(),
            ))
            .unwrap();

        // A succeeded run (different source_id, but the count is
        // keyed by `nom:{nomination_id}` LIKE — succeeded rows
        // typically use a host-derived source_id, not the
        // `nom:` prefix).
        let recipe_id = Uuid::now_v7();
        store
            .insert_fetch_run_outcome(&sample_succeeded(
                plan_id,
                Uuid::now_v7(),
                recipe_id,
            ))
            .unwrap();

        // A failure on the SAME `nom:` source_id — Failed is
        // distinct from Declined; this must not be counted.
        let mut failed = sample_failed(plan_id, Uuid::now_v7(), recipe_id, "apply");
        failed.source_id = format!("nom:{nom}");
        store.insert_fetch_run_outcome(&failed).unwrap();

        let count = store.decline_count_for_nomination(plan_id, nom).unwrap();
        assert_eq!(
            count, 1,
            "only Declined outcomes count; Succeeded/Failed must be ignored"
        );
    }

    #[test]
    fn apply_failures_orders_oldest_first_across_distinct_urls() {
        // Two distinct URLs failed in two distinct runs. The proposer
        // reads prior_attempts in chronological order (matching the
        // within-run attempts the prefetch-failure path emits), so
        // the query must return oldest-first.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let nom = Uuid::now_v7();
        let recipe_old = Uuid::now_v7();
        let recipe_new = Uuid::now_v7();
        insert_recipe_for_nomination(
            &store,
            recipe_old,
            plan_id,
            nom,
            "observation_metric",
            0,
            "old.example.com",
            "https://old.example.com/data",
        );
        insert_recipe_for_nomination(
            &store,
            recipe_new,
            plan_id,
            nom,
            "observation_metric",
            1,
            "new.example.com",
            "https://new.example.com/data",
        );

        store
            .insert_fetch_run_outcome(&sample_apply_failure(
                plan_id,
                Uuid::now_v7(),
                recipe_old,
                "older failure",
                Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            ))
            .unwrap();
        store
            .insert_fetch_run_outcome(&sample_apply_failure(
                plan_id,
                Uuid::now_v7(),
                recipe_new,
                "newer failure",
                Utc.with_ymd_and_hms(2026, 5, 9, 18, 12, 0).unwrap(),
            ))
            .unwrap();

        let out = store
            .apply_failures_for_nomination(plan_id, nom)
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].source_url, "https://old.example.com/data");
        assert_eq!(out[1].source_url, "https://new.example.com/data");
    }
}
