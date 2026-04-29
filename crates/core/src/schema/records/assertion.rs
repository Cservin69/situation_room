//! `Assertion` — a structured claim made by some claimant, with a stance.
//!
//! This is the primary output type of the LLM extraction layer. When
//! the extractor reads a news article that says "Chile produced 142kt
//! of lithium in 2025, according to USGS," it emits an Assertion whose:
//!
//! - `envelope.provenance` points to the article (where the claim was made),
//! - `claimant` is USGS (who made the claim, per the article),
//! - `stance` is [`Stance::Asserted`](crate::vocab::Stance::Asserted)
//!   (the article reports this as fact, not speculation),
//! - `content` is [`AssertedContent::Observation`] wrapping the
//!   production number.
//!
//! The pipeline's promotion stage then decides whether this Assertion is
//! trusted enough to elevate to a full [`Observation`](super::Observation)
//! (via either authoritative-source designation or multi-source consensus).

use crate::schema::content::AssertedContent;
use crate::schema::envelope::Envelope;
use crate::vocab::{EntityId, Stance};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assertion {
    pub id: Uuid,

    /// For dedup when the same article is re-fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    /// Who is making this claim? May differ from `envelope.provenance`:
    /// the provenance is *where the claim was recorded* (e.g. a Reuters
    /// article), the claimant is *who is making the claim* (e.g. USGS,
    /// as cited in that article).
    pub claimant: EntityId,

    /// The claimant's stance toward the content.
    pub stance: Stance,

    /// The content being claimed.
    pub content: AssertedContent,

    pub envelope: Envelope,
}

impl Assertion {
    pub fn new(
        claimant: EntityId,
        stance: Stance,
        content: AssertedContent,
        envelope: Envelope,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            dedup_key: None,
            claimant,
            stance,
            content,
            envelope,
        }
    }
}
