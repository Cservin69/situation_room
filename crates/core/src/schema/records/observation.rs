//! `Observation` — an authoritative measurement of a metric at a point
//! in time. Directly fetched from a trusted source, or promoted from
//! consensus Assertions.

use crate::schema::content::ObservationContent;
use crate::schema::envelope::Envelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Observation {
    pub id: Uuid,

    /// Optional natural key for dedup. When a source has stable native IDs
    /// (SEC EDGAR's accession number, LME's daily-report key), ingest code
    /// can populate this to let the pipeline detect duplicates across fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    pub envelope: Envelope,
    pub content: ObservationContent,
}

impl Observation {
    /// Construct with a fresh time-ordered UUIDv7.
    pub fn new(envelope: Envelope, content: ObservationContent) -> Self {
        Self {
            id: Uuid::now_v7(),
            dedup_key: None,
            envelope,
            content,
        }
    }

    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = Some(key.into());
        self
    }
}
