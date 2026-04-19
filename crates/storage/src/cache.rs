//! Cache primitives:
//!
//! - **Ring buffer**: per-source, per-feed bounded buffers (e.g. "keep the
//!   last 100 news items per commodity, evict older").
//! - **Archive**: pinned items the user explicitly opened/saved, never evicted.
//!
//! See ADR 0008 (to be written) for the offline-mode caching model.
//! Phase 2 implements both.
