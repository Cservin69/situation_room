//! Source-descriptor loader for `config/sources.toml`.
//!
//! Lifted in Session 24 from two word-for-word identical copies that
//! used to live in `apps/desktop/src-tauri/src/main.rs` and
//! `apps/situation_room/src/main.rs`. The two binaries now both call
//! into here.
//!
//! ## Schema
//!
//! See `config/sources.toml`'s header comment for the human-facing
//! schema documentation. The Rust types in this module are the
//! mechanical mirror.
//!
//! ## Behavior contract (preserved verbatim from the prior copies)
//!
//! - Missing file is **not** an error. The caller logs the absence
//!   via the warning emitted here and proceeds with an empty
//!   descriptor list. The classifier handles an empty list by
//!   telling the LLM "no sources are currently known" and
//!   nominating purely by description.
//! - The first `limit` entries are returned, in file order. The
//!   classifier shows them to the LLM in the same order, so the
//!   priority tiering in the TOML file is preserved end-to-end.
//! - `description` is trimmed of leading and trailing whitespace
//!   (TOML triple-quoted strings make this convenient for editors,
//!   awkward for prompt-building).
//! - `endpoint_hint` is parsed as `Option<String>`; absent, empty,
//!   and whitespace-only values all normalize to `None` so the
//!   executor's lookup path doesn't see a useless empty string.
//! - URL validity is **not** checked at load time. A bad URL here
//!   produces a clean fallback to the synthetic-placeholder
//!   behavior at use time, not a hard configuration error. ADR
//!   0014 surfaces the consequence as a `StubExcerpt` chip.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use situation_room_pipeline::research_classifier::SourceDescriptor;
use tracing::warn;

/// On-disk shape of `config/sources.toml`. Mirrors
/// [`SourceDescriptor`] one-for-one but keeps `authoritative_for`
/// optional so simple entries don't need to declare it.
#[derive(Debug, Deserialize)]
pub struct SourcesFile {
    #[serde(default)]
    pub source: Vec<SourceEntry>,
}

/// A single `[[source]]` table in the TOML.
#[derive(Debug, Deserialize)]
pub struct SourceEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub authoritative_for: Vec<String>,
    /// Optional URL the fetch executor pre-fetches at recipe-authoring
    /// time. See Session 10 handoff §"Top: F" — without this, the
    /// LLM tends to keep `https://example.invalid/{id}` placeholders.
    /// `None` is legal; the executor falls back to a placeholder URL
    /// and a stub excerpt and continues. ADR 0014 chips the resulting
    /// recipe so the operator can see the provenance.
    #[serde(default)]
    pub endpoint_hint: Option<String>,
}

/// Load up to `limit` source descriptors from the TOML file at `path`.
///
/// Returns an empty `Vec` if the file does not exist (and emits a
/// warning via `tracing::warn!`). Returns an error only if the file
/// exists but cannot be read or cannot be parsed as the expected TOML
/// schema; the caller should propagate the error so a *malformed*
/// `sources.toml` produces a loud boot failure rather than a silent
/// "no sources known" classification.
pub fn load_source_descriptors(path: &Path, limit: usize) -> Result<Vec<SourceDescriptor>> {
    if !path.exists() {
        // Missing file is not an error — the classifier handles an
        // empty list by telling the LLM to nominate by description
        // only. We log a warning so users notice if they expected it
        // to load.
        warn!(
            path = %path.display(),
            "sources file not found; classifier will see no registered sources"
        );
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let file: SourcesFile = toml::from_str(&raw)
        .with_context(|| format!("parsing TOML in {}", path.display()))?;

    let descriptors: Vec<SourceDescriptor> = file
        .source
        .into_iter()
        .take(limit)
        .map(|e| SourceDescriptor {
            id: e.id,
            display_name: e.display_name,
            description: e.description.trim().to_string(),
            authoritative_for: e.authoritative_for,
            endpoint_hint: e
                .endpoint_hint
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        })
        .collect();

    Ok(descriptors)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These are the tests that lived alongside the old per-binary copies
// of the loader (the CLI had them; the desktop binary had no separate
// copy). All eight pre-existing tests are preserved verbatim except
// for the path-resolution test, which now points at the workspace
// root from this crate's location (two levels up). One new test added
// in Session 24 — `parses_entry_without_endpoint_hint_documents_omission`
// — round-trips a description with the kind of explicit "no hint
// because…" prose the LME entry now uses, locking the convention
// against accidental schema drift.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn load_source_descriptors_reads_real_config_file() {
        // The real config file should always parse — if this fails,
        // someone broke the schema or removed a required field.
        let path = Path::new("../../config/sources.toml");
        if !path.exists() {
            // Tests may be run from various CWDs; skip cleanly rather
            // than fail when the relative path doesn't resolve.
            return;
        }
        let out = load_source_descriptors(path, 100).expect("real config should parse");
        assert!(!out.is_empty(), "real config should have at least one source");
    }

    #[test]
    fn load_source_descriptors_returns_empty_for_missing_file() {
        let out = load_source_descriptors(Path::new("/nonexistent/path/sources.toml"), 10).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn load_source_descriptors_respects_limit() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "a"
display_name = "A"
description = "first"

[[source]]
id = "b"
display_name = "B"
description = "second"

[[source]]
id = "c"
display_name = "C"
description = "third"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
    }

    #[test]
    fn load_source_descriptors_handles_empty_authoritative_for() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "x"
display_name = "X"
description = "no authority field"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].authoritative_for.is_empty());
    }

    #[test]
    fn load_source_descriptors_trims_description_whitespace() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "y"
display_name = "Y"
description = """

  Indented description.

"""
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out[0].description, "Indented description.");
    }

    #[test]
    fn load_source_descriptors_parses_endpoint_hint_when_present() {
        // Session 10, Option F: an `endpoint_hint` survives TOML
        // parsing and lands on the descriptor. Whitespace is trimmed
        // (TOML allows multi-line strings; descriptions use them and
        // we want endpoint_hints to behave consistently).
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "wb"
display_name = "World Bank"
description = "indicators"
endpoint_hint = "https://api.worldbank.org/v2/indicator?format=json"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out[0].id, "wb");
        assert_eq!(
            out[0].endpoint_hint.as_deref(),
            Some("https://api.worldbank.org/v2/indicator?format=json")
        );
    }

    #[test]
    fn load_source_descriptors_treats_empty_endpoint_hint_as_absent() {
        // Empty / whitespace-only endpoint_hint normalizes to None so
        // the executor's lookup path doesn't see a useless empty string.
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "blank"
display_name = "Blank"
description = "no useful hint"
endpoint_hint = "   "
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert!(out[0].endpoint_hint.is_none());
    }

    #[test]
    fn load_source_descriptors_defaults_endpoint_hint_to_none() {
        // Carry-over: existing TOML entries without an `endpoint_hint`
        // line stay parseable. Guards against accidentally making the
        // field required.
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "minimal"
display_name = "Minimal"
description = "nothing else set"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert!(out[0].endpoint_hint.is_none());
    }

    /// New in Session 24. Locks the prose convention adopted by the
    /// LME entry in `config/sources.toml` after the P1 audit: when
    /// no endpoint_hint exists for a real architectural reason
    /// (paywalled subscription product, no public face the LLM can
    /// pre-fetch), the *description* itself documents the omission
    /// so the next operator and the next prompt revision can see
    /// that the absence is deliberate. The loader stays
    /// schema-equivalent — nothing mechanical changes — but a test
    /// here makes it harder for someone in a future session to
    /// "clean up" a long description and accidentally strip the
    /// reasoning.
    #[test]
    fn parses_entry_without_endpoint_hint_documents_omission() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "lme_like"
display_name = "Paywalled Source"
description = """
A made-up source with no public URL. The description carries the
reason for the omission so the next operator can see it.

NOTE on `endpoint_hint`: omitted intentionally. The data is
behind a vendor paywall.
"""
authoritative_for = ["industrial_metals_prices"]
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out.len(), 1);
        let entry = &out[0];
        assert_eq!(entry.id, "lme_like");
        assert!(entry.endpoint_hint.is_none(), "no hint should round-trip as None");
        // The description body survives intact — the loader does not
        // strip the explanatory paragraph.
        assert!(
            entry.description.contains("NOTE on `endpoint_hint`"),
            "description should preserve the omission rationale; got {:?}",
            entry.description
        );
        assert!(
            entry.description.contains("paywall"),
            "description should preserve the architectural reason; got {:?}",
            entry.description
        );
    }

    /// Tiny in-process tempdir helper. We don't pull in `tempfile`
    /// for one test fixture; this is enough.
    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce: u64 = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        };
        p.push(format!("situation_room_apps_common_test_{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        // The dir leaks on test crash; acceptable for now.
        p
    }
}
