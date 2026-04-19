//! Event record type.
//!
//! Phase 2 will define the real fields. See docs/adr/0003-six-record-types.md.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    // Phase 2: real fields land here.
    // Default impl + empty struct keeps the stub usable as a placeholder.
    #[serde(skip)]
    _placeholder: (),
}
