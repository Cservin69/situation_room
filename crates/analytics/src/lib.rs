// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-analytics
//!
//! Anomaly detection, aggregates, and scoring.
//!
//! Detectors are pure functions over the storage layer — given the database
//! at time T, they produce zero or more [`Anomaly`] records. They are
//! deterministic, debuggable, and have no LLM in the detection loop. The
//! LLM is only used in the *explanation* layer, after detection.
//!
//! ## Phase 1 status
//!
//! Trait and module structure declared. Detector implementations land in
//! Phase 4.

pub mod anomalies;
pub mod detectors;
pub mod aggregates;
pub mod scoring;

pub use anomalies::{Anomaly, AnomalySeverity};
pub use detectors::Detector;
