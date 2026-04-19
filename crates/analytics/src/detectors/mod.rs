//! Anomaly detectors. One file per detector. Promote to a folder when a
//! detector grows multiple internal modules.

pub mod inventory_divergence;
pub mod guidance_dispersion;
pub mod flow_rerouting;
pub mod capex_production;
pub mod concentration;
pub mod news_latency;
pub mod filing_language;

use crate::anomalies::Anomaly;
use async_trait::async_trait;

/// Contract every detector implements.
///
/// Detectors are stateless — they read from storage, compute, return anomalies.
/// Persisting detected anomalies is the orchestrator's job.
#[async_trait]
pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// Run one pass of the detector against current state.
    /// Returns zero or more anomalies.
    async fn detect(&self) -> Vec<Anomaly>;
}
