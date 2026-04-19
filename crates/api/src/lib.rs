// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports, unused_variables)]

//! # stockpile-api
//!
//! The frontend-facing surface. Tauri commands, query handlers, live
//! subscription channels, and TypeScript type export so the Svelte frontend
//! gets type-checked types generated from these Rust definitions.
//!
//! Why a separate crate from `apps/desktop/src-tauri`? Because the Tauri
//! shim is the *composition root* (boots the runtime, registers commands)
//! while this crate is the *interface definition*. Keeping them apart means
//! we can run the same API surface from a CLI binary (or a future web
//! server) without dragging Tauri into the dependency graph.
//!
//! ## Phase 1 status
//!
//! Module structure declared. Commands implemented in Phase 4 alongside
//! the frontend.

pub mod commands;
pub mod queries;
pub mod subscriptions;
pub mod types_export;
