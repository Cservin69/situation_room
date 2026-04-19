//! Topic research orchestrator.
//!
//! When the user types a topic ("chip production", "uranium enrichment",
//! "rare earth refining outside China"), this module:
//!
//! 1. Asks the LLM to decompose it into a structured [`ResearchPlan`].
//! 2. Matches the plan against the source registry to discover which
//!    sources can contribute (and which gaps will remain).
//! 3. Triggers targeted ingestion via the regular pipeline.
//! 4. Surfaces both the plan and its coverage to the UI so users see what's
//!    being researched and where the gaps are.
//!
//! This is the function that makes Stockpile general-purpose rather than
//! commodity-only. See ADR 0007 (to be written) for the design.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A structured plan for researching a free-text topic. Produced by the LLM
/// during decomposition; consumed by source matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchPlan {
    /// User's original topic string.
    pub topic: String,
    /// LLM's restated interpretation, surfaced to the user for verification.
    pub interpretation: String,
    /// Entities the system should track for this topic.
    pub entities_of_interest: Vec<EntityOfInterest>,
    /// Metrics the system should try to find numbers for.
    pub metrics_of_interest: Vec<MetricOfInterest>,
    /// Event types worth watching.
    pub event_types_of_interest: Vec<String>,
    /// Geographic scope, if applicable.
    pub geographic_scope: Vec<String>,
    /// Time horizon — how far back to look.
    pub historical_window_days: u32,
    /// Plan creation timestamp.
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityOfInterest {
    /// e.g. "TSMC", "ASML", "Albemarle"
    pub name: String,
    /// e.g. "company", "facility", "agency"
    pub kind: String,
    /// Why the LLM included this entity. Surfaced for transparency.
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricOfInterest {
    /// e.g. "wafer_starts", "fab_utilization", "capex"
    pub name: String,
    /// Unit hint ("units/month", "USD billions", "%").
    pub unit_hint: Option<String>,
    pub rationale: String,
}

/// Coverage report — which parts of a [`ResearchPlan`] have data sources
/// and which don't. Surfaced to the UI alongside the populated panels so
/// the user knows what's missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    pub plan_topic: String,
    pub matched_sources: Vec<MatchedSource>,
    pub gaps: Vec<CoverageGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedSource {
    pub source_id: String,
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageGap {
    /// What's missing — entity, metric, or event_type name.
    pub item: String,
    /// What kind of gap it is.
    pub kind: GapKind,
    /// Suggested action the user could take.
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GapKind {
    /// No registered source covers this item.
    NoSourceAvailable,
    /// A source could cover it but requires API key the user hasn't set.
    NeedsApiKey { source_id: String, env_var: String },
    /// Source available but only at low cadence — surface so user knows.
    LowCadenceOnly { source_id: String },
}
