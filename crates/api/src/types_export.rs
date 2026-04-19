//! TypeScript type generation.
//!
//! Uses ts-rs to derive TS types from the Rust structs in core, analytics,
//! pipeline::research, and the api crate itself. The frontend imports the
//! generated `.d.ts` files so type changes in Rust immediately break the
//! TypeScript build.
//!
//! Phase 4 implementation. Phase 1 declares the module.
