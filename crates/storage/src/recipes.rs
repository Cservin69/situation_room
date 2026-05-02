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
    /// Where the recipe-author prompt's document excerpt came from
    /// (real bytes vs. stub). See [`AuthoredFrom`] and ADR 0014.
    /// New recipes always carry `FetchedBytes` or `StubExcerpt`;
    /// `Unknown` is reserved for recipes whose authoring predates
    /// migration v10.
    pub authored_from: AuthoredFrom,
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
    /// Authoring provenance — see [`AuthoredFrom`] and ADR 0014.
    /// Reads NULL → `Unknown` for recipes authored before migration
    /// v10. Reads the recorded variant otherwise.
    pub authored_from: AuthoredFrom,
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
                static_payload, authored_from
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                        static_payload, authored_from
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
                        static_payload, authored_from
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
                        static_payload, authored_from
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
}

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
}
