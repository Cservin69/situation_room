//! `Relation` — a directed edge between two entities, possibly with
//! magnitude and time validity.

use crate::schema::content::RelationContent;
use crate::schema::envelope::Envelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relation {
    pub id: Uuid,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    pub envelope: Envelope,
    pub content: RelationContent,
}

impl Relation {
    pub fn new(envelope: Envelope, content: RelationContent) -> Self {
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
