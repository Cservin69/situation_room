//! `Event` — a discrete dated happening. Directly fetched from a trusted
//! source, or promoted from consensus Assertions.

use crate::schema::content::EventContent;
use crate::schema::envelope::Envelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: Uuid,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    pub envelope: Envelope,
    pub content: EventContent,
}

impl Event {
    pub fn new(envelope: Envelope, content: EventContent) -> Self {
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
