//! Recipe storage.
//!
//! Recipes are the Level-2 output of the research function (ADR 0007)
//! and are stored alongside records but not *as* records. The typed
//! `FetchRecipe` lives in `situation_room-pipeline`; storage accepts the
//! recipe as a pre-serialized `serde_json::Value` plus the scalar
//! columns we index on. Keeping the typed recipe out of storage means
//! storage stays the record-persistence layer and doesn't acquire a
//! reverse dependency on pipeline.
//!
//! A typed helper sits in `situation_room_pipeline::recipes_store` that
//! does the serialization and calls these functions. Callers should
//! prefer that helper over invoking these methods directly.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// AuthoredFrom — the provenance signal (ADR 0014)
// ---------------------------------------------------------------------------

/// Where the document excerpt fed to the Level-2 author came from for
/// this recipe's authoring run. ADR 0014.
///
/// The fetch executor's [`fetch_executor::author_one`](../../../pipeline/src/fetch_executor.rs)
/// already takes one of two excerpt-construction branches: real bytes
/// from `prefetch_excerpt` or a stub from `stub_excerpt`. This enum
/// records the branch on the recipe row so the UI can surface the
/// difference and a future ADR amendment can act on it (option 3 from
/// the Session 21 handoff — silent self-healing — is deferred but
/// would consume this field as its trigger).
///
/// ## Wire form
///
/// `serde(rename_all = "snake_case")` matches the project's existing
/// convention on `PlanStatus`, `ExtractionSpec`, and `RecipeOutcome`.
/// The DB column is the same lowercase string. Bend either side and
/// the other follows.
///
/// ## Why `Unknown` exists
///
/// Migration v10 added the column nullable. Recipes written before
/// this ADR landed read NULL; the load path coerces NULL → `Unknown`.
/// Treating NULL as `FetchedBytes` would be a retroactive truth
/// claim about historical authoring runs the new code never
/// witnessed; treating it as `StubExcerpt` would be the inverse
/// false claim. `Unknown` is honest about what's known.
///
/// New recipes written by Session-21+-aware code always carry a
/// populated value. The executor's stamping path is the only place
/// `FetchedBytes` or `StubExcerpt` are produced; tests construct
/// `Unknown` explicitly when the field is incidental to what's being
/// tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredFrom {
    /// The recipe-author prompt included the source's actual fetched
    /// response bytes (the `prefetch_excerpt` Some-path).
    FetchedBytes,
    /// The recipe-author prompt included a stub excerpt synthesized
    /// from the plan + URL only (the `stub_excerpt` path). Reachable
    /// when the source has no `endpoint_hint`, the hint is
    /// unparseable, or the pre-fetch returned an HTTP/transport error.
    StubExcerpt,
    /// The DB column was NULL (pre-ADR-0014 row). Default for any
    /// recipe whose authoring run predates the column.
    #[default]
    Unknown,
}

impl AuthoredFrom {
    /// The exact string written to the DuckDB column. Centralized so
    /// the migration's wire form and the Rust round-trip can't drift.
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthoredFrom::FetchedBytes => "fetched_bytes",
            AuthoredFrom::StubExcerpt => "stub_excerpt",
            AuthoredFrom::Unknown => "unknown",
        }
    }
}

impl fmt::Display for AuthoredFrom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AuthoredFrom {
    type Err = StorageError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "fetched_bytes" => Ok(AuthoredFrom::FetchedBytes),
            "stub_excerpt" => Ok(AuthoredFrom::StubExcerpt),
            "unknown" => Ok(AuthoredFrom::Unknown),
            other => Err(StorageError::Other(format!(
                "unknown authored_from in column: {other:?} \
                 (expected one of fetched_bytes / stub_excerpt / unknown)"
            ))),
        }
    }
}

/// Columns a recipe must provide to storage. The full recipe shape
/// lives in `extraction_json` + `produces_json`; these are the parts
/// we index on.
#[derive(Debug, Clone)]
pub struct RecipeRow {
    pub id: Uuid,
    pub dedup_key: Option<String>,
    pub plan_id: Uuid,
    pub source_id: String,
    pub source_url: String,
    pub extraction_json: String,
    pub produces_json: String,
    pub authored_at: DateTime<Utc>,
    pub authored_by: String,
    pub version: u32,
    /// Bake-time-frozen payload served to extraction in place of an
    /// HTTP fetch. `None` means the runtime fetches `source_url`
    /// normally; `Some(bytes)` short-circuits the fetch and feeds
    /// these bytes to the apply stage. See ADR 0007 Amendment 3.
    pub static_payload: Option<String>,
    /// JSON-encoded `Option<ExtractionSpec>` — the ADR 0016 iterator.
    /// `None` means "scalar recipe" (the pre-ADR-0016 contract: one
    /// record per recipe per fetch). `Some(json)` means "iterator
    /// recipe": the runtime evaluates this spec against the fetched
    /// document to get N matches, then evaluates `extraction_json`
    /// once per match scoped to the match's sub-tree, producing one
    /// record per match per `produces` binding.
    ///
    /// Stored as a JSON string (DuckDB's JSON column type) so the
    /// column round-trips through `serde_json::from_str` /
    /// `serde_json::to_string` exactly like `extraction_json`. The
    /// typed pipeline crate (`recipes_store::stored_to_recipe`)
    /// parses this back into the typed `Option<ExtractionSpec>` at
    /// the boundary; storage stays the persistence layer and never
    /// types the inner shape.
    ///
    /// **Why nullable on disk.** Migration 0015 added the column
    /// nullable. Existing rows arrive with NULL → `None` here. The
    /// recipe-author validator (`build_validated_recipe`) decides
    /// whether to populate the field per ADR 0016's mode-congruence
    /// and dedup_key_field rules; storage just persists the bytes.
    pub iterator: Option<String>,
    /// Where the recipe-author prompt's document excerpt came from
    /// (real bytes vs. stub). See [`AuthoredFrom`] and ADR 0014.
    /// New recipes always carry `FetchedBytes` or `StubExcerpt`;
    /// `Unknown` is reserved for recipes whose authoring predates
    /// migration v10.
    pub authored_from: AuthoredFrom,
    /// The recipe this row supersedes. `Some(prior.id)` for a
    /// re-authored recipe (Track A, Session 26 — manual re-author UI),
    /// `None` for first-authored recipes and for any row written before
    /// migration v11. ADR 0012 §"Storage: recipe version chain".
    ///
    /// Walkable chain: each row points at most one hop back; the
    /// chain terminates when a row's `prior_recipe_id` is `None`.
    /// `Store::recipe_lineage` walks this with a depth cap so a
    /// pathological cycle (which the executor would never produce
    /// but a hand-edit could) is caught rather than infinite-looped.
    ///
    /// **Why nullable on disk.** Migration v11 added the column
    /// nullable. Existing rows arrive with NULL → `None` here. The
    /// re-author path stamps `Some(prior.id)`; the normal authoring
    /// path stamps `None` explicitly so the field is never silently
    /// inherited from a stale struct value.
    pub prior_recipe_id: Option<Uuid>,

    /// Why this row exists in the form it does, when it was written
    /// by a re-author event. Captured from the prior recipe's last
    /// fetch failure message and (optionally) the operator's note
    /// from the dialog. `None` for first-authored recipes.
    ///
    /// Travels alongside [`Self::prior_recipe_id`]: a row with
    /// `Some(prior)` should also carry `Some(reauthor_reason)`. The
    /// SQL doesn't enforce this — the typed `RecipeRow` and the
    /// `reauthor_recipe` command path are the load-bearing
    /// invariant. Track A, Session 25/26.
    ///
    /// **Why nullable on disk.** Migration v12 added the column
    /// nullable. Existing rows (and all first-authored recipes) carry
    /// NULL.
    pub reauthor_reason: Option<String>,
}

/// A recipe row as it comes back out of storage. Same shape as
/// [`RecipeRow`]; the caller reassembles the typed `FetchRecipe`.
#[derive(Debug, Clone)]
pub struct StoredRecipe {
    pub id: Uuid,
    pub dedup_key: Option<String>,
    pub plan_id: Uuid,
    pub source_id: String,
    pub source_url: String,
    pub extraction_json: String,
    pub produces_json: String,
    pub authored_at: DateTime<Utc>,
    pub authored_by: String,
    pub version: u32,
    /// Bake-time-frozen payload — see [`RecipeRow::static_payload`].
    /// `None` for recipes authored before Session 18 (migration 0008
    /// adds the column nullable; existing rows read NULL).
    pub static_payload: Option<String>,
    /// ADR 0016 iterator — see [`RecipeRow::iterator`]. NULL on disk
    /// → `None` here. Pre-ADR-0016 recipes (and post-ADR-0016
    /// recipes against single-instance URLs) carry `None`; recipes
    /// against listing-shaped sources carry `Some(json)`.
    pub iterator: Option<String>,
    /// Authoring provenance — see [`AuthoredFrom`] and ADR 0014.
    /// Reads NULL → `Unknown` for recipes authored before migration
    /// v10. Reads the recorded variant otherwise.
    pub authored_from: AuthoredFrom,
    /// The recipe this row supersedes — see
    /// [`RecipeRow::prior_recipe_id`]. NULL on disk → `None` here
    /// (the chain head: this row was authored fresh, not re-authored
    /// from a prior). Track A, Session 26.
    pub prior_recipe_id: Option<Uuid>,
    /// Why this row was re-authored — see [`RecipeRow::reauthor_reason`].
    /// NULL on disk → `None` here. First-authored recipes carry None;
    /// re-authored rows carry the failure message + operator note.
    /// Track A, Session 25/26.
    pub reauthor_reason: Option<String>,
}

impl Store {
    /// Insert a recipe. Errors on a PRIMARY KEY conflict — callers
    /// that want upsert semantics (same id → bump version) should
    /// delete first or check for an existing dedup_key via
    /// [`Self::get_recipe_by_dedup_key`] and decide.
    pub fn insert_recipe(&self, r: &RecipeRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO recipes (
                id, dedup_key, plan_id, source_id, source_url,
                extraction, produces, authored_at, authored_by, version,
                static_payload, authored_from, prior_recipe_id, reauthor_reason,
                iterator
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.id,
                r.dedup_key,
                r.plan_id,
                r.source_id,
                r.source_url,
                r.extraction_json,
                r.produces_json,
                r.authored_at,
                r.authored_by,
                r.version as i64,
                r.static_payload,
                r.authored_from.as_str(),
                r.prior_recipe_id,
                r.reauthor_reason,
                r.iterator,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Fetch a recipe by id. Returns `Ok(None)` if not present.
    pub fn get_recipe(&self, id: Uuid) -> Result<Option<StoredRecipe>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, dedup_key, plan_id, source_id, source_url,
                        extraction, produces, authored_at, authored_by, version,
                        static_payload, authored_from, prior_recipe_id, reauthor_reason,
                        iterator
                 FROM recipes WHERE id = ?",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt.query(params![id]).map_err(StorageError::DuckDb)?;
        if let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            Ok(Some(row_to_stored(row)?))
        } else {
            Ok(None)
        }
    }

    /// Fetch a recipe by dedup_key. Returns `Ok(None)` if not present.
    /// When multiple versions share the dedup_key (the intended
    /// upsert-with-version pattern), returns the highest version.
    pub fn get_recipe_by_dedup_key(&self, dedup_key: &str) -> Result<Option<StoredRecipe>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, dedup_key, plan_id, source_id, source_url,
                        extraction, produces, authored_at, authored_by, version,
                        static_payload, authored_from, prior_recipe_id, reauthor_reason,
                        iterator
                 FROM recipes
                 WHERE dedup_key = ?
                 ORDER BY version DESC
                 LIMIT 1",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![dedup_key])
            .map_err(StorageError::DuckDb)?;
        if let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            Ok(Some(row_to_stored(row)?))
        } else {
            Ok(None)
        }
    }

    /// Count recipes. Small helper for demos and smoke tests.
    pub fn count_recipes(&self) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recipes", [], |r| r.get(0))
            .map_err(StorageError::DuckDb)?;
        Ok(count as u64)
    }

    /// List every recipe authored against a given plan, newest first.
    ///
    /// The fetch executor's primary read path: "given an accepted
    /// plan, what recipes do I run?" — answered in one indexed query.
    /// Uses the `(plan_id, source_id)` index from migration v3 for
    /// the WHERE clause; the ORDER BY is on `authored_at` because
    /// when the same `(plan_id, source_id)` has multiple versions
    /// (re-authoring), the newest is the one we want.
    ///
    /// Iteration is manual rather than `query_map` for the same
    /// reason `recent_research_plans_by_status` does it manually:
    /// the row-mapper returns `crate::Result`, and `query_map`'s
    /// closure must return `duckdb::Result`. Here the row mapper
    /// is infallible at the column level, but the cast pattern is
    /// kept to match the rest of the storage crate.
    pub fn recipes_for_plan(&self, plan_id: Uuid) -> Result<Vec<StoredRecipe>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, dedup_key, plan_id, source_id, source_url,
                        extraction, produces, authored_at, authored_by, version,
                        static_payload, authored_from, prior_recipe_id, reauthor_reason,
                        iterator
                 FROM recipes
                 WHERE plan_id = ?
                 ORDER BY authored_at DESC, version DESC",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt.query(params![plan_id]).map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            out.push(row_to_stored(row)?);
        }
        Ok(out)
    }

    /// Walk the lineage chain for a recipe — the row itself, then its
    /// `prior_recipe_id` predecessor, then that row's predecessor, and
    /// so on until the chain head (a row whose `prior_recipe_id` is
    /// `None`). ADR 0012 §"Storage: recipe version chain".
    ///
    /// Returns the chain newest-to-oldest, with `recipe_id` itself in
    /// position 0. An empty Vec means `recipe_id` doesn't exist; a
    /// single-element Vec means the recipe has no prior (chain head).
    ///
    /// The chain depth is bounded by [`MAX_RECIPE_LINEAGE_DEPTH`].
    /// Hitting the cap is a hard error, not silent truncation — a
    /// real lineage that long would mean the operator has re-authored
    /// the same source 32 times in one plan, which is past the
    /// "something is structurally wrong" bar (ADR 0012 §"Frontier LLM
    /// pushback discipline" caps useful retries at 2). A *cycle*
    /// (which the executor would never produce but a hand-edit could)
    /// also trips this cap rather than infinite-looping.
    pub fn recipe_lineage(&self, recipe_id: Uuid) -> Result<Vec<StoredRecipe>> {
        let mut chain = Vec::new();
        let mut cursor = Some(recipe_id);
        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

        while let Some(id) = cursor {
            if !seen.insert(id) {
                // Cycle. The executor cannot produce one, so reaching
                // here means a hand-edit. Refuse to walk further; the
                // caller surfaces the error.
                return Err(StorageError::Other(format!(
                    "recipe lineage contains a cycle at id {id}"
                )));
            }
            if chain.len() >= MAX_RECIPE_LINEAGE_DEPTH {
                return Err(StorageError::Other(format!(
                    "recipe lineage exceeds depth cap of {MAX_RECIPE_LINEAGE_DEPTH} \
                     (start id: {recipe_id})"
                )));
            }
            match self.get_recipe(id)? {
                Some(stored) => {
                    cursor = stored.prior_recipe_id;
                    chain.push(stored);
                }
                None => {
                    // The pointer references a row that doesn't
                    // exist. This is a soft inconsistency — could
                    // happen if a manual cleanup deleted the prior
                    // row without unlinking the pointer — but it's
                    // not catastrophic: we return what we have so
                    // far and let the caller decide. An empty `chain`
                    // (recipe_id itself missing) is still the right
                    // signal for "not found" at the entry point.
                    break;
                }
            }
        }

        Ok(chain)
    }
}

/// Maximum number of nodes in a recipe lineage chain before
/// [`Store::recipe_lineage`] returns an error. ADR 0012 §"Executor
/// retry loop" caps automated retries at 2 (so 3 total nodes per
/// chain in the eventual automated path); the manual path has no
/// cap but a real chain longer than 32 implies the operator is
/// trapped in oscillation and should stop. The number is deliberately
/// generous — more than the manual path will plausibly produce —
/// while still small enough to bound a pathological cycle's runtime.
pub const MAX_RECIPE_LINEAGE_DEPTH: usize = 32;

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredRecipe> {
    Ok(StoredRecipe {
        id: row.get(0).map_err(StorageError::DuckDb)?,
        dedup_key: row.get(1).map_err(StorageError::DuckDb)?,
        plan_id: row.get(2).map_err(StorageError::DuckDb)?,
        source_id: row.get(3).map_err(StorageError::DuckDb)?,
        source_url: row.get(4).map_err(StorageError::DuckDb)?,
        extraction_json: row.get(5).map_err(StorageError::DuckDb)?,
        produces_json: row.get(6).map_err(StorageError::DuckDb)?,
        authored_at: row.get(7).map_err(StorageError::DuckDb)?,
        authored_by: row.get(8).map_err(StorageError::DuckDb)?,
        version: {
            let v: i64 = row.get(9).map_err(StorageError::DuckDb)?;
            v as u32
        },
        static_payload: row.get(10).map_err(StorageError::DuckDb)?,
        // ADR 0014: NULL on disk → AuthoredFrom::Unknown. Recipes
        // written before migration v10 carry NULL; we coerce
        // honestly rather than guess `FetchedBytes` or
        // `StubExcerpt`. New recipes always carry a populated
        // value (`fetched_bytes` or `stub_excerpt`), set by the
        // executor. An *unknown string* in the column (e.g. a
        // hand-written DB edit) surfaces as a hard error rather
        // than silent demotion to Unknown — the exact-string
        // contract is part of the load-bearing invariant.
        authored_from: {
            let raw: Option<String> = row.get(11).map_err(StorageError::DuckDb)?;
            match raw {
                None => AuthoredFrom::Unknown,
                Some(s) => AuthoredFrom::from_str(&s)?,
            }
        },
        // ADR 0012 §"Storage: recipe version chain": NULL → None.
        // Recipes authored before migration v11 carry NULL; new
        // first-authored recipes also carry NULL (they have no
        // prior). Only re-authored recipes (Track A, Session 26)
        // carry Some(prior.id). The duckdb crate's UUID converter
        // handles `Option<Uuid>` directly — no string round-trip
        // needed, unlike `authored_from` whose closed enum lives
        // in Rust.
        prior_recipe_id: row.get(12).map_err(StorageError::DuckDb)?,
        // Track A, Session 25/26: reauthor_reason travels alongside
        // prior_recipe_id. NULL → None for first-authored recipes;
        // re-authored rows carry Some(failure_msg + operator_note).
        reauthor_reason: row.get(13).map_err(StorageError::DuckDb)?,
        // ADR 0016: NULL on disk → None. Pre-Session-38 recipes
        // and post-Session-38 recipes against single-instance URLs
        // both carry NULL; only iterator-bearing recipes (against
        // listing-shaped sources) carry Some(json). The typed
        // pipeline crate (`recipes_store::stored_to_recipe`)
        // parses the inner JSON into a typed
        // `Option<ExtractionSpec>` at the boundary; storage stays
        // string-typed.
        iterator: row.get(14).map_err(StorageError::DuckDb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_row(id: Uuid) -> RecipeRow {
        RecipeRow {
            id,
            dedup_key: Some("plan_abc:world_bank:pop_total".into()),
            plan_id: Uuid::now_v7(),
            source_id: "world_bank_indicators".into(),
            source_url: "https://api.worldbank.org/v2/country/CL/indicator/SP.POP.TOTL?format=json&date=2022".into(),
            extraction_json: r#"{"mode":"json_path","path":"$[1][0].value"}"#.into(),
            produces_json: r#"[{"record_type":"observation","expectation":{"list":"observation_metric","index":0},"field_mappings":[{"path":"value","source":{"kind":"extracted"}}]}]"#.into(),
            authored_at: Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: tests that don't otherwise care about
            // authoring provenance use FetchedBytes — the optimistic
            // case, the one most code paths take in production. New
            // tests below pin StubExcerpt and Unknown explicitly.
            authored_from: AuthoredFrom::FetchedBytes,
            // ADR 0012: tests that don't exercise re-authoring leave
            // `prior_recipe_id` at None — the chain head, the shape of
            // a first-authored recipe. The new lineage tests below
            // explicitly populate Some(prior).
            prior_recipe_id: None,
            // Track A: paired with prior_recipe_id. None for
            // first-authored recipes. Re-author tests populate both.
            reauthor_reason: None,
            // ADR 0016: tests that don't exercise iteration leave
            // `iterator` at None — the scalar-recipe shape, the
            // pre-ADR-0016 contract. The new iterator round-trip
            // test below explicitly populates Some(json).
            iterator: None,
        }
    }

    #[test]
    fn recipe_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id);
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(got.id, id);
        assert_eq!(got.source_id, "world_bank_indicators");
        assert_eq!(got.version, 1);
        assert!(got.extraction_json.contains("json_path"));
    }

    #[test]
    fn recipe_lookup_by_dedup_key() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id);
        store.insert_recipe(&row).unwrap();

        let got = store
            .get_recipe_by_dedup_key("plan_abc:world_bank:pop_total")
            .unwrap()
            .expect("row should exist");
        assert_eq!(got.id, id);
    }

    #[test]
    fn recipe_lookup_by_dedup_key_picks_highest_version() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut v1 = sample_row(Uuid::now_v7());
        v1.version = 1;
        let mut v2 = sample_row(Uuid::now_v7());
        v2.version = 2;
        store.insert_recipe(&v1).unwrap();
        store.insert_recipe(&v2).unwrap();

        let got = store
            .get_recipe_by_dedup_key("plan_abc:world_bank:pop_total")
            .unwrap()
            .expect("row should exist");
        assert_eq!(got.version, 2);
    }

    #[test]
    fn get_recipe_returns_none_for_missing_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        assert!(store.get_recipe(Uuid::now_v7()).unwrap().is_none());
    }

    #[test]
    fn count_recipes_is_zero_initially() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        assert_eq!(store.count_recipes().unwrap(), 0);
    }

    // -----------------------------------------------------------------
    // Session 8 — recipes_for_plan
    // -----------------------------------------------------------------

    #[test]
    fn recipes_for_plan_returns_only_matching_plan() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();

        let mut a1 = sample_row(Uuid::now_v7());
        a1.plan_id = plan_a;
        a1.dedup_key = Some("a1".into());
        store.insert_recipe(&a1).unwrap();

        let mut a2 = sample_row(Uuid::now_v7());
        a2.plan_id = plan_a;
        a2.dedup_key = Some("a2".into());
        store.insert_recipe(&a2).unwrap();

        let mut b1 = sample_row(Uuid::now_v7());
        b1.plan_id = plan_b;
        b1.dedup_key = Some("b1".into());
        store.insert_recipe(&b1).unwrap();

        let for_a = store.recipes_for_plan(plan_a).unwrap();
        assert_eq!(for_a.len(), 2);
        assert!(for_a.iter().all(|r| r.plan_id == plan_a));

        let for_b = store.recipes_for_plan(plan_b).unwrap();
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_b[0].plan_id, plan_b);
    }

    #[test]
    fn recipes_for_plan_returns_empty_when_no_recipes_yet() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let recipes = store.recipes_for_plan(Uuid::now_v7()).unwrap();
        assert!(recipes.is_empty());
    }

    // -----------------------------------------------------------------
    // Session 18 — static_payload field (ADR 0007 Amendment 3)
    // -----------------------------------------------------------------

    /// Default shape: a recipe authored without baked bytes round-trips
    /// with `static_payload: None`. Migration 0008 made the column
    /// nullable so this is the on-disk shape for HTML-addressable
    /// recipes (the common case).
    #[test]
    fn recipe_round_trips_with_no_static_payload() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id); // sample_row sets static_payload: None
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert!(got.static_payload.is_none(),
            "expected None for unbaked recipe, got {:?}", got.static_payload);
    }

    /// Bake-time-frozen shape: a recipe with a JSON payload round-trips
    /// the bytes verbatim. The runtime serves these to extraction in
    /// place of an HTTP fetch (`fetch_executor` short-circuit; ADR
    /// 0007 Amendment 3 §"Runtime path").
    #[test]
    fn recipe_round_trips_with_static_payload() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let mut row = sample_row(id);
        let payload = r#"{"date":"2026-03-26","rate":"6.50","direction":"hold"}"#;
        row.static_payload = Some(payload.into());
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(got.static_payload.as_deref(), Some(payload));
    }

    /// `recipes_for_plan` (the executor's primary read path) carries
    /// the field through. Without this guarantee the executor would
    /// read NULL for every payload-bearing recipe and skip the
    /// short-circuit.
    #[test]
    fn recipes_for_plan_carries_static_payload_through() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut unbaked = sample_row(Uuid::now_v7());
        unbaked.plan_id = plan_id;
        unbaked.dedup_key = Some("unbaked".into());
        store.insert_recipe(&unbaked).unwrap();

        let mut baked = sample_row(Uuid::now_v7());
        baked.plan_id = plan_id;
        baked.dedup_key = Some("baked".into());
        baked.static_payload = Some(r#"{"x":1}"#.into());
        store.insert_recipe(&baked).unwrap();

        let recipes = store.recipes_for_plan(plan_id).unwrap();
        assert_eq!(recipes.len(), 2);
        // Order is `authored_at DESC, version DESC`; both rows share
        // both. Match by dedup_key rather than position.
        let unbaked_back = recipes.iter()
            .find(|r| r.dedup_key.as_deref() == Some("unbaked"))
            .expect("unbaked recipe present");
        let baked_back = recipes.iter()
            .find(|r| r.dedup_key.as_deref() == Some("baked"))
            .expect("baked recipe present");
        assert!(unbaked_back.static_payload.is_none());
        assert_eq!(baked_back.static_payload.as_deref(), Some(r#"{"x":1}"#));
    }

    // -----------------------------------------------------------------
    // Session 21 — authored_from field (ADR 0014)
    // -----------------------------------------------------------------

    /// Wire-form discipline mirrors `PlanStatus`: each variant has a
    /// fixed lowercase string. Round-trip through serde and through
    /// `as_str` / `from_str` must agree byte-for-byte. A drift here
    /// silently corrupts every authored_from column read; pinning
    /// the strings in a test is cheap insurance.
    #[test]
    fn authored_from_strings_are_stable() {
        assert_eq!(AuthoredFrom::FetchedBytes.as_str(), "fetched_bytes");
        assert_eq!(AuthoredFrom::StubExcerpt.as_str(), "stub_excerpt");
        assert_eq!(AuthoredFrom::Unknown.as_str(), "unknown");

        for v in [
            AuthoredFrom::FetchedBytes,
            AuthoredFrom::StubExcerpt,
            AuthoredFrom::Unknown,
        ] {
            // FromStr round-trip
            let parsed: AuthoredFrom = v.as_str().parse().unwrap();
            assert_eq!(parsed, v);
            // Serde round-trip
            let json = serde_json::to_string(&v).unwrap();
            let back: AuthoredFrom = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v);
        }
    }

    /// `AuthoredFrom::default() == Unknown` keeps backward-compat
    /// with code paths that don't think about provenance — the
    /// load path coerces NULL → Unknown using this default. If a
    /// future contributor adds a variant and accidentally bumps
    /// the default, this test catches the drift before any row
    /// reads the wrong value.
    #[test]
    fn authored_from_defaults_to_unknown() {
        assert_eq!(AuthoredFrom::default(), AuthoredFrom::Unknown);
    }

    /// An unknown column value is a hard error, not a silent
    /// demotion to Unknown. Reasoning: `Unknown` means "we never
    /// asked the executor about this row" — a NULL pre-v10 row.
    /// An *unrecognized non-null string* (a hand-written DB edit, a
    /// future variant added without updating this code) is a
    /// genuine inconsistency the load path should refuse rather
    /// than absorb. Same posture as PlanStatus's strict FromStr.
    #[test]
    fn authored_from_from_str_rejects_unknown_variant() {
        let err = AuthoredFrom::from_str("not_a_real_variant").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not_a_real_variant"),
            "error should name the offending value; got {msg}"
        );
    }

    /// FetchedBytes round-trips through storage. The optimistic
    /// case: a recipe authored against the source's actual response.
    #[test]
    fn recipe_roundtrips_fetched_bytes_authored_from() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let mut row = sample_row(id);
        row.authored_from = AuthoredFrom::FetchedBytes;
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(got.authored_from, AuthoredFrom::FetchedBytes);
    }

    /// StubExcerpt round-trips through storage. The motivating case
    /// for ADR 0014: a recipe authored from a fallback description
    /// (no endpoint_hint, unparseable hint, or pre-fetch failure).
    /// The chip in the UI surfaces this exact value.
    #[test]
    fn recipe_roundtrips_stub_excerpt_authored_from() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let mut row = sample_row(id);
        row.authored_from = AuthoredFrom::StubExcerpt;
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(got.authored_from, AuthoredFrom::StubExcerpt);
    }

    /// Pre-v10 rows (no `authored_from` column at insert time) load
    /// as `Unknown`. Simulates the legacy state by writing a row
    /// then nulling the column directly via raw SQL — duckdb's
    /// nullable-column shape lets this exercise the load path's
    /// NULL → Unknown coercion against actual NULL on disk, not a
    /// Rust-side faked None.
    #[test]
    fn recipe_with_null_authored_from_loads_as_unknown() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id);
        store.insert_recipe(&row).unwrap();

        // Directly NULL the column to simulate a pre-v10 row. Real
        // pre-v10 rows would carry NULL because the column didn't
        // exist when they were written; v10's ALTER added the
        // column nullable, so reads against those rows return NULL.
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE recipes SET authored_from = NULL WHERE id = ?",
                params![id],
            )
            .unwrap();
        }

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(
            got.authored_from,
            AuthoredFrom::Unknown,
            "NULL on disk must coerce to Unknown, not silently to FetchedBytes"
        );
    }

    /// `recipes_for_plan` (the executor's primary read path) carries
    /// the field through. Without this guarantee the executor would
    /// see Unknown for every recipe — the chip would never appear,
    /// and the deferred option-3 self-healing trigger would never
    /// fire. Mirrors the static_payload equivalent above.
    #[test]
    fn recipes_for_plan_carries_authored_from_through() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut fetched = sample_row(Uuid::now_v7());
        fetched.plan_id = plan_id;
        fetched.dedup_key = Some("fetched".into());
        fetched.authored_from = AuthoredFrom::FetchedBytes;
        store.insert_recipe(&fetched).unwrap();

        let mut stub = sample_row(Uuid::now_v7());
        stub.plan_id = plan_id;
        stub.dedup_key = Some("stub".into());
        stub.authored_from = AuthoredFrom::StubExcerpt;
        store.insert_recipe(&stub).unwrap();

        let recipes = store.recipes_for_plan(plan_id).unwrap();
        assert_eq!(recipes.len(), 2);
        let fetched_back = recipes
            .iter()
            .find(|r| r.dedup_key.as_deref() == Some("fetched"))
            .expect("fetched recipe present");
        let stub_back = recipes
            .iter()
            .find(|r| r.dedup_key.as_deref() == Some("stub"))
            .expect("stub recipe present");
        assert_eq!(fetched_back.authored_from, AuthoredFrom::FetchedBytes);
        assert_eq!(stub_back.authored_from, AuthoredFrom::StubExcerpt);
    }

    // -----------------------------------------------------------------
    // Session 26 / Track A — prior_recipe_id and lineage walk
    // -----------------------------------------------------------------

    /// Default shape: a first-authored recipe carries `None` for
    /// `prior_recipe_id`. Migration v11 made the column nullable; the
    /// load path coerces NULL → None. This is the on-disk shape for
    /// every recipe authored before Session 26 and for every fresh
    /// (non-re-authored) recipe afterward.
    #[test]
    fn recipe_round_trips_with_no_prior_recipe_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id); // sample_row sets prior_recipe_id: None
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert!(
            got.prior_recipe_id.is_none(),
            "expected None for first-authored recipe, got {:?}",
            got.prior_recipe_id
        );
    }

    /// Re-authored shape: a recipe with `Some(prior.id)` round-trips
    /// the pointer verbatim. Track A's manual re-author UI lands the
    /// new recipe with this field populated; the storage layer must
    /// preserve it for `recipe_lineage` to walk.
    #[test]
    fn recipe_round_trips_with_prior_recipe_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let prior_id = Uuid::now_v7();
        let prior = sample_row(prior_id);
        store.insert_recipe(&prior).unwrap();

        let new_id = Uuid::now_v7();
        let mut new_row = sample_row(new_id);
        new_row.dedup_key = Some("re_authored".into());
        new_row.version = 2;
        new_row.prior_recipe_id = Some(prior_id);
        store.insert_recipe(&new_row).unwrap();

        let got = store.get_recipe(new_id).unwrap().expect("row should exist");
        assert_eq!(got.prior_recipe_id, Some(prior_id));
    }

    /// `recipes_for_plan` carries the field through. Without this
    /// guarantee the inspection panel would never see lineage even
    /// when it exists in the DB. Mirrors the static_payload and
    /// authored_from equivalents.
    #[test]
    fn recipes_for_plan_carries_prior_recipe_id_through() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut head = sample_row(Uuid::now_v7());
        head.plan_id = plan_id;
        head.dedup_key = Some("dk".into());
        head.version = 1;
        let head_id = head.id;
        store.insert_recipe(&head).unwrap();

        let mut tail = sample_row(Uuid::now_v7());
        tail.plan_id = plan_id;
        tail.dedup_key = Some("dk".into());
        tail.version = 2;
        tail.prior_recipe_id = Some(head_id);
        let tail_id = tail.id;
        store.insert_recipe(&tail).unwrap();

        let recipes = store.recipes_for_plan(plan_id).unwrap();
        assert_eq!(recipes.len(), 2);
        let head_back = recipes
            .iter()
            .find(|r| r.id == head_id)
            .expect("head recipe present");
        let tail_back = recipes
            .iter()
            .find(|r| r.id == tail_id)
            .expect("tail recipe present");
        assert_eq!(head_back.prior_recipe_id, None);
        assert_eq!(tail_back.prior_recipe_id, Some(head_id));
    }

    /// `recipe_lineage` on a missing id returns an empty Vec, not an
    /// error. The chain entry-point is "not found"; the caller
    /// surfaces that as `RecipeNotFound`. This mirrors the existing
    /// `get_recipe(missing)` shape (`Ok(None)`) — same posture: not
    /// finding something is not an error condition.
    #[test]
    fn recipe_lineage_returns_empty_for_missing_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let chain = store.recipe_lineage(Uuid::now_v7()).unwrap();
        assert!(chain.is_empty());
    }

    /// `recipe_lineage` on a chain head returns a single-element Vec.
    /// This is the steady-state shape for any recipe authored without
    /// re-authoring — the most common case in production.
    #[test]
    fn recipe_lineage_returns_one_for_chain_head() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id);
        store.insert_recipe(&row).unwrap();

        let chain = store.recipe_lineage(id).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, id);
        assert!(chain[0].prior_recipe_id.is_none());
    }

    /// `recipe_lineage` walks a 3-deep chain newest-to-oldest. The
    /// canonical Track A scenario: operator re-authored a recipe
    /// twice. Each row points one hop back; the walk terminates at
    /// the chain head whose `prior_recipe_id` is `None`.
    #[test]
    fn recipe_lineage_walks_three_deep_chain() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let v1_id = Uuid::now_v7();
        let v1 = sample_row(v1_id);
        store.insert_recipe(&v1).unwrap();

        let v2_id = Uuid::now_v7();
        let mut v2 = sample_row(v2_id);
        v2.dedup_key = Some("dk".into());
        v2.version = 2;
        v2.prior_recipe_id = Some(v1_id);
        store.insert_recipe(&v2).unwrap();

        let v3_id = Uuid::now_v7();
        let mut v3 = sample_row(v3_id);
        v3.dedup_key = Some("dk".into());
        v3.version = 3;
        v3.prior_recipe_id = Some(v2_id);
        store.insert_recipe(&v3).unwrap();

        let chain = store.recipe_lineage(v3_id).unwrap();
        assert_eq!(chain.len(), 3, "chain should be 3 nodes");
        assert_eq!(chain[0].id, v3_id);
        assert_eq!(chain[1].id, v2_id);
        assert_eq!(chain[2].id, v1_id);
        // The head of the chain has no prior.
        assert!(chain[2].prior_recipe_id.is_none());
    }

    /// `recipe_lineage` detects cycles. The executor cannot produce
    /// one — re-authoring always points at an existing earlier UUIDv7,
    /// and v7 ids are time-monotonic — but a hand-edit could. The
    /// walker refuses to loop and returns an error naming the
    /// offending id; better to surface the inconsistency than spin
    /// forever or silently truncate.
    #[test]
    fn recipe_lineage_rejects_cycle() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let a_id = Uuid::now_v7();
        let b_id = Uuid::now_v7();

        // Insert a normally, then b pointing at a.
        let a = sample_row(a_id);
        store.insert_recipe(&a).unwrap();

        let mut b = sample_row(b_id);
        b.dedup_key = Some("dk_b".into());
        b.prior_recipe_id = Some(a_id);
        store.insert_recipe(&b).unwrap();

        // Now hand-rewire a to point at b — closing the cycle. This
        // is not reachable through the executor; we synthesize the
        // condition with a raw UPDATE to verify the walker's defense
        // against pathological data.
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE recipes SET prior_recipe_id = ? WHERE id = ?",
                params![b_id, a_id],
            )
            .unwrap();
        }

        let err = store.recipe_lineage(a_id).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle"),
            "error should name the cycle; got {msg}"
        );
    }

    /// Reaching the depth cap surfaces as a hard error, not silent
    /// truncation. A pathological lineage longer than the cap means
    /// the operator has re-authored the same source past the
    /// "something is structurally wrong" bar (ADR 0012 §"Frontier
    /// LLM pushback discipline" caps useful retries at 2) — refusing
    /// to walk further is the correct signal.
    #[test]
    fn recipe_lineage_caps_at_max_depth() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // Build a chain MAX_RECIPE_LINEAGE_DEPTH + 1 nodes long. Each
        // row points at the prior id; the head has prior_recipe_id =
        // None. The walker should refuse to traverse more than
        // MAX_RECIPE_LINEAGE_DEPTH nodes.
        let mut prior: Option<Uuid> = None;
        let mut last: Option<Uuid> = None;
        for i in 0..=MAX_RECIPE_LINEAGE_DEPTH {
            let id = Uuid::now_v7();
            let mut row = sample_row(id);
            row.dedup_key = Some(format!("dk_{i}"));
            row.version = (i as u32) + 1;
            row.prior_recipe_id = prior;
            store.insert_recipe(&row).unwrap();
            prior = Some(id);
            last = Some(id);
        }

        let err = store.recipe_lineage(last.unwrap()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("depth cap"),
            "error should mention the depth cap; got {msg}"
        );
    }

    /// Pre-v11 rows (no `prior_recipe_id` column at insert time) load
    /// as `None`. Simulates the legacy state by writing a row then
    /// nulling the column directly via raw SQL — duckdb's
    /// nullable-column shape lets this exercise the load path's
    /// NULL → None coercion against actual NULL on disk.
    #[test]
    fn recipe_with_null_prior_recipe_id_loads_as_none() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let mut row = sample_row(id);
        // Pretend the executor wrote a Some, then a manual edit
        // nulled it. The storage layer must coerce that NULL to None
        // honestly (the chain head shape) rather than retain stale
        // state from the struct.
        row.prior_recipe_id = Some(Uuid::now_v7());
        store.insert_recipe(&row).unwrap();

        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE recipes SET prior_recipe_id = NULL WHERE id = ?",
                params![id],
            )
            .unwrap();
        }

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert!(
            got.prior_recipe_id.is_none(),
            "NULL on disk must coerce to None"
        );
    }

    // -----------------------------------------------------------------
    // Track A, Session 25/26 — reauthor_reason
    // -----------------------------------------------------------------

    /// Default shape: a first-authored recipe carries `None` for
    /// `reauthor_reason`. Migration v12 made the column nullable; the
    /// load path coerces NULL → None. Mirrors the prior_recipe_id
    /// equivalent.
    #[test]
    fn recipe_round_trips_with_no_reauthor_reason() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id);
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert!(
            got.reauthor_reason.is_none(),
            "expected None for first-authored recipe, got {:?}",
            got.reauthor_reason
        );
    }

    /// Re-authored shape: a recipe with `Some(reason)` round-trips the
    /// reason verbatim. Track A's manual re-author UI lands the new
    /// recipe with both `prior_recipe_id` and `reauthor_reason`
    /// populated; the storage layer must preserve the reason for the
    /// inspection panel and any future audit query.
    #[test]
    fn recipe_round_trips_with_reauthor_reason() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let prior_id = Uuid::now_v7();
        let prior = sample_row(prior_id);
        store.insert_recipe(&prior).unwrap();

        let new_id = Uuid::now_v7();
        let mut new_row = sample_row(new_id);
        new_row.dedup_key = Some("re_authored".into());
        new_row.version = 2;
        new_row.prior_recipe_id = Some(prior_id);
        let reason =
            "extraction [regex_capture]: pattern matched nothing\n\
             operator note: BBC RSS does not wrap title in CDATA";
        new_row.reauthor_reason = Some(reason.into());
        store.insert_recipe(&new_row).unwrap();

        let got = store.get_recipe(new_id).unwrap().expect("row should exist");
        assert_eq!(got.reauthor_reason.as_deref(), Some(reason));
        assert_eq!(got.prior_recipe_id, Some(prior_id));
    }

    // -----------------------------------------------------------------
    // Session 38 — iterator field (ADR 0016)
    // -----------------------------------------------------------------

    /// A recipe without an iterator (the pre-ADR-0016 contract: scalar
    /// recipe, one record per fetch) reads back with `iterator: None`.
    /// `sample_row` already sets the field to None; this test pins
    /// the round-trip so a future migration that accidentally writes
    /// a non-NULL default would fail visibly here.
    #[test]
    fn recipe_round_trips_with_no_iterator() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let row = sample_row(id); // sample_row sets iterator: None
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert!(
            got.iterator.is_none(),
            "expected None for scalar recipe, got {:?}",
            got.iterator
        );
    }

    /// Iterator-bearing shape: a recipe that carries an iterator JSON
    /// round-trips it verbatim. The runtime parses the JSON into a
    /// typed `ExtractionSpec` at the pipeline boundary
    /// (`recipes_store::stored_to_recipe`); storage stays string-typed.
    #[test]
    fn recipe_round_trips_with_iterator() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let id = Uuid::now_v7();
        let mut row = sample_row(id);
        // The iterator is a serialized ExtractionSpec — same shape as
        // `extraction_json`, but evaluated at iterator position. Here
        // we exercise css_select because that's the Phase 1 mode (ADR
        // 0016 §"Per-match evaluation semantics, by mode"); the
        // storage layer is mode-agnostic and would persist any
        // valid ExtractionSpec JSON identically.
        let iter_json = r#"{"mode":"css_select","selector":".c-card"}"#;
        row.iterator = Some(iter_json.into());
        store.insert_recipe(&row).unwrap();

        let got = store.get_recipe(id).unwrap().expect("row should exist");
        assert_eq!(got.iterator.as_deref(), Some(iter_json));
    }

    /// `recipes_for_plan` (the executor's primary read path) carries
    /// the iterator through. Without this guarantee the executor
    /// would read NULL for every iterator-bearing recipe and silently
    /// fall back to scalar-recipe semantics — exactly the bug ADR
    /// 0016 fixes, re-introduced from a different angle.
    #[test]
    fn recipes_for_plan_carries_iterator_through() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut scalar = sample_row(Uuid::now_v7());
        scalar.plan_id = plan_id;
        scalar.dedup_key = Some("scalar".into());
        store.insert_recipe(&scalar).unwrap();

        let mut iterating = sample_row(Uuid::now_v7());
        iterating.plan_id = plan_id;
        iterating.dedup_key = Some("iterating".into());
        let iter_json = r#"{"mode":"css_select","selector":".c-card"}"#;
        iterating.iterator = Some(iter_json.into());
        store.insert_recipe(&iterating).unwrap();

        let recipes = store.recipes_for_plan(plan_id).unwrap();
        assert_eq!(recipes.len(), 2);
        let scalar_back = recipes
            .iter()
            .find(|r| r.dedup_key.as_deref() == Some("scalar"))
            .expect("scalar recipe present");
        let iterating_back = recipes
            .iter()
            .find(|r| r.dedup_key.as_deref() == Some("iterating"))
            .expect("iterating recipe present");
        assert!(scalar_back.iterator.is_none());
        assert_eq!(iterating_back.iterator.as_deref(), Some(iter_json));
    }
}
