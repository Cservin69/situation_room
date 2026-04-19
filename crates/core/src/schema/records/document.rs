//! `Document` — raw content as fetched from a source: an article, filing,
//! transcript, press release, tweet, satellite image caption, etc.
//!
//! Documents are preserved because Assertions are extracted from them
//! via the LLM layer; the Assertion's `derived_from` field points back
//! at the Document's UUID so the user can trace a claim to its source.

use crate::schema::envelope::Envelope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub id: Uuid,

    /// For dedup across re-fetches. Often the source URL or a hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    /// Title or headline, if the document has one.
    pub title: Option<String>,

    /// Document kind. Lowercase snake_case vocabulary:
    /// `article`, `filing`, `transcript`, `press_release`, `tweet`,
    /// `research_note`, `satellite_image_caption`.
    pub kind: String,

    /// MIME type of the content body. Usually `text/plain`, `text/html`,
    /// or `application/pdf`. Guides downstream parsing.
    pub mime: String,

    /// The body of the document. For binary formats (PDFs), this is the
    /// extracted-text representation; the original binary goes to the
    /// file archive keyed by `id`. We keep text inline so extraction
    /// doesn't need a second round-trip to the filesystem.
    pub body: String,

    /// Source-reported publication timestamp, if any. Distinct from
    /// `envelope.observed_at` (when we fetched it) and from
    /// `envelope.valid_at` (which is unused for documents — a document
    /// isn't true or false, it exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,

    /// Author or byline, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    pub envelope: Envelope,
}

impl Document {
    pub fn new(kind: impl Into<String>, body: impl Into<String>, envelope: Envelope) -> Self {
        Self {
            id: Uuid::now_v7(),
            dedup_key: None,
            title: None,
            kind: kind.into(),
            mime: "text/plain".into(),
            body: body.into(),
            published_at: None,
            author: None,
            envelope,
        }
    }
}
