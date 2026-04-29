//! Geometry is an optional field on Entity, Event, and Observation.
//! It is *not* a top-level record type — see ADR 0003 for why.
//!
//! Phase 2 will replace `Phase1Placeholder` with Point/LineString/Polygon variants.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Geometry {
    /// Stub variant so the enum is inhabited in Phase 1. Removed in Phase 2.
    Phase1Placeholder,
}
