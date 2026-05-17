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

use std::collections::HashMap;

use situation_room_core::RecordType;
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

// ---------------------------------------------------------------------------
// records_recent_global — Session 63
// ---------------------------------------------------------------------------

/// Default per-type cap for `records_recent_global`. Each of the six
/// per-type Vecs in the returned bucket is capped independently at
/// this many newest-first rows, so the wire payload's worst case is
/// 6 × 200 = 1,200 record rows. On realistic populations the totals
/// are far smaller — most types are empty for any given session — and
/// the IPC round-trip stays sub-100ms.
const DEFAULT_GLOBAL_LIMIT: usize = 200;

/// Hard ceiling for the operator-supplied limit. Keeps a pathological
/// `limit = 1_000_000` from blowing the IPC round-trip even if the DB
/// grew unexpectedly large. Operator-supplied values above this clamp
/// down silently — there's no error case, just a saner answer.
const MAX_GLOBAL_LIMIT: usize = 500;

/// Return the most recent records of each type across **all plans**.
/// Pure read; no LLM call.
///
/// Powers the situation-room dashboard's cross-plan view (Session 63).
/// The per-plan view via [`records_for_plan`] still exists; this is
/// the surface that answers "what has the system collected over
/// time, across every plan." That's the operator's mental model of
/// the dashboard: a cumulative view of records, not a per-plan
/// projection that resets every time a new topic is classified.
///
/// ## Status gating
///
/// None. Unlike [`records_for_plan`], which refuses pending plans
/// because their records bucket is meaningless, the global query has
/// no plan to gate on. An empty store legitimately returns an empty
/// `RecordsByPlanDto` — the frontend distinguishes "nothing has been
/// fetched yet" from "we just don't have data for this type" by the
/// six per-type counts in the response.
///
/// ## Limit
///
/// `limit` caps each per-type Vec independently. Defaults to
/// [`DEFAULT_GLOBAL_LIMIT`] when `None`; clamped to
/// [`MAX_GLOBAL_LIMIT`] regardless.
///
/// ## Errors
///
/// - `Storage` — DB-level failure during one of the six per-table
///   queries. No other failure modes; the query is a pure read with
///   no input validation surface.
#[tauri::command]
pub async fn records_recent_global(
    limit: Option<usize>,
    state: tauri::State<'_, AppState>,
) -> Result<RecordsByPlanDto, CommandError> {
    let effective = limit.unwrap_or(DEFAULT_GLOBAL_LIMIT).min(MAX_GLOBAL_LIMIT);

    info!(limit = effective, "records_recent_global command invoked");

    let bucket = state
        .store
        .recent_records_global(effective)
        .map_err(CommandError::from)?;

    info!(
        limit = effective,
        observations = bucket.observations.len(),
        events = bucket.events.len(),
        entities = bucket.entities.len(),
        relations = bucket.relations.len(),
        documents = bucket.documents.len(),
        assertions = bucket.assertions.len(),
        "records_recent_global returning"
    );

    Ok(RecordsByPlanDto::from_typed(bucket))
}

// ---------------------------------------------------------------------------
// promote_consensus_for_plan — Session 81 (ADR 0004 / ADR 0021)
// ---------------------------------------------------------------------------

/// Run the consensus-promotion pass for one plan and return the
/// summary report. The pass walks every persisted `Assertion` tied
/// to the plan and, for groups of ≥ `min_independent_claimants`
/// (default 3) distinct claimants making compatible claims, emits a
/// single promoted record (`Observation` / `Event` / `Relation`) or
/// a consensus-stamped `EntityAttribute` assertion.
///
/// **Idempotent on re-run.** The `dedup_key` for each promoted record
/// is content-derived (`promotion:{content_hash}:{subject_hash}`); a
/// second invocation against the same assertion store skips groups
/// already promoted on a prior run.
///
/// ## Status gating
///
/// Same as [`records_for_plan`]: the plan must be `accepted` or
/// `rejected`. A `pending` plan has by definition no Assertion rows
/// to consense over, so the call surfaces `InvalidInput` instead of
/// silently returning a zero report.
///
/// ## Errors
///
/// - `InvalidInput { field: "id" }` — id isn't a valid UUID, or the
///   plan is `pending`.
/// - `NotFound` — id not in store.
/// - `Storage` — DB-level failure during the assertion-load read.
#[tauri::command]
pub async fn promote_consensus_for_plan(
    id: String,
    min_independent_claimants: Option<u32>,
    state: tauri::State<'_, AppState>,
) -> Result<situation_room_pipeline::promote::PromoteReport, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    info!(plan_id = %parsed, "promote_consensus_for_plan command invoked");

    let stored = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    match stored.status {
        PlanStatus::Pending => {
            return Err(CommandError::InvalidInput {
                field: "id".into(),
                message: "plan must be accepted before consensus promotion (current: pending)"
                    .into(),
            });
        }
        PlanStatus::Accepted | PlanStatus::Rejected => {}
    }

    let plan = situation_room_pipeline::research_plans_store::load_research_plan(
        &state.store,
        parsed,
    )
    .map_err(|e| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("plan deserialization failed: {e}"),
    })?
    .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    let cfg = situation_room_pipeline::promote::PromoteConfig {
        min_independent_claimants: min_independent_claimants.unwrap_or_else(|| {
            situation_room_pipeline::promote::PromoteConfig::default().min_independent_claimants
        }),
        // Session 82 — ADR 0004 pathway 1. Snapshot the live registry
        // (Session 84 hot-reload) and deref-clone the contents into
        // the per-call PromoteConfig. The registry stays small; the
        // clone cost is negligible.
        authoritative: (*state.authoritative.snapshot()).clone(),
    };

    let report = situation_room_pipeline::promote::promote_consensus_for_plan(
        &state.store,
        &plan,
        &cfg,
    )
    .map_err(|e| match e {
        situation_room_pipeline::promote::PromoteError::Storage(s) => CommandError::from(s),
    })?;

    info!(
        plan_id = %parsed,
        considered = report.assertions_considered,
        promoted = report.groups_promoted,
        skipped = report.skipped_already_promoted,
        observations = report.observations_emitted,
        events = report.events_emitted,
        relations = report.relations_emitted,
        entity_attributes = report.entity_attributes_emitted,
        "promote_consensus_for_plan returning"
    );

    // Session 84 — record this run into AppState's
    // `last_promote_summary` so the dashboard tile picks it up.
    crate::commands::record_last_promote_summary(
        state.inner(),
        parsed,
        crate::commands::LastPromoteTrigger::Manual,
        report.clone(),
    );

    Ok(report)
}

// ---------------------------------------------------------------------------
// reextract_relations_for_plan — Session 92, ADR 0023 Option 2
// ---------------------------------------------------------------------------

/// Re-run the relation Assertion extractor over every article-kind
/// Document already on disk for one plan, using the v1.2 prompt
/// currently loaded in `AppState::document_assertions_prompt`.
///
/// The executor's per-Document hook (Session 77) fires the v1.2
/// prompt on net-new fetches only; this command exists so the
/// operator can backfill the pre-Sn-91 Document corpus without
/// re-fetching. Cost is bounded by article-kind Document count per
/// plan (one workhorse-tier LLM call per Document) — the report
/// surfaces the count so the operator can see the spend.
///
/// **Per-plan granularity.** Session 92 Option 2 chose per-plan
/// over per-Document selection; the operator picks one plan and
/// kicks the full re-extraction, rather than scrolling Document
/// drawers and clicking individually.
///
/// **Idempotency caveat.** Re-running this command produces fresh
/// Assertion rows (v1 has no per-Document dedup); the downstream
/// `promote_consensus_for_plan` pass dedups at the cross-source
/// consensus layer. Operators who plan to re-extract repeatedly
/// should run promote between passes.
///
/// ## Status gating
///
/// Plan must be `Accepted` or `Rejected`. Pending plans haven't
/// been fetched yet, so they have no Documents on disk to
/// re-extract from — surfacing `InvalidInput` here matches
/// `promote_consensus_for_plan`'s posture.
///
/// ## Errors
///
/// - `InvalidInput { field: "id" }` — id isn't a valid UUID, or
///   the plan is pending.
/// - `NotFound` — id not in store.
/// - `Storage` — DB-level failure during the plan / recipe loads.
#[tauri::command]
pub async fn reextract_relations_for_plan(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<situation_room_pipeline::reextract::ReextractReport, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    info!(plan_id = %parsed, "reextract_relations_for_plan command invoked");

    let stored = state
        .store
        .get_research_plan(parsed)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    match stored.status {
        PlanStatus::Pending => {
            return Err(CommandError::InvalidInput {
                field: "id".into(),
                message:
                    "plan must be accepted before re-extraction (current: pending; nothing fetched yet)"
                        .into(),
            });
        }
        PlanStatus::Accepted | PlanStatus::Rejected => {}
    }

    let plan = situation_room_pipeline::research_plans_store::load_research_plan(
        &state.store,
        parsed,
    )
    .map_err(|e| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("plan deserialization failed: {e}"),
    })?
    .ok_or_else(|| CommandError::NotFound { id: id.clone() })?;

    let report = situation_room_pipeline::reextract::reextract_relations_for_plan(
        &state.store,
        state.provider.as_ref(),
        state.document_assertions_prompt,
        &plan,
    )
    .await;

    info!(
        plan_id = %parsed,
        documents_considered = report.documents_considered,
        documents_unrouted = report.documents_unrouted,
        assertions_extracted = report.assertions_extracted,
        assertions_persisted = report.assertions_persisted,
        assertion_insert_failures = report.assertion_insert_failures,
        llm_call_errors = report.llm_call_errors,
        "reextract_relations_for_plan returning"
    );

    Ok(report)
}

// ---------------------------------------------------------------------------
// record_types_for_ids — Session 88 (Sn-87 candidate 4)
// ---------------------------------------------------------------------------

/// Cap on the batch size for one `record_types_for_ids` call. Promote
/// reports today carry at most a few dozen ids; this cap is set well
/// above the realistic ceiling to keep a hostile / accidental
/// gigantic-batch call from holding the storage mutex for long, while
/// still being large enough that any single PromoteDetailDrawer row
/// dump fits in one request.
const MAX_IDS_PER_BATCH: usize = 500;

/// Resolve a batch of record UUIDs to their per-table record type.
///
/// Returns a map `{ id → "observation" | "event" | … }`. Ids that
/// don't exist in any of the six per-type tables are simply absent
/// from the map (no error), so the caller can render a placeholder
/// chip for unknown ids without special-casing the error branch.
///
/// Used today by the PromoteDetailDrawer to colour-code the per-pass
/// `promoted_record_ids` strip (Session 87) by the record type each
/// id resolves to. Generalises to any future inspector that ingests
/// a heterogeneous id list and needs to dispatch to the type-specific
/// drawer.
///
/// ## Errors
///
/// - `InvalidInput { field: "ids" }` — one of the input strings isn't
///   a valid UUID, or the batch exceeds `MAX_IDS_PER_BATCH`.
/// - `Storage` — DB-level failure on one of the six per-table scans.
#[tauri::command]
pub async fn record_types_for_ids(
    ids: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<HashMap<String, String>, CommandError> {
    if ids.len() > MAX_IDS_PER_BATCH {
        return Err(CommandError::InvalidInput {
            field: "ids".into(),
            message: format!(
                "batch size {} exceeds cap {MAX_IDS_PER_BATCH}",
                ids.len()
            ),
        });
    }

    let parsed: Vec<Uuid> = ids
        .iter()
        .map(|s| {
            s.parse::<Uuid>().map_err(|e| CommandError::InvalidInput {
                field: "ids".into(),
                message: format!("not a valid UUID `{s}`: {e}"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    info!(batch_size = parsed.len(), "record_types_for_ids command invoked");

    let resolved: HashMap<Uuid, RecordType> = state
        .store
        .record_types_for_ids(&parsed)
        .map_err(CommandError::from)?;

    // Flatten Uuid → String + RecordType → snake_case string for the
    // wire. Frontend `Map<string, RecordType>` consumes it directly.
    let out: HashMap<String, String> = resolved
        .into_iter()
        .map(|(id, kind)| (id.to_string(), kind.as_str().to_string()))
        .collect();

    info!(returned = out.len(), "record_types_for_ids returning");

    Ok(out)
}

// ---------------------------------------------------------------------------
// get_record_by_id — Session 88 (Sn-87 candidate 5)
// ---------------------------------------------------------------------------

/// Fetch a single record by UUID, across all six per-type tables.
///
/// Returns the typed `Record` enum (six-variant closed vocab) wrapped
/// in `Option`: `None` when the id resolves nowhere. Designed for
/// click-through from an opaque id list (the PromoteDetailDrawer's
/// per-pass id rows, a future paste-id inspector, …) where the caller
/// doesn't know the type up-front. Pure read.
///
/// ## Errors
///
/// - `InvalidInput { field: "id" }` — input isn't a valid UUID.
/// - `Storage` — DB-level failure on the per-table lookup once the
///   type was resolved. (A missing id surfaces as `Ok(None)`, not an
///   error.)
#[tauri::command]
pub async fn get_record_by_id(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Option<situation_room_core::Record>, CommandError> {
    let parsed: Uuid = id.parse().map_err(|e: uuid::Error| CommandError::InvalidInput {
        field: "id".into(),
        message: format!("not a valid UUID: {e}"),
    })?;

    info!(record_id = %parsed, "get_record_by_id command invoked");

    let rec = state
        .store
        .get_record_by_id(parsed)
        .map_err(CommandError::from)?;

    info!(
        record_id = %parsed,
        found = rec.is_some(),
        record_type = rec.as_ref().map(|r| r.record_type().as_str()).unwrap_or("none"),
        "get_record_by_id returning"
    );

    Ok(rec)
}
