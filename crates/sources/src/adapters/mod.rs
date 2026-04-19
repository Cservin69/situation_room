//! Source adapters. Each adapter is a folder containing at minimum a `mod.rs`
//! that defines a struct implementing [`crate::traits::Source`].
//!
//! Phase 1 ships empty folders — adapters land in Phase 3+.

pub mod usgs;
pub mod comtrade;
pub mod lme;
pub mod gfex;
pub mod sec_edgar;
pub mod gdelt;
pub mod rss;
