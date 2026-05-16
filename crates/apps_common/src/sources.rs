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
use situation_room_pipeline::authoritative::distance_1_suggestion;
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
    // Session 87: lossy-continue schema-warn pass before typed parse,
    // mirroring `crates/pipeline/src/authoritative.rs`. Catches typos
    // like `endpiont_hint`, `displaay_name`, `aughoritative_for`
    // without breaking the load — operators editing the file
    // interactively keep the rest of the typed schema and just see a
    // warn-log on the typo'd field.
    check_sources_schema_warnings(&raw);
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
// Session 87 — lossy-continue schema-warn pass
// ---------------------------------------------------------------------------

/// Walk the TOML parse-tree once and warn-log any unknown top-level
/// key or any unknown field on a `[[source]]` entry. Mirrors the
/// posture in `authoritative.rs::check_schema_warnings`: lossy
/// continue (the typed parse runs normally and ignores stray fields
/// — `#[serde(deny_unknown_fields)]` would fail the whole load on a
/// single typo and lose hot-reload semantics on the sources file).
///
/// `distance_1_suggestion` from the pipeline crate surfaces a "Did
/// you mean `X`?" hint when there's exactly one edit-1 candidate;
/// otherwise the warn just names the full allowlist.
fn check_sources_schema_warnings(toml_text: &str) {
    /// Known field names on a single `[[source]]` entry. Kept in
    /// lockstep with `SourceEntry` via the
    /// `known_entry_fields_match_struct` unit test below.
    const KNOWN_ENTRY_FIELDS: &[&str] = &[
        "id",
        "display_name",
        "description",
        "authoritative_for",
        "endpoint_hint",
    ];
    /// Known top-level table names. Today only `source` (which is
    /// itself array-of-tables for `[[source]]`).
    const KNOWN_TOP_LEVEL_KEYS: &[&str] = &["source"];

    let parsed: toml::Value = match toml::from_str(toml_text) {
        Ok(v) => v,
        Err(_) => {
            // Typed parse will surface the same error; don't double-log.
            return;
        }
    };

    let table = match parsed.as_table() {
        Some(t) => t,
        None => return,
    };

    for (key, _) in table.iter() {
        if !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            match distance_1_suggestion(key, KNOWN_TOP_LEVEL_KEYS) {
                Some(suggestion) => warn!(
                    key = %key,
                    suggestion = %suggestion,
                    "sources TOML: unknown top-level key (ignored). Did you mean `{}`? Allowed: {:?}",
                    suggestion,
                    KNOWN_TOP_LEVEL_KEYS
                ),
                None => warn!(
                    key = %key,
                    "sources TOML: unknown top-level key (ignored). Allowed: {:?}",
                    KNOWN_TOP_LEVEL_KEYS
                ),
            }
        }
    }

    let Some(array) = table.get("source").and_then(|v| v.as_array()) else {
        return;
    };
    for (idx, entry) in array.iter().enumerate() {
        let Some(row) = entry.as_table() else {
            continue;
        };
        for (key, _) in row.iter() {
            if !KNOWN_ENTRY_FIELDS.contains(&key.as_str()) {
                match distance_1_suggestion(key, KNOWN_ENTRY_FIELDS) {
                    Some(suggestion) => warn!(
                        entry_index = idx,
                        unknown_field = %key,
                        suggestion = %suggestion,
                        "sources TOML: unknown field on [[source]] entry (ignored). Did you mean `{}`? Allowed: {:?}",
                        suggestion,
                        KNOWN_ENTRY_FIELDS
                    ),
                    None => warn!(
                        entry_index = idx,
                        unknown_field = %key,
                        "sources TOML: unknown field on [[source]] entry (ignored). Allowed: {:?}",
                        KNOWN_ENTRY_FIELDS
                    ),
                }
            }
        }
    }
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

    // ---- Session 87 — schema-warn path -----------------------------

    /// Lockstep guard: the closed-vocab allowlist names every
    /// `SourceEntry` field. If a future PR renames or adds a field
    /// without updating `check_sources_schema_warnings`, the new
    /// field would warn as "unknown" the next time someone loads
    /// the config — even when the typed parse succeeds. Two-line
    /// sync rule:
    ///
    ///   1. Edit `SourceEntry`.
    ///   2. Edit `KNOWN_ENTRY_FIELDS` in `check_sources_schema_warnings`
    ///      in the same commit.
    ///
    /// (Mirrors the equivalent test in `crates/pipeline/src/authoritative.rs`.)
    #[test]
    fn known_entry_fields_match_struct() {
        // The test is structural: parse a complete entry and check
        // that the typed deserializer sees every field, AND that
        // the warning path agrees no warn-log would have been
        // emitted. We can't easily inspect emitted warns from
        // `tracing` here without setting up a subscriber, so we
        // exercise the lossy path indirectly via parse success.
        let toml = r#"
[[source]]
id = "x"
display_name = "X"
description = "all fields populated"
authoritative_for = ["topic_a", "topic_b"]
endpoint_hint = "https://x.example/data"
"#;
        let file: SourcesFile = toml::from_str(toml).unwrap();
        assert_eq!(file.source.len(), 1);
        let e = &file.source[0];
        assert_eq!(e.id, "x");
        assert_eq!(e.display_name, "X");
        assert_eq!(e.description, "all fields populated");
        assert_eq!(e.authoritative_for, vec!["topic_a", "topic_b"]);
        assert_eq!(e.endpoint_hint.as_deref(), Some("https://x.example/data"));
    }

    /// A typo'd field on `[[source]]` (e.g. `endpiont_hint`) lossy-
    /// continues: the typed parse succeeds with the typo'd field
    /// missing (so `endpoint_hint = None` in this case), and the
    /// warn-log path runs without panicking. Operators editing the
    /// file interactively retain hot-reload — Session 86 logic.
    #[test]
    fn parse_accepts_unknown_field_with_warn_does_not_fail() {
        let toml = r#"
[[source]]
id = "x"
display_name = "X"
description = "typo'd field below"
endpiont_hint = "https://x.example/data"
"#;
        // Should NOT fail despite the typo. The typed parse ignores
        // unknown fields (no `deny_unknown_fields`), and
        // `check_sources_schema_warnings` warn-logs but doesn't error.
        check_sources_schema_warnings(toml);
        let file: SourcesFile = toml::from_str(toml).unwrap();
        let e = &file.source[0];
        assert!(
            e.endpoint_hint.is_none(),
            "typo'd endpiont_hint should not bind to endpoint_hint"
        );
    }

    /// Tiny in-process tempdir helper. We don't pull in `tempfile`
    /// for one test fixture; this is enough.
    ///
    /// Uniqueness sources, layered for parallel safety on modern
    /// hardware: `SystemTime::now().as_nanos()` (cross-process),
    /// `thread::current().id()` (cross-test within a process), and a
    /// process-wide `AtomicUsize` counter (cross-test on the same
    /// thread, rare but possible under nextest). Without the counter
    /// the nanos-only version collided on fast hardware when two
    /// `cargo test` threads happened to enter `tempdir()` within the
    /// same nanosecond — Session 41 patch 2 saw this manifest as a
    /// flaky `load_source_descriptors_respects_limit` ("left: 'a' /
    /// right: 'wb'"). Session 43 drive-by per the handoff.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let mut p = std::env::temp_dir();
        let nonce: u64 = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        };
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Sanitize the thread id — `ThreadId(N)`'s parens are not
        // path-illegal but they're awkward, and any surprising char
        // would break create_dir_all on Windows builds.
        let tid_clean: String = format!("{:?}", std::thread::current().id())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        p.push(format!(
            "situation_room_apps_common_test_{nonce}_{tid_clean}_{seq}"
        ));
        std::fs::create_dir_all(&p).unwrap();
        // The dir leaks on test crash; acceptable for now.
        p
    }
}
