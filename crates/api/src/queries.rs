//! Read queries the frontend issues. All return strongly-typed results
//! that get TS-exported via `types_export`.
//!
//! Examples (Phase 4):
//! - `by_subject(commodity: CommodityId) -> Vec<Record>`
//! - `by_metric(metric: String, since: DateTime<Utc>) -> Vec<Observation>`
//! - `by_event_type(et: EventType) -> Vec<Event>`
