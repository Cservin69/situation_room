// Phase 1: many declared items are stubs. These allow attributes will be
// removed as Phase 2/3 fill in real implementations.
#![allow(dead_code, unused_imports)]

//! # stockpile-core
//!
//! The schema. This crate defines the six record types, the envelope that
//! every record carries, and the controlled vocabularies they reference.
//!
//! It has zero dependencies on other workspace crates by design: if you want
//! to know what Stockpile *is*, you read this crate and you have the full
//! picture.
//!
//! ## Module layout
//!
//! - [`schema`] — record types and the envelope
//! - [`vocab`] — controlled vocabularies (commodity codes, country codes,
//!   event types, units, currencies, stance)
//! - [`error`] — the crate's error type
//!
//! ## Phase 1 status
//!
//! Module stubs only. Types land in Phase 2.

pub mod schema;
pub mod vocab;
pub mod error;

pub use error::CoreError;
