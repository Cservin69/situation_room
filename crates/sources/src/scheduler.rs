//! Source scheduler. Phase 2+ implements cron-style triggering and watermark
//! persistence. This stub establishes the module path.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Per-source watermark — the most recent observed_at successfully fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watermark {
    pub source_id: String,
    pub last_fetch_started: Option<DateTime<Utc>>,
    pub last_fetch_succeeded: Option<DateTime<Utc>>,
    pub last_observed_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}
