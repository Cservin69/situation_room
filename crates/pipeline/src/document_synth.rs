//! Per-fetch Document synthesis (Session 69).
//!
//! Every successful recipe fetch produces one `Document` row capturing
//! the raw page the executor pulled from the URL — kind from MIME,
//! body truncated to a preview-friendly cap, provenance keyed so the
//! plan dashboard picks it up via `records_for_plan`'s recipe-id
//! `LIKE` join.
//!
//! ## Why this lives outside recipe_apply
//!
//! `recipe_apply::build_record` explicitly rejects `RecordType::Document`:
//! Documents are "raw content as fetched," not "extracted via field
//! mappings," and the recipe author prompt never offers a Document
//! template. The split is intentional — the recipe describes the
//! extraction; the Document is the input to that extraction. Both
//! deserve to be persisted, but they're produced at different layers.
//!
//! Before this module, the gap meant the Documents bucket was zero on
//! every plan regardless of recipe success — operators saw 10 OBSERVATIONS
//! but 0 DOCUMENTS on the dashboard even though the executor had every
//! byte needed to fill it.
//!
//! ## Scope
//!
//! - Called from each `run_X_recipe` in `fetch_executor.rs` immediately
//!   after `fetch_recipe_bytes` returns `Ok`. The Document lands
//!   whether apply succeeds or fails (a fetched-but-unextractable
//!   page is still useful evidence; the operator can inspect it via
//!   the dashboard).
//! - Insert failure is warn-logged, never fatal. Same posture as
//!   `record_apply_failure_attempt`: auxiliary persistence must not
//!   break the runtime path.
//!
//! ## What this module does NOT do
//!
//! - Dedup across re-fetches. The `documents` table has no UNIQUE on
//!   `dedup_key`; each fetch run adds a row. That's the right model
//!   for now — Documents are time-versioned (today's page ≠ tomorrow's
//!   page) and the operator wants to see fetch-by-fetch history. A
//!   future session can layer upsert if storage volume becomes an
//!   issue.
//! - Title / author extraction. Parsing HTML to pluck `<title>` or
//!   schema.org bylines is a Phase-3 ingest concern. Today's Document
//!   carries enough to render the dashboard tile (kind + mime +
//!   source_url + observed_at + body preview).
//! - HTML/PDF body extraction. We truncate raw bytes to a UTF-8-safe
//!   preview; downstream consumers wanting clean text run a separate
//!   extraction pass. Today the body is "first ~32 KiB of bytes,
//!   stripped of non-UTF-8 garbage" — enough for an operator to
//!   inspect "what did we fetch" without bloating the row to PDF
//!   sizes.

use chrono::{DateTime, Utc};
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::Document;
use situation_room_core::vocab::Confidence;
use situation_room_storage::Store;
use tracing::warn;
use uuid::Uuid;

use crate::recipes::FetchRecipe;
use crate::research::ResearchPlan;

/// Maximum bytes of fetched content we copy into a Document's `body`
/// for preview. 32 KiB is large enough for a typical news article or
/// JSON response (median observed size in production: ~12 KiB) and
/// small enough to keep the documents table from ballooning when a
/// plan re-fetches the same recipe daily.
///
/// Binary MIMEs (PDF, images) get an empty body — the raw file is
/// not human-readable inline, and an extraction layer that materialises
/// the text representation will be Phase-3 work (see module-level
/// docs). The dashboard's Document tile renders kind + source_url even
/// for empty-body rows, so the operator still sees "we fetched a PDF
/// from URL X at time T."
pub const BODY_PREVIEW_CAP_BYTES: usize = 32 * 1024;

/// Synthesize and insert a `Document` row capturing a successful fetch.
///
/// Returns `()` regardless of insert outcome — Document persistence is
/// auxiliary to the runtime path and must not surface as a recipe
/// failure. On insert error we warn-log with the recipe id and bytes
/// length so the operator (and the eventual `cargo run` log reader)
/// can correlate, but the caller continues to apply + insert the
/// extracted records.
pub fn insert_fetch_document(
    store: &Store,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) {
    let document = build_fetch_document(plan, recipe, bytes, response_content_type, fetched_at);
    if let Err(e) = store.insert_document(&document) {
        warn!(
            recipe_id = %recipe.id,
            bytes_len = bytes.len(),
            error = %e,
            "failed to persist per-fetch Document; the page-was-fetched evidence is lost \
             for this run but the recipe outcome is preserved"
        );
    }
}

/// Pure-function form: build the Document without touching storage.
/// Split out so unit tests can assert on shape without standing up a
/// DB. The runtime path always goes through
/// [`insert_fetch_document`] which wraps this + the persist call.
pub fn build_fetch_document(
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) -> Document {
    let mime = normalise_mime(response_content_type);
    let kind = document_kind_from_mime(&mime);
    let body = body_preview(&mime, bytes);

    // Provenance uses the same `{source}#recipe:{id}@v{ver}` format
    // that `recipe_apply::build_record` writes for Observations /
    // Events / Relations. The per-plan dashboard's `records_for_plan`
    // SQL filters on this exact substring; matching it is what
    // routes the Document into the right plan's bucket on the
    // dashboard.
    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            recipe.source_id, recipe.id, recipe.version
        ),
        source_url: Some(recipe.source_url.to_string()),
        source_published_at: None,
        license: "unknown".into(),
        derived_from: vec![],
    };

    let subjects = Subjects {
        entities: vec![],
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: None,
        observed_at: fetched_at,
        confidence: Confidence::ONE,
    };

    Document {
        id: Uuid::now_v7(),
        // dedup_key carries the canonical "what page is this" identity
        // — source URL. Today there's no UNIQUE constraint on the
        // column, so re-fetches insert distinct rows (which is the
        // right semantic for time-versioned page captures). If a
        // future session adds upsert-on-conflict to documents, this
        // is the key it should use.
        dedup_key: Some(recipe.source_url.to_string()),
        title: None,
        kind,
        mime,
        body,
        published_at: None,
        author: None,
        envelope,
    }
}

/// Normalise a `Content-Type` header value to a lowercase MIME without
/// parameters. `text/html; charset=utf-8` → `text/html`. Missing /
/// empty header → `application/octet-stream` (the HTTP-spec default
/// for "I don't know"). Static-payload recipes have `None` here too,
/// so they also fall through to the octet-stream default.
fn normalise_mime(raw: Option<&str>) -> String {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() {
        return "application/octet-stream".to_string();
    }
    s.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Map a normalised MIME to a Document `kind` string.
///
/// The `kind` vocabulary is the open snake_case set named in
/// `core::schema::records::Document` (article / filing / transcript /
/// press_release / tweet / research_note / satellite_image_caption)
/// extended with `data_feed` for structured API responses — the
/// existing vocabulary has nothing that fits a JSON / CSV / XML feed,
/// and "data_feed" reads cleanly in the UI bucket tile. The closed
/// vocabulary discipline (`project_sr_no_source_routing`) is about
/// host strings; document kind is downstream of MIME, which is
/// already open by definition.
pub fn document_kind_from_mime(mime: &str) -> String {
    let m = mime.trim().to_ascii_lowercase();
    if m.starts_with("text/html") || m.starts_with("application/xhtml") {
        "article".to_string()
    } else if m == "application/pdf" {
        "filing".to_string()
    } else if m.starts_with("application/json")
        || m.starts_with("text/json")
        || m.starts_with("application/xml")
        || m.starts_with("text/xml")
        || m.starts_with("text/csv")
        || m == "text/tab-separated-values"
        || m == "application/vnd.ms-excel"
    {
        "data_feed".to_string()
    } else if m.starts_with("text/plain") {
        "transcript".to_string()
    } else {
        // Unknown / binary / image / video / future MIMEs all fall
        // through to data_feed. Picking the "structured but not
        // human-readable inline" bucket gives the operator a hint
        // ("this isn't an article") without us guessing wrong.
        "data_feed".to_string()
    }
}

/// Produce a UTF-8-safe text preview of `bytes`, capped at
/// `BODY_PREVIEW_CAP_BYTES`. For binary MIMEs (PDF, image, video,
/// octet-stream) returns an empty string — there's no useful inline
/// text representation and forcing UTF-8 through a PDF stream just
/// produces garbage.
///
/// For text MIMEs we keep the longest valid-UTF-8 prefix of the cap.
/// `std::str::from_utf8`'s `valid_up_to()` gives the exact boundary —
/// no manual byte-walking, no chance of leaving a truncated codepoint
/// in the slice. Trailing invalid bytes (a mid-codepoint truncation or
/// genuine mojibake) are dropped, which is the right move for a
/// preview: we'd rather show 32 KiB minus a few bytes of valid text
/// than 32 KiB with a replacement-char tail.
fn body_preview(mime: &str, bytes: &[u8]) -> String {
    if is_binary_mime(mime) {
        return String::new();
    }
    let take = bytes.len().min(BODY_PREVIEW_CAP_BYTES);
    let slice = &bytes[..take];
    match std::str::from_utf8(slice) {
        Ok(s) => s.to_string(),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            // Safety: valid_up_to() is documented as the longest valid
            // UTF-8 prefix length, so this from_utf8 always succeeds.
            std::str::from_utf8(&slice[..valid_up_to])
                .expect("valid_up_to is the valid prefix by definition")
                .to_string()
        }
    }
}

/// Decide whether a MIME's body has no useful inline text shape.
///
/// PDFs *do* contain text but the raw bytes are a binary stream; an
/// extraction pass to pull the text representation is downstream work.
/// Today we leave PDF body empty so the row tells the operator "we
/// fetched a PDF" without storing garbage. Same for images, video,
/// audio, and the octet-stream default.
fn is_binary_mime(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    m == "application/pdf"
        || m.starts_with("image/")
        || m.starts_with("audio/")
        || m.starts_with("video/")
        || m == "application/octet-stream"
        || m == "application/zip"
        || m == "application/x-tar"
        || m == "application/gzip"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{ExtractionSpec, FetchRecipe};
    use crate::research::{RecordExpectations, ResearchPlan};
    use chrono::TimeZone;
    use situation_room_core::vocab::Topic;
    use url::Url;
    use uuid::Uuid;

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "session-69 document synth".into(),
            interpretation: "test".into(),
            topic_tags: vec![Topic::new("test_topic").unwrap()],
            geographic_scope: vec![],
            historical_window_days: 30,
            expectations: RecordExpectations::default(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 13, 0, 0, 0).unwrap(),
        }
    }

    fn sample_recipe(plan: &ResearchPlan, url: &str, source_id: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:{}:demo", plan.id, source_id)),
            plan_id: plan.id,
            source_id: source_id.into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::CsvCell {
                column: "value".into(),
                row_filter: None,
            },
            iterator: None,
            produces: vec![],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 13, 0, 0, 0).unwrap(),
            authored_by: "session-69-test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    #[test]
    fn document_kind_from_mime_routes_html_to_article() {
        assert_eq!(document_kind_from_mime("text/html"), "article");
        assert_eq!(document_kind_from_mime("text/html; charset=utf-8"), "article");
        assert_eq!(document_kind_from_mime("TEXT/HTML"), "article");
        assert_eq!(document_kind_from_mime("application/xhtml+xml"), "article");
    }

    #[test]
    fn document_kind_from_mime_routes_pdf_to_filing() {
        assert_eq!(document_kind_from_mime("application/pdf"), "filing");
    }

    #[test]
    fn document_kind_from_mime_routes_structured_to_data_feed() {
        assert_eq!(document_kind_from_mime("application/json"), "data_feed");
        assert_eq!(document_kind_from_mime("text/csv"), "data_feed");
        assert_eq!(document_kind_from_mime("application/xml"), "data_feed");
        assert_eq!(document_kind_from_mime("text/xml"), "data_feed");
        assert_eq!(document_kind_from_mime("application/vnd.ms-excel"), "data_feed");
    }

    #[test]
    fn document_kind_from_mime_routes_plain_to_transcript() {
        assert_eq!(document_kind_from_mime("text/plain"), "transcript");
        assert_eq!(document_kind_from_mime("text/plain; charset=utf-8"), "transcript");
    }

    #[test]
    fn document_kind_from_mime_unknown_falls_back_to_data_feed() {
        // Unfamiliar MIMEs are still records of "something was fetched";
        // we don't refuse to classify, we pick the safest catch-all.
        assert_eq!(document_kind_from_mime("application/x-protobuf"), "data_feed");
        assert_eq!(document_kind_from_mime(""), "data_feed");
    }

    #[test]
    fn normalise_mime_strips_parameters_and_lowercases() {
        assert_eq!(normalise_mime(Some("Text/HTML; charset=UTF-8")), "text/html");
        assert_eq!(normalise_mime(Some(" application/json ")), "application/json");
    }

    #[test]
    fn normalise_mime_defaults_to_octet_stream_when_absent() {
        assert_eq!(normalise_mime(None), "application/octet-stream");
        assert_eq!(normalise_mime(Some("")), "application/octet-stream");
        assert_eq!(normalise_mime(Some("   ")), "application/octet-stream");
    }

    #[test]
    fn body_preview_returns_empty_for_pdf() {
        let pdf_bytes = b"%PDF-1.4\n... binary garbage ...";
        assert_eq!(body_preview("application/pdf", pdf_bytes), "");
    }

    #[test]
    fn body_preview_returns_empty_for_image() {
        assert_eq!(body_preview("image/png", &[0x89, 0x50, 0x4e, 0x47]), "");
    }

    #[test]
    fn body_preview_caps_long_text_at_32_kib() {
        let big = "a".repeat(100 * 1024);
        let out = body_preview("text/html", big.as_bytes());
        assert_eq!(out.len(), BODY_PREVIEW_CAP_BYTES);
    }

    #[test]
    fn body_preview_preserves_short_html() {
        let html = b"<html><body>hello</body></html>";
        assert_eq!(body_preview("text/html", html), "<html><body>hello</body></html>");
    }

    #[test]
    fn body_preview_does_not_split_multibyte_codepoint() {
        // Prepend a single ASCII byte to a stream of "é" (2 bytes each,
        // 0xC3 0xA9). Cap = 32 KiB, so the slice ends at byte 32768,
        // which lands mid-"é" (between the 0xC3 lead and the 0xA9
        // continuation). A correct implementation drops that trailing
        // partial codepoint; a buggy one leaves it and `from_utf8_lossy`
        // produces a U+FFFD replacement char.
        let mut bytes = vec![b'A'];
        bytes.extend("é".repeat(BODY_PREVIEW_CAP_BYTES).as_bytes());
        let out = body_preview("text/plain", &bytes);

        assert!(!out.contains('\u{FFFD}'), "preview corrupted a codepoint: {out:?}");
        // The clean prefix is 1 ASCII byte + 16383 full "é"s = 32767
        // bytes; the 16384th "é"'s lead byte at index 32767 gets dropped
        // because the continuation byte is past the cap.
        assert_eq!(out.len(), 32767);
        assert!(out.starts_with('A'));
        assert!(out.ends_with('é'));
    }

    #[test]
    fn build_fetch_document_attaches_recipe_routed_provenance() {
        // The whole point: provenance.source_id must match the
        // `%#recipe:<id>@v%` LIKE pattern used by
        // `records_for_plan`, or the Document won't show up on the
        // plan's dashboard. This test pins the format so a future
        // refactor that changes the source_id shape would either
        // update both sites or break this test loudly.
        let plan = sample_plan();
        let recipe = sample_recipe(&plan, "https://example.test/index.html", "example_site");
        let bytes = b"<html><body>hi</body></html>";
        let at = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();

        let doc = build_fetch_document(&plan, &recipe, bytes, Some("text/html"), at);

        assert_eq!(doc.kind, "article");
        assert_eq!(doc.mime, "text/html");
        assert_eq!(doc.body, "<html><body>hi</body></html>");
        assert_eq!(doc.envelope.observed_at, at);
        assert_eq!(
            doc.envelope.provenance.source_id,
            format!("example_site#recipe:{}@v1", recipe.id)
        );
        assert_eq!(
            doc.envelope.provenance.source_url.as_deref(),
            Some("https://example.test/index.html")
        );
        assert_eq!(doc.dedup_key.as_deref(), Some("https://example.test/index.html"));
        // Plan topic tags flow onto the document envelope so the
        // dashboard's per-topic filters work the same way they do
        // for Observations.
        assert_eq!(doc.envelope.subjects.topics, plan.topic_tags);
    }

    #[test]
    fn build_fetch_document_handles_missing_content_type() {
        // Static-payload recipes have no transport so no Content-Type.
        // We must still produce a renderable Document — the operator
        // sees a row tagged `data_feed` and `application/octet-stream`
        // rather than no Document at all.
        let plan = sample_plan();
        let recipe = sample_recipe(&plan, "https://example.test/static", "baked");
        let at = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let doc = build_fetch_document(&plan, &recipe, b"", None, at);
        assert_eq!(doc.mime, "application/octet-stream");
        assert_eq!(doc.kind, "data_feed");
        assert_eq!(doc.body, ""); // octet-stream is treated as binary
    }

    #[test]
    fn build_fetch_document_leaves_pdf_body_empty() {
        let plan = sample_plan();
        let recipe = sample_recipe(&plan, "https://example.test/report.pdf", "annual_report");
        let pdf = b"%PDF-1.4\n... binary content ...";
        let at = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let doc = build_fetch_document(&plan, &recipe, pdf, Some("application/pdf"), at);
        assert_eq!(doc.kind, "filing");
        assert_eq!(doc.mime, "application/pdf");
        assert_eq!(doc.body, "");
    }
}
