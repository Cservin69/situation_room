//! Typed research-plan storage helper.
//!
//! Thin marshalling layer between [`ResearchPlan`] and the storage
//! crate's [`ResearchPlanRow`]. Storage stays agnostic of the typed
//! plan shape (pipeline owns the types); this module is the single
//! place the conversion lives.
//!
//! Mirrors the shape of [`crate::recipes_store`]; the rationale is
//! identical (storage mustn't reverse-depend on pipeline).

use stockpile_storage::{
    research_plans::{ResearchPlanRow, StoredResearchPlan},
    Store,
};
use thiserror::Error;
use uuid::Uuid;

use crate::research::{RecordExpectations, ResearchPlan};

#[derive(Debug, Error)]
pub enum ResearchPlanStoreError {
    #[error("storage error: {0}")]
    Storage(#[from] stockpile_storage::StorageError),

    #[error("plan serialization failed: {0}")]
    Serialize(String),

    #[error("plan deserialization failed: {0}")]
    Deserialize(String),
}

/// Persist a typed [`ResearchPlan`] to storage.
///
/// `classified_by` is the provider id that ran classification (e.g.
/// `"xai"`). It's persisted as part of the plan row so audits can
/// trace which model produced the classification.
pub fn save_research_plan(
    store: &Store,
    plan: &ResearchPlan,
    classified_by: &str,
) -> Result<(), ResearchPlanStoreError> {
    let row = plan_to_row(plan, classified_by)?;
    store
        .insert_research_plan(&row)
        .map_err(ResearchPlanStoreError::Storage)
}

/// Look up a plan by id. Returns `Ok(None)` if not present.
pub fn load_research_plan(
    store: &Store,
    id: Uuid,
) -> Result<Option<ResearchPlan>, ResearchPlanStoreError> {
    let stored: Option<StoredResearchPlan> = store
        .get_research_plan(id)
        .map_err(ResearchPlanStoreError::Storage)?;
    stored.map(stored_to_plan).transpose()
}

fn plan_to_row(
    plan: &ResearchPlan,
    classified_by: &str,
) -> Result<ResearchPlanRow, ResearchPlanStoreError> {
    let topic_tags_json = serde_json::to_string(&plan.topic_tags)
        .map_err(|e| ResearchPlanStoreError::Serialize(format!("topic_tags: {e}")))?;
    let geographic_scope_json = serde_json::to_string(&plan.geographic_scope)
        .map_err(|e| ResearchPlanStoreError::Serialize(format!("geographic_scope: {e}")))?;
    let expectations_json = serde_json::to_string(&plan.expectations)
        .map_err(|e| ResearchPlanStoreError::Serialize(format!("expectations: {e}")))?;

    Ok(ResearchPlanRow {
        id: plan.id,
        topic: plan.topic.clone(),
        interpretation: plan.interpretation.clone(),
        topic_tags_json,
        geographic_scope_json,
        historical_window_days: plan.historical_window_days,
        expectations_json,
        created_at: plan.created_at,
        classified_by: classified_by.to_string(),
    })
}

fn stored_to_plan(s: StoredResearchPlan) -> Result<ResearchPlan, ResearchPlanStoreError> {
    let topic_tags = serde_json::from_str(&s.topic_tags_json)
        .map_err(|e| ResearchPlanStoreError::Deserialize(format!("topic_tags: {e}")))?;
    let geographic_scope = serde_json::from_str(&s.geographic_scope_json)
        .map_err(|e| ResearchPlanStoreError::Deserialize(format!("geographic_scope: {e}")))?;
    let expectations: RecordExpectations = serde_json::from_str(&s.expectations_json)
        .map_err(|e| ResearchPlanStoreError::Deserialize(format!("expectations: {e}")))?;

    Ok(ResearchPlan {
        id: s.id,
        topic: s.topic,
        interpretation: s.interpretation,
        topic_tags,
        geographic_scope,
        historical_window_days: s.historical_window_days,
        expectations,
        created_at: s.created_at,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{
        DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
        MetricExpectation, RecordExpectations, RelationKindExpectation,
    };
    use chrono::Utc;
    use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium supply chain".into(),
            interpretation: "Lithium production, refining, trade flows.".into(),
            topic_tags: vec![
                Topic::new("lithium").unwrap(),
                Topic::new("battery_supply_chain").unwrap(),
            ],
            geographic_scope: vec![
                GeoScope {
                    code: "AU".into(),
                    display: "Australia".into(),
                },
                GeoScope {
                    code: "CL".into(),
                    display: "Chile".into(),
                },
                GeoScope::code_only("CN"),
            ],
            historical_window_days: 730,
            expectations: RecordExpectations {
                observation_metrics: vec![MetricExpectation {
                    name: "production".into(),
                    unit_hint: Some(Unit::new("t").unwrap()),
                    rationale: "Primary volume metric".into(),
                }],
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("mine_opened").unwrap(),
                    rationale: "Capacity expansion signal".into(),
                }],
                entity_kinds: vec![EntityKindExpectation {
                    kind: "mine".into(),
                    exemplars: vec![EntityId::new("mine:greenbushes").unwrap()],
                    rationale: "Atomic unit of supply".into(),
                }],
                relation_kinds: vec![RelationKindExpectation {
                    kind: "operator_of".into(),
                    rationale: "Operator-asset link".into(),
                }],
                document_sources: vec![DocumentSourceHint {
                    description: "USGS Mineral Commodity Summaries".into(),
                    preferred_source_ids: vec!["usgs_mcs".into()],
                }],
                assertion_guidance: None,
            },
            created_at: Utc::now(),
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan();
        save_research_plan(&store, &plan, "xai").unwrap();

        let back = load_research_plan(&store, plan.id).unwrap().unwrap();
        assert_eq!(back.id, plan.id);
        assert_eq!(back.topic, plan.topic);
        assert_eq!(back.interpretation, plan.interpretation);
        assert_eq!(back.topic_tags.len(), plan.topic_tags.len());
        assert_eq!(back.geographic_scope, plan.geographic_scope);
        assert_eq!(back.historical_window_days, plan.historical_window_days);
        assert_eq!(
            back.expectations.observation_metrics.len(),
            plan.expectations.observation_metrics.len()
        );
        assert_eq!(
            back.expectations.event_types.len(),
            plan.expectations.event_types.len()
        );
        assert_eq!(
            back.expectations.entity_kinds.len(),
            plan.expectations.entity_kinds.len()
        );
    }

    #[test]
    fn load_returns_none_when_missing() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let got = load_research_plan(&store, Uuid::now_v7()).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn save_preserves_classified_by_provider() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan = sample_plan();
        save_research_plan(&store, &plan, "xai").unwrap();

        // Verify via the underlying StoredResearchPlan, since the
        // typed ResearchPlan doesn't carry classified_by — that's a
        // storage-layer audit field, not part of the plan itself.
        let stored = store.get_research_plan(plan.id).unwrap().unwrap();
        assert_eq!(stored.classified_by, "xai");
    }
}
