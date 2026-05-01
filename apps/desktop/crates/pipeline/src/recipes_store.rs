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
}
