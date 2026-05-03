//! Shared composition-root helpers reused by the desktop binary
//! (`apps/desktop/src-tauri`) and the CLI binary (`apps/situation_room`).
//!
//! The crate exists to host code that is *genuinely common* to both
//! entry points — code that has no app-specific decisions and that
//! would otherwise be word-for-word duplicated. Lifted in Session 24
//! per the Session 23 followup ("`SourceDescriptor` consolidation"):
//! the two binaries each had a local `SourcesFile`/`SourceEntry` pair
//! and a local `load_source_descriptors` function whose code differed
//! only in a single `tracing::warn!` macro path (`tracing::warn!` vs
//! `warn!` after a `use tracing::warn;`). Maintaining two copies of
//! TOML schema parsing is exactly the duplication ADR 0001's "if we
//! later need a non-Tauri entry point … duplicating whatever wiring
//! is shared at that point" anticipates needing to lift, so this is
//! that lift.
//!
//! ## What does NOT belong here
//!
//! Per Session 23's documented reasoning for the *deliberately*
//! duplicated `pick_provider` helper: app-level boot decisions stay
//! in each binary's `main.rs`. The CLI defaults differ from the
//! desktop binary's defaults, the error-surfacing posture differs
//! (CLI exits with a non-zero code; desktop logs and tries to
//! continue), and the LLM-provider selection in particular ties the
//! binary to specific concrete provider types we want each main.rs
//! to import directly.
//!
//! Concretely: this crate hosts pure helper functions over types
//! defined elsewhere (TOML schemas, descriptor loaders). It does
//! **not** host:
//!
//! - `AppState` construction (lives in each binary's main).
//! - LLM provider selection (the `pick_provider` helper stays
//!   duplicated by design).
//! - `.env` loading (each binary's main owns its own minimal
//!   loader; the CLI uses `dotenvy`, the desktop binary uses a
//!   hand-rolled walker that returns the workspace root).
//! - Logging initialization (each binary calls
//!   `situation_room_secure::logging::init` directly).
//! - Tauri command registration (binary-specific by definition).
//!
//! If a new helper is being lifted into this crate, the test is:
//! "does its body contain *any* binary-specific decision?" If yes,
//! it stays in the binary; only the truly-shared part gets lifted.

pub mod sources;

pub use sources::{load_source_descriptors, SourceEntry, SourcesFile};
