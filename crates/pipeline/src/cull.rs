//! Operator-triggered cull pass for boilerplate-shaped Assertions
//! (Session 93, ADR 0023 follow-on).
//!
//! ## Why this module exists
//!
//! Sn-91's measurement found 7/7 singleton relation triples in the
//! global-aluminium plan traced to a topic-index URL whose body was
//! navigation chrome rather than article prose. ADR 0023's multi-
//! claimant prompt has no attribution to extract when the bytes are
//! a listing. The Sn-93 follow-the-link work
//! ([`crate::index_page_detector`]) stops *new* such Assertions from
//! being written, but the existing pile needs an operator-triggered
//! cull pass.
//!
//! ## What this module does
//!
//! [`cull_index_assertions_for_plan`] iterates a plan's Assertion
//! rows, looks up the source Document each one was extracted from,
//! re-runs the index-page detector against the Document's body, and
//! deletes the Assertion when the detector returns
//! [`crate::index_page_detector::IndexPageSignal::Index`]. The
//! detector is read-only and structural; the deletion side-effect is
//! the only write.
//!
//! [`sample_index_assertions_for_plan`] runs the same scan in
//! read-only mode and returns up to N candidate Assertion ids + their
//! source-Document hostless paths + the detector's reason — the UI
//! shows this list as a preview before the operator confirms the
//! destructive pass. Per the Sn-93 verify runbook's COST WARNING
//! shape: never delete without showing what would go.
//!
//! ## Closed-vocab discipline
//!
//! The detector module is the only place hosts could leak in, and it
//! intentionally doesn't read them. This module touches no host
//! strings either — Documents are routed back to source bytes via
//! the dashboard's existing `records_for_plan` join, the same path
//! [`crate::reextract`] uses.
//!
//! ## Idempotency
//!
//! Deletion is `DELETE … WHERE id = ?` — re-running the cull pass
//! over the same plan after a successful sweep is a no-op (the rows
//! the detector would target are gone). The preview path is always
//! safe to run.

use serde::{Deserialize, Serialize};
use situation_room_storage::Store;
use tracing::{info, warn};
use uuid::Uuid;

use crate::index_page_detector::{classify_fetched_bytes, IndexPageSignal};

/// Per-plan cull summary, surfaced to the dashboard. Matches the
/// shape of [`crate::reextract::ReextractReport`] for consistent UI
/// rendering — both are operator-triggered Assertion-side passes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CullReport {
    /// Assertions visited by the scan (this plan's full Assertion
    /// pile, regardless of routing or detector outcome).
    pub assertions_considered: u32,
    /// Assertions whose source Document could not be located inside
    /// this plan's record set — skipped. Typically rows whose source
    /// Document was deleted, or whose source_id shape doesn't route.
    pub assertions_unrouted: u32,
    /// Assertions whose source Document scored `Index` and were
    /// deleted from storage.
    pub assertions_culled: u32,
    /// Assertions whose source Document scored `Article` and were
    /// kept. The proper outcome for prose-shaped sources.
    pub assertions_kept_article: u32,
    /// Assertions whose source Document scored `Unknown` (non-HTML
    /// MIME, sparse body, non-UTF-8 bytes). Kept — `Unknown` is a
    /// "don't cull on this signal" verdict by design.
    pub assertions_kept_unknown: u32,
    /// Per-Assertion `delete_assertion` errors. Should be zero in
    /// steady state.
    pub delete_failures: u32,
}

/// Read-only preview of which Assertions the cull pass *would*
/// delete. Surfaced to the UI before the operator clicks "cull".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CullPreviewItem {
    /// Assertion row id — the operator can correlate this with the
    /// Assertions panel on the dashboard.
    pub assertion_id: Uuid,
    /// content_kind of the Assertion (`relation`, `entity_attribute`,
    /// `observation`, `event` — the closed-vocab tag from
    /// `AssertedContent`).
    pub content_kind: &'static str,
    /// Short hostless path of the source Document's URL, for the UI
    /// to render alongside the Assertion id without leaking host
    /// strings into log fields. Truncated to 80 chars.
    pub source_path: String,
    /// The detector's verdict label (`"index"`).
    pub detector_signal: &'static str,
}

/// Default soft cap on the preview's length. The UI surfaces this
/// many candidates at most so the operator isn't paging through
/// thousands of rows; the real cull pass is uncapped (it runs to
/// completion on click).
pub const DEFAULT_PREVIEW_CAP: usize = 50;

/// Iterate this plan's Assertions, re-classify each one's source
/// Document with [`classify_fetched_bytes`], and delete every
/// Assertion whose Document scored `Index`. Returns a [`CullReport`].
///
/// **Idempotent.** Re-running after a successful pass is a no-op:
/// the rows the detector would target are gone.
///
/// **Read side-effects.** None — the detector is a pure function of
/// (bytes, mime, url). The only write is `delete_assertion`.
pub fn cull_index_assertions_for_plan(store: &Store, plan_id: Uuid) -> CullReport {
    let mut report = CullReport::default();

    let records = match store.records_for_plan(plan_id) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                plan_id = %plan_id,
                error = %e,
                "cull: records_for_plan failed; nothing to scan"
            );
            return report;
        }
    };

    // Build a Document lookup keyed by source_id so each Assertion's
    // O(1) lookup doesn't degenerate to O(D) per row. Multiple
    // Documents may share a source_id (each refresh writes one); we
    // keep the most-recently-observed one because the operator's
    // mental model is "the URL's current shape", not "the historical
    // shape at extraction time". Page restructures are rare enough
    // that the latest snapshot reflects the present state.
    let doc_by_source_id =
        latest_document_per_source_id(&records.documents);

    for assertion in &records.assertions {
        report.assertions_considered += 1;
        let src_id = assertion.envelope.provenance.source_id.as_str();
        let doc = match doc_by_source_id.get(src_id) {
            Some(d) => *d,
            None => {
                report.assertions_unrouted += 1;
                continue;
            }
        };

        let url = doc.envelope.provenance.source_url.as_deref().unwrap_or("");
        let signal = classify_fetched_bytes(doc.body.as_bytes(), doc.mime.as_str(), url);
        match signal {
            IndexPageSignal::Index => {
                match store.delete_assertion(assertion.id) {
                    Ok(()) => {
                        report.assertions_culled += 1;
                    }
                    Err(e) => {
                        report.delete_failures += 1;
                        warn!(
                            plan_id = %plan_id,
                            assertion_id = %assertion.id,
                            error = %e,
                            "cull: delete_assertion failed; row stays"
                        );
                    }
                }
            }
            IndexPageSignal::Article => {
                report.assertions_kept_article += 1;
            }
            IndexPageSignal::Unknown => {
                report.assertions_kept_unknown += 1;
            }
        }
    }

    info!(
        plan_id = %plan_id,
        assertions_considered = report.assertions_considered,
        assertions_unrouted = report.assertions_unrouted,
        assertions_culled = report.assertions_culled,
        assertions_kept_article = report.assertions_kept_article,
        assertions_kept_unknown = report.assertions_kept_unknown,
        delete_failures = report.delete_failures,
        "cull: index-page boilerplate cull complete for plan"
    );

    report
}

/// Read-only preview of [`cull_index_assertions_for_plan`]: walks
/// the same path, scores the same Documents, but returns the
/// candidate list instead of deleting. Capped at
/// [`DEFAULT_PREVIEW_CAP`] items.
pub fn sample_index_assertions_for_plan(
    store: &Store,
    plan_id: Uuid,
    cap: usize,
) -> Vec<CullPreviewItem> {
    let records = match store.records_for_plan(plan_id) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                plan_id = %plan_id,
                error = %e,
                "cull preview: records_for_plan failed; empty list"
            );
            return Vec::new();
        }
    };

    let doc_by_source_id = latest_document_per_source_id(&records.documents);

    let mut out = Vec::new();
    for assertion in &records.assertions {
        if out.len() >= cap {
            break;
        }
        let src_id = assertion.envelope.provenance.source_id.as_str();
        let doc = match doc_by_source_id.get(src_id) {
            Some(d) => *d,
            None => continue,
        };
        let url = doc.envelope.provenance.source_url.as_deref().unwrap_or("");
        let signal = classify_fetched_bytes(doc.body.as_bytes(), doc.mime.as_str(), url);
        if signal != IndexPageSignal::Index {
            continue;
        }
        out.push(CullPreviewItem {
            assertion_id: assertion.id,
            content_kind: content_kind_label(&assertion.content),
            source_path: shorten_path(url, 80),
            detector_signal: signal.label(),
        });
    }
    out
}

/// Index Documents by source_id, preferring the most recently
/// observed one when multiple share the same key. Shared by the
/// preview and the cull pass.
fn latest_document_per_source_id<'a>(
    docs: &'a [situation_room_core::schema::records::Document],
) -> std::collections::HashMap<&'a str, &'a situation_room_core::schema::records::Document> {
    let mut map: std::collections::HashMap<
        &'a str,
        &'a situation_room_core::schema::records::Document,
    > = std::collections::HashMap::new();
    for d in docs {
        let key = d.envelope.provenance.source_id.as_str();
        let take = match map.get(key) {
            Some(existing) => d.envelope.observed_at > existing.envelope.observed_at,
            None => true,
        };
        if take {
            map.insert(key, d);
        }
    }
    map
}

/// Closed-vocab label for the AssertedContent variants — same
/// strings storage's `content_kind` column uses for the discriminator
/// (see `crates/storage/src/assertions.rs::content_kind_of`).
fn content_kind_label(content: &situation_room_core::schema::content::AssertedContent) -> &'static str {
    use situation_room_core::schema::content::AssertedContent;
    match content {
        AssertedContent::Observation(_) => "observation",
        AssertedContent::Event(_) => "event",
        AssertedContent::Relation(_) => "relation",
        AssertedContent::EntityAttribute(_) => "entity_attribute",
    }
}

/// Render a URL as its hostless path + at most `max_chars` codepoints
/// so the UI can show "what page" without leaking the host into the
/// preview. A URL without `://` (relative / malformed) is returned
/// as-is, truncated.
fn shorten_path(url: &str, max_chars: usize) -> String {
    let path = match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[i + 3 + j..],
            None => "/",
        },
        None => url,
    };
    if path.chars().count() <= max_chars {
        path.to_string()
    } else {
        let s: String = path.chars().take(max_chars - 1).collect();
        format!("{s}…")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cull_report_default_is_zeroed() {
        let r = CullReport::default();
        assert_eq!(r.assertions_considered, 0);
        assert_eq!(r.assertions_unrouted, 0);
        assert_eq!(r.assertions_culled, 0);
        assert_eq!(r.assertions_kept_article, 0);
        assert_eq!(r.assertions_kept_unknown, 0);
        assert_eq!(r.delete_failures, 0);
    }

    #[test]
    fn cull_report_serde_roundtrip() {
        let r = CullReport {
            assertions_considered: 17,
            assertions_unrouted: 0,
            assertions_culled: 7,
            assertions_kept_article: 8,
            assertions_kept_unknown: 2,
            delete_failures: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: CullReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn shorten_path_strips_host() {
        assert_eq!(
            shorten_path("https://www.example.com/topic/aluminium", 80),
            "/topic/aluminium"
        );
    }

    #[test]
    fn shorten_path_truncates_with_ellipsis() {
        let long = "/".to_string() + &"a".repeat(200);
        let url = format!("https://example.com{long}");
        let out = shorten_path(&url, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn shorten_path_handles_url_without_path() {
        assert_eq!(shorten_path("https://example.com", 80), "/");
    }

    #[test]
    fn shorten_path_handles_malformed_url() {
        // No `://` ⇒ treat as already-a-path.
        assert_eq!(shorten_path("/topic/x", 80), "/topic/x");
    }

    #[test]
    fn default_preview_cap_is_reasonable() {
        // Defensive: don't accidentally cap at 0 or at 100000.
        assert!((10..=200).contains(&DEFAULT_PREVIEW_CAP));
    }
}
