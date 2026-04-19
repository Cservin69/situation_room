//! The metadata envelope that every record carries.
//!
//! Phase 2 will define provenance, subjects, tags, valid_at/observed_at,
//! and confidence as fields here.

use serde::{Deserialize, Serialize};

/// Placeholder. Phase 2 will replace this with the real envelope fields:
/// provenance, subjects, tags, valid_at, observed_at, confidence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Envelope {
    // Phase 2: real fields land here.
    #[serde(skip)]
    _placeholder: (),
}
