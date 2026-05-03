//! Tauri commands for records-on-the-workstation rendering (Session 22).
//!
//! Lives in its own module to avoid bloating
//! [`super::commands`], which is already a long file. Imports
//! [`AppState`] and [`CommandError`] from the parent module — the
//! command surface stays consolidated under the same shared state
//! container, just split across files.
//!
//! The frontend invokes `records_for_plan` through the same
//! `tauri::generate_handler!` registration the binary already
//! does for the other commands; no separate IPC channel exists.

use situation_room_storage::research_plans::PlanStatus;
use situation_room_storage::StorageError;
use tracing::info;
use uuid::Uuid;

use crate::commands::{AppState, CommandError};
use crate::records_dto::RecordsByPlanDto;

// ---------------------------------------------------------------------------
// records_for_plan
// ---------------------------------------------------------------------------

/// Return every record produced by any recipe attached to a plan,
/// bucketed by record type.
///
/// Pure read; no LLM call. Cheap on a plan with few recipes (typical:
/// ≤10), bounded by the number of records in the per-type tables for
/// pathological cases.
///
/// ## Status gating
///
/// The plan must be `accepted` or `rejected`. A `pending` plan has by
/// definition not been fetched yet (the executor only runs against
/// accepted plans), so listing its records would return an empty
/// `RecordsByPlanDto` regardless — but returning empty would mask
/// the lifecycle problem. We surface `InvalidInput` instead so the
/// frontend can render "this plan hasn't been accepted; accept it
/// before fetching." Rejected plans are allowed because they may
/// have records from a prior accept-then-reject cycle, and the
/// operator may still want to inspect them for audit.
///
/// ## Response shape
///
/// [`RecordsByPlanDto`] is six per-type Vecs. Empty buckets surface
/// as empty Vecs, never errors. The frontend distinguishes "no
/// expectations for this type — by design" from "expectations present
/// but no records yet" by comparing the plan's expectations against
/// the bucket's records length.
///
/// ## Errors
///
/// - `InvalidInput { field: "id" }` — id isn't a valid UUID, or the
///   plan is in `pending` state.
/// - `NotFound` — id not in store.
/// - `Storage` — DB-level failure during the join.
#[tauri::command]
pub async fn records_for_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<RecordsByPlanDto, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    info!(plan_id = %parsed, "records_for_plan command invoked");

    // Status check: confirm the plan exists and is past pending.
    // We fetch the plan to read its status; if NotFound, surface that
    // distinct from a generic InvalidInput so the frontend can route
    // the message appropriately ("plan disappeared" vs "bad input").
    let plan = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    match plan.status {
        PlanStatus::Pending => {
            return Err(CommandError::InvalidInput {
                field: "id".into(),
                message: "plan must be accepted before records can be listed (current: pending)"
                    .into(),
            });
        }
        // Accepted or Rejected — both legitimate; rejected may still
        // have records from a pre-rejection accept cycle.
        PlanStatus::Accepted | PlanStatus::Rejected => {}
    }

    // The actual join. records_for_plan is the cross-record-type
    // query that pulls everything for the plan in one method call.
    let bucket = state
        .store
        .records_for_plan(parsed)
        .map_err(|e| match e {
            StorageError::NotFound(_) => CommandError::NotFound { id: id.clone() },
            other => CommandError::from(other),
        })?;

    info!(
        plan_id = %parsed,
        observations = bucket.observations.len(),
        events = bucket.events.len(),
        entities = bucket.entities.len(),
        relations = bucket.relations.len(),
        documents = bucket.documents.len(),
        assertions = bucket.assertions.len(),
        "records_for_plan returning"
    );

    Ok(RecordsByPlanDto::from_typed(bucket))
}
