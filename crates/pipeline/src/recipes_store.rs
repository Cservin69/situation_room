//! Typed recipe storage helper.
//!
//! Thin marshalling layer between [`FetchRecipe`] and the storage
//! crate's [`RecipeRow`]. Storage stays agnostic of the typed recipe
//! shape (pipeline owns the types); this module is the single place
//! the conversion lives.
//!
//! ## Why this indirection
//!
//! [`storage`] is load-bearing across the app and mustn't reverse-
//! depend on [`pipeline`]. Storage therefore accepts recipes as
//! pre-serialized JSON plus the scalar columns it indexes. This
//! module does that serialization once per save/load and hands
//! callers typed values.

use crate::recipes::{ExtractionSpec, FetchRecipe, ProductionBinding};
use situation_room_storage::{RecipeRow, Result as StorageResult, StoredRecipe, Store};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecipeStoreError {
    #[error("storage error: {0}")]
    Storage(#[from] situation_room_storage::StorageError),

    #[error("recipe serialization failed: {0}")]
    Serialize(String),

    #[error("recipe deserialization failed: {0}")]
    Deserialize(String),
}

/// Persist a typed [`FetchRecipe`] to storage.
pub fn save_recipe(store: &Store, recipe: &FetchRecipe) -> Result<(), RecipeStoreError> {
    let row = recipe_to_row(recipe)?;
    store.insert_recipe(&row).map_err(RecipeStoreError::Storage)
}

/// Look up a recipe by id. Returns `Ok(None)` if not present.
pub fn load_recipe(
    store: &Store,
    id: uuid::Uuid,
) -> Result<Option<FetchRecipe>, RecipeStoreError> {
    let stored: Option<StoredRecipe> = store.get_recipe(id).map_err(RecipeStoreError::Storage)?;
    stored.map(stored_to_recipe).transpose()
}

/// Look up the highest-version recipe for a dedup_key. Returns
/// `Ok(None)` if none present.
pub fn load_recipe_by_dedup_key(
    store: &Store,
    dedup_key: &str,
) -> Result<Option<FetchRecipe>, RecipeStoreError> {
    let stored: Option<StoredRecipe> = store
        .get_recipe_by_dedup_key(dedup_key)
        .map_err(RecipeStoreError::Storage)?;
    stored.map(stored_to_recipe).transpose()
}

/// Load every recipe authored against a plan, deserialized into
/// typed [`FetchRecipe`]s. Newest first (matching
/// [`situation_room_storage::Store::recipes_for_plan`]'s ordering).
///
/// Used by the fetch executor to decide whether Level-2 authoring
/// needs to run for a plan: a non-empty result means recipes already
/// exist and authoring is skipped (ADR 0007 §"runtime path").
pub fn load_recipes_for_plan(
    store: &Store,
    plan_id: uuid::Uuid,
) -> Result<Vec<FetchRecipe>, RecipeStoreError> {
    let stored = store
        .recipes_for_plan(plan_id)
        .map_err(RecipeStoreError::Storage)?;
    stored.into_iter().map(stored_to_recipe).collect()
}

/// Load only the **latest** recipe per `source_id` for a given plan.
/// Track A, Session 26 — manual re-author UI.
///
/// Storage's [`recipes_for_plan`](situation_room_storage::Store::recipes_for_plan)
/// returns every version of every recipe for the plan, ordered
/// `authored_at DESC, version DESC`. After re-authoring, that yields
/// 2+ rows per source — the executor would fetch and apply each
/// version independently, doing duplicate work and producing
/// duplicate records.
///
/// This loader collapses to one row per `source_id`: the first row
/// the storage layer returns for each id wins (which, given the
/// ORDER BY, is the highest authored_at + version — the latest).
/// Sources without any recipes are absent from the result, same as
/// [`load_recipes_for_plan`].
///
/// Order preservation: the storage layer returns rows in
/// `authored_at DESC` order, so sources with the most recent
/// re-authoring activity come first. This matches the operator's
/// mental model — newly re-authored sources are the ones whose
/// behaviour is most likely to differ on the next run.
///
/// Why filter in Rust rather than SQL: DuckDB's `ROW_NUMBER() OVER
/// (PARTITION BY source_id ORDER BY ...)` would do the same job in
/// one query, but the in-Rust filter keeps the storage layer's
/// query surface narrow (one method, one shape) and lets the
/// pipeline crate own the "latest per source" semantics. Storage
/// stays the persistence layer; pipeline owns invariants over the
/// data.
pub fn load_latest_recipes_for_plan(
    store: &Store,
    plan_id: uuid::Uuid,
) -> Result<Vec<FetchRecipe>, RecipeStoreError> {
    let stored = store
        .recipes_for_plan(plan_id)
        .map_err(RecipeStoreError::Storage)?;
    let mut seen_sources: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut latest = Vec::new();
    for row in stored {
        if seen_sources.insert(row.source_id.clone()) {
            latest.push(stored_to_recipe(row)?);
        }
    }
    Ok(latest)
}

fn recipe_to_row(r: &FetchRecipe) -> Result<RecipeRow, RecipeStoreError> {
    let extraction_json = serde_json::to_string(&r.extraction)
        .map_err(|e| RecipeStoreError::Serialize(format!("extraction: {e}")))?;
    let produces_json = serde_json::to_string(&r.produces)
        .map_err(|e| RecipeStoreError::Serialize(format!("produces: {e}")))?;

    Ok(RecipeRow {
        id: r.id,
        dedup_key: r.dedup_key.clone(),
        plan_id: r.plan_id,
        source_id: r.source_id.clone(),
        source_url: r.source_url.to_string(),
        extraction_json,
        produces_json,
        authored_at: r.authored_at,
        authored_by: r.authored_by.clone(),
        version: r.version,
        static_payload: r.static_payload.clone(),
        // ADR 0014: thread the provenance signal through. The
        // executor stamps the value on the typed FetchRecipe
        // before save_recipe is called; storage just persists it.
        authored_from: r.authored_from,
        // ADR 0012: thread the lineage pointer through. The
        // re-author entry point stamps the value on the typed
        // FetchRecipe before save_recipe is called; storage
        // just persists it.
        prior_recipe_id: r.prior_recipe_id,
        // Track A, Session 25/26: travels with prior_recipe_id.
        reauthor_reason: r.reauthor_reason.clone(),
    })
}

fn stored_to_recipe(s: StoredRecipe) -> Result<FetchRecipe, RecipeStoreError> {
    let extraction: ExtractionSpec = serde_json::from_str(&s.extraction_json)
        .map_err(|e| RecipeStoreError::Deserialize(format!("extraction: {e}")))?;
    let produces: Vec<ProductionBinding> = serde_json::from_str(&s.produces_json)
        .map_err(|e| RecipeStoreError::Deserialize(format!("produces: {e}")))?;
    let source_url = url::Url::parse(&s.source_url)
        .map_err(|e| RecipeStoreError::Deserialize(format!("source_url: {e}")))?;

    Ok(FetchRecipe {
        id: s.id,
        dedup_key: s.dedup_key,
        plan_id: s.plan_id,
        source_id: s.source_id,
        source_url,
        extraction,
        produces,
        authored_at: s.authored_at,
        authored_by: s.authored_by,
        version: s.version,
        static_payload: s.static_payload,
        // ADR 0014: storage's row_to_stored already coerced NULL →
        // Unknown, so legacy rows arrive here with the right value
        // without further work in this layer.
        authored_from: s.authored_from,
        // ADR 0012: storage's row_to_stored already coerced NULL →
        // None, so legacy rows arrive here as chain heads.
        prior_recipe_id: s.prior_recipe_id,
        // Track A, Session 25/26: paired with prior_recipe_id.
        reauthor_reason: s.reauthor_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{
        ExpectationRef, FieldMap, FieldValueSource, ProductionBinding,
    };
    use chrono::{TimeZone, Utc};
    use situation_room_core::RecordType;
    use url::Url;
    use uuid::Uuid;

    fn sample() -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some("dk".into()),
            plan_id: Uuid::now_v7(),
            source_id: "s".into(),
            source_url: Url::parse("https://example.com/x").unwrap(),
            extraction: ExtractionSpec::JsonPath {
                path: "$.a".into(),
            },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![FieldMap {
                    path: "value".into(),
                    source: FieldValueSource::Extracted,
                }],
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: tests in this module focus on JSON-string
            // round-trips and dedup_key lookup; FetchedBytes is the
            // optimistic-case default. Round-trip of authored_from
            // itself is covered in `crates/storage/src/recipes.rs`
            // and `crates/pipeline/src/recipes.rs`.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    #[test]
    fn recipe_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe = sample();
        let id = recipe.id;
        save_recipe(&store, &recipe).unwrap();

        let back = load_recipe(&store, id).unwrap().expect("present");
        assert_eq!(back.id, recipe.id);
        assert_eq!(back.source_id, recipe.source_id);
        assert_eq!(back.version, recipe.version);
        assert_eq!(back.extraction, recipe.extraction);
        assert_eq!(back.produces, recipe.produces);
    }

    #[test]
    fn recipe_roundtrips_via_dedup_key() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe = sample();
        save_recipe(&store, &recipe).unwrap();

        let back = load_recipe_by_dedup_key(&store, "dk")
            .unwrap()
            .expect("present");
        assert_eq!(back.id, recipe.id);
    }

    /// ADR 0014: a StubExcerpt-stamped recipe round-trips through
    /// the typed marshalling layer end-to-end. The two-direction
    /// pass exercises both `recipe_to_row` (FetchRecipe → RecipeRow,
    /// `authored_from` copied) and `stored_to_recipe` (StoredRecipe
    /// → FetchRecipe, value preserved). If either side were to drop
    /// the field the chip in the UI would silently never appear.
    #[test]
    fn recipe_marshals_authored_from_stub_excerpt_through_storage() {
        use situation_room_storage::AuthoredFrom;

        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let mut recipe = sample();
        recipe.authored_from = AuthoredFrom::StubExcerpt;
        let id = recipe.id;
        save_recipe(&store, &recipe).unwrap();

        let back = load_recipe(&store, id).unwrap().expect("present");
        assert_eq!(back.authored_from, AuthoredFrom::StubExcerpt);
    }

    /// ADR 0012 / Track A: `prior_recipe_id` round-trips through the
    /// typed marshalling layer end-to-end. The lineage chip in the UI
    /// depends on this value reaching the wire intact; a silent drop
    /// here would make every re-authored recipe look like a chain
    /// head, defeating the manual-re-author flow's central
    /// affordance.
    #[test]
    fn recipe_marshals_prior_recipe_id_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let prior = sample();
        let prior_id = prior.id;
        save_recipe(&store, &prior).unwrap();

        let mut new_recipe = sample();
        new_recipe.id = Uuid::now_v7();
        new_recipe.dedup_key = Some("re_authored".into());
        new_recipe.version = 2;
        new_recipe.prior_recipe_id = Some(prior_id);
        let new_id = new_recipe.id;
        save_recipe(&store, &new_recipe).unwrap();

        let back = load_recipe(&store, new_id).unwrap().expect("present");
        assert_eq!(back.prior_recipe_id, Some(prior_id));
        assert_eq!(back.version, 2);
    }

    /// `load_latest_recipes_for_plan` returns one recipe per source —
    /// the highest-authored-at row. After re-authoring, the executor
    /// must see only the new version, not both. Without this filter,
    /// fetching would double-process every re-authored source and
    /// produce duplicate records.
    #[test]
    fn load_latest_recipes_for_plan_dedups_to_newest_per_source() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let one_day = chrono::Duration::days(1);
        let base = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();

        // Source A: two versions. Newer wins.
        let mut a_v1 = sample();
        a_v1.id = Uuid::now_v7();
        a_v1.plan_id = plan_id;
        a_v1.source_id = "source_a".into();
        a_v1.dedup_key = Some(format!("{plan_id}:source_a"));
        a_v1.version = 1;
        a_v1.authored_at = base;
        let a_v1_id = a_v1.id;
        save_recipe(&store, &a_v1).unwrap();

        let mut a_v2 = sample();
        a_v2.id = Uuid::now_v7();
        a_v2.plan_id = plan_id;
        a_v2.source_id = "source_a".into();
        a_v2.dedup_key = Some(format!("{plan_id}:source_a"));
        a_v2.version = 2;
        a_v2.authored_at = base + one_day;
        a_v2.prior_recipe_id = Some(a_v1_id);
        let a_v2_id = a_v2.id;
        save_recipe(&store, &a_v2).unwrap();

        // Source B: only one version.
        let mut b_v1 = sample();
        b_v1.id = Uuid::now_v7();
        b_v1.plan_id = plan_id;
        b_v1.source_id = "source_b".into();
        b_v1.dedup_key = Some(format!("{plan_id}:source_b"));
        b_v1.version = 1;
        b_v1.authored_at = base;
        let b_v1_id = b_v1.id;
        save_recipe(&store, &b_v1).unwrap();

        let latest = load_latest_recipes_for_plan(&store, plan_id).unwrap();
        assert_eq!(latest.len(), 2, "expected one recipe per source");

        let a = latest.iter().find(|r| r.source_id == "source_a").unwrap();
        assert_eq!(a.id, a_v2_id, "source_a should resolve to v2");

        let b = latest.iter().find(|r| r.source_id == "source_b").unwrap();
        assert_eq!(b.id, b_v1_id, "source_b should resolve to v1");
    }

    /// `load_recipes_for_plan` (the unfiltered variant) still returns
    /// every version. The inspection panel's lineage view depends on
    /// this — a re-author should be visible in the listing, not
    /// silently squashed.
    #[test]
    fn load_recipes_for_plan_returns_all_versions() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();

        let mut v1 = sample();
        v1.id = Uuid::now_v7();
        v1.plan_id = plan_id;
        v1.dedup_key = Some(format!("{plan_id}:source_a"));
        save_recipe(&store, &v1).unwrap();

        let mut v2 = sample();
        v2.id = Uuid::now_v7();
        v2.plan_id = plan_id;
        v2.dedup_key = Some(format!("{plan_id}:source_a"));
        v2.version = 2;
        v2.prior_recipe_id = Some(v1.id);
        save_recipe(&store, &v2).unwrap();

        let all = load_recipes_for_plan(&store, plan_id).unwrap();
        assert_eq!(all.len(), 2, "both versions should be visible");
    }
}
