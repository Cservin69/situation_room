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

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

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
                extraction, produces, authored_at, authored_by, version
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                        extraction, produces, authored_at, authored_by, version
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
                        extraction, produces, authored_at, authored_by, version
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
                        extraction, produces, authored_at, authored_by, version
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
}
