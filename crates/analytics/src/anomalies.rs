//! The Anomaly record type — what detectors emit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub id: String,
    /// Which detector fired.
    pub detector_id: String,
    /// When the anomaly was detected.
    pub detected_at: DateTime<Utc>,
    /// What this anomaly is about — references to records that triggered it.
    pub triggering_records: Vec<String>,
    /// Subjects affected (commodity, country, entity).
    pub subjects: Vec<String>,
    /// Severity bucket.
    pub severity: AnomalySeverity,
    /// Historical base rate for patterns like this. 0.05 = "this happens
    /// in 5% of similar windows historically". Lets the UI distinguish
    /// rare-and-meaningful from common noise.
    pub historical_base_rate: Option<f64>,
    /// The data supporting the detection — detector-specific JSON.
    pub evidence: Value,
    /// One-line LLM-generated rationale (filled by the explanation layer).
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AnomalySeverity {
    Info,
    Notable,
    High,
    Critical,
}
