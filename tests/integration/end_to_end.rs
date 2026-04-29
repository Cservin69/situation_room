//! End-to-end integration test scaffold.
//!
//! Phase 3+ will exercise the full pipeline: fixture source → ingest →
//! normalize → extract → promote → detector → query. Phase 1 just verifies
//! the test crate compiles.

#[test]
fn workspace_compiles() {
    // This test exists so `cargo test --workspace` has at least one
    // assertion to run. Real tests land in Phase 3.
    assert!(true);
}
