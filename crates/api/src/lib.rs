//! # situation_room-api
//!
//! The frontend-facing surface. Tauri commands and the TypeScript type
//! exports the Svelte frontend imports as compile-time-checked DTOs.
//!
//! Why a separate crate from `apps/desktop/src-tauri`? Because the
//! Tauri shim is the *composition root* (boots the runtime, registers
//! commands) while this crate is the *interface definition*. Keeping
//! them apart means we can run the same API surface from a CLI binary
//! (or a future web server) without dragging Tauri into the
//! dependency graph everywhere.
//!
//! ## What's here
//!
//! - [`commands`] — `#[tauri::command]` handlers wrapping pipeline /
//!   storage operations, plus the [`commands::AppState`] container
//!   the binary populates.
//! - [`types_export`] — wire-shape DTOs for the plan + recipe + fetch
//!   surfaces with `ts-rs` derives that emit `.ts` files into
//!   `apps/desktop/src/lib/api/types/` when the crate's tests are run.
//!   Owning the wire schema in a dedicated set of types keeps
//!   `pipeline` free of any tooling dependency.
//! - [`records_dto`] (Session 22) — wire-shape DTOs for records
//!   produced by a plan's recipes. Renders the workstation surface.
//!
//! ## Removed in Session 6
//!
//! The Phase-1 stubs `queries` and `subscriptions` were deleted. They
//! were placeholder modules whose described surface (`by_subject`,
//! live subscriptions) is downstream of recipe authoring landing in
//! the UI, which the handoff defers to a later session. Per the
//! "structure follows code, not anticipates it" rule (ADR 0001), an
//! empty module is a worse signal than its absence.

pub mod commands;
pub mod commands_records;
pub mod records_dto;
pub mod types_export;
