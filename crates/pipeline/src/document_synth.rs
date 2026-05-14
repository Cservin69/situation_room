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

/// Maximum bytes we *consider* when producing an HTML preview. After
/// stripping `<script>`, `<style>`, and remaining tags, a typical
/// news-article HTML doc shrinks 3-5×, so a 128 KiB window comfortably
/// produces a 32 KiB visible-text preview. We pay one larger UTF-8
/// decode per article-kind Document, then the regex stripper runs
/// against a bounded slice — costs are independent of the upstream
/// page size.
const HTML_STRIP_INPUT_CAP_BYTES: usize = 4 * BODY_PREVIEW_CAP_BYTES;

/// Produce a UTF-8-safe text preview of `bytes`, capped at
/// `BODY_PREVIEW_CAP_BYTES`. For binary MIMEs (PDF, image, video,
/// octet-stream) returns an empty string — there's no useful inline
/// text representation and forcing UTF-8 through a PDF stream just
/// produces garbage.
///
/// ## HTML special-case (Session 70)
///
/// For `text/html` and `application/xhtml*` MIMEs the raw bytes are
/// useless as a dashboard preview (`<!doctype html> <html lang="en-US"
/// theme="auto" data-color-...`). Before the cap we run a scoped
/// strip:
///
///   1. Drop `<script>…</script>` and `<style>…</style>` blocks
///      (case-insensitive, `.` matches `\n`).
///   2. Drop remaining `<…>` tags.
///   3. Decode a small whitelist of HTML entities so the preview
///      reads as natural prose (no `&amp;` or `&nbsp;` artefacts).
///   4. Collapse runs of whitespace to a single space.
///
/// The stripper is closed-vocabulary by construction — no host
/// strings, no source-specific rules; just an HTML-MIME gate. This
/// keeps `project_sr_no_source_routing` discipline intact (MIME-based
/// routing is open by definition; the closed-vocabulary discipline is
/// about host identity).
///
/// For non-HTML text MIMEs (`text/plain`, `application/json`,
/// `text/csv`, …) we keep the pre-Session-70 behaviour: the longest
/// valid-UTF-8 prefix of the byte cap. JSON bodies are routed
/// downstream by `RecordsDashboard.detectTimeSeriesShape` into the
/// sparkline preview, so leaving them as raw JSON is correct.
fn body_preview(mime: &str, bytes: &[u8]) -> String {
    if is_binary_mime(mime) {
        return String::new();
    }
    if is_html_mime(mime) {
        return html_body_preview(bytes);
    }
    text_body_preview(bytes)
}

/// Stripped HTML text preview. Takes up to `HTML_STRIP_INPUT_CAP_BYTES`
/// of UTF-8 input, removes script/style/tag noise, decodes a small
/// entity whitelist, collapses whitespace, then caps at
/// `BODY_PREVIEW_CAP_BYTES` on a char boundary.
fn html_body_preview(bytes: &[u8]) -> String {
    let take = bytes.len().min(HTML_STRIP_INPUT_CAP_BYTES);
    let raw = utf8_lossy_prefix(&bytes[..take]);
    let stripped = strip_html_for_preview(&raw);
    truncate_at_char_boundary(&stripped, BODY_PREVIEW_CAP_BYTES)
}

/// Pre-Session-70 text preview: the longest valid-UTF-8 prefix of the
/// byte cap. `std::str::from_utf8`'s `valid_up_to()` gives the exact
/// boundary — no manual byte-walking, no chance of leaving a truncated
/// codepoint in the slice. Trailing invalid bytes (a mid-codepoint
/// truncation or genuine mojibake) are dropped.
fn text_body_preview(bytes: &[u8]) -> String {
    let take = bytes.len().min(BODY_PREVIEW_CAP_BYTES);
    utf8_lossy_prefix(&bytes[..take])
}

/// Helper: return the longest valid-UTF-8 prefix of `slice` as an owned
/// `String`. Used by both the HTML and plain-text paths.
fn utf8_lossy_prefix(slice: &[u8]) -> String {
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

/// Truncate `s` to at most `max_bytes` bytes, rounding down to the
/// nearest UTF-8 char boundary. After HTML stripping we have valid
/// UTF-8 to begin with, so the boundary check is cheap (`char_indices`
/// walk). Returns the truncated owned String.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk char boundaries forward, keeping track of the largest
    // boundary that's still within the cap. We advance `last` to the
    // byte position *after* each char that fully fits, so the final
    // `last` value is the slice length we return.
    let mut last = 0usize;
    for (idx, ch) in s.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        last = next;
    }
    s[..last].to_string()
}

/// Decide whether a MIME is HTML-ish (article-kind Documents). HTML
/// and XHTML both produce `kind: "article"` upstream
/// (`document_kind_from_mime`); this predicate mirrors that gate so
/// the strip pass runs on exactly the same inputs that the kind
/// classifier labels `article`.
pub fn is_html_mime(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    m.starts_with("text/html") || m.starts_with("application/xhtml")
}

/// Strip HTML markup for preview-quality display.
///
/// The contract is "what would the operator read if they opened this
/// page in a browser, with all the navigation chrome turned off?"
/// Not a full DOM parser — a scoped regex pass that's good enough to
/// surface real article text without dragging `<script>` payloads,
/// CSS, or attribute soup into the dashboard tile.
///
/// ## Why not `scraper`?
///
/// `scraper` is already a workspace dependency for recipe extraction
/// (CSS-select recipes). It would handle this better than regex.
/// We're not using it because:
///   1. The recipe path uses `scraper` against *trusted* recipe-
///      authored selectors. Throwing raw fetched HTML at it on every
///      Document insert adds CPU we don't need.
///   2. Document synthesis runs on the executor's hot path. The
///      regex-based stripper is O(n) byte-walking; `scraper`'s parse
///      + serialise round-trip is markedly more expensive.
///   3. For preview-quality output, "no `<script>` payloads, no tags,
///      decoded entities" is sufficient. Cleaning attribute-only
///      mistakes (e.g. malformed comment delimiters) isn't worth the
///      cost difference.
///
/// If a future use-case wants higher-fidelity preview text (Phase-3
/// extraction layer), the right move is to plumb that pass through
/// `scraper` once and persist the result alongside the raw body, not
/// to upgrade this preview function.
pub fn strip_html_for_preview(input: &str) -> String {
    use std::sync::OnceLock;
    use regex::RegexBuilder;

    static SCRIPT_RE: OnceLock<regex::Regex> = OnceLock::new();
    static STYLE_RE: OnceLock<regex::Regex> = OnceLock::new();
    static COMMENT_RE: OnceLock<regex::Regex> = OnceLock::new();
    static TAG_RE: OnceLock<regex::Regex> = OnceLock::new();
    static WS_RE: OnceLock<regex::Regex> = OnceLock::new();

    let script_re = SCRIPT_RE.get_or_init(|| {
        RegexBuilder::new(r"<script\b[^>]*>.*?</script\s*>")
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()
            .expect("static script regex compiles")
    });
    let style_re = STYLE_RE.get_or_init(|| {
        RegexBuilder::new(r"<style\b[^>]*>.*?</style\s*>")
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()
            .expect("static style regex compiles")
    });
    let comment_re = COMMENT_RE.get_or_init(|| {
        RegexBuilder::new(r"<!--.*?-->")
            .dot_matches_new_line(true)
            .build()
            .expect("static comment regex compiles")
    });
    // Tag stripper: any `<` followed by characters up to `>`. Doesn't
    // try to handle `>` inside quoted attributes — for preview-quality
    // output, "everything between < and the next >" is good enough,
    // and HTML5 doesn't allow `<` inside tag-attribute names anyway.
    let tag_re = TAG_RE
        .get_or_init(|| regex::Regex::new(r"<[^>]*>").expect("static tag regex compiles"));
    let ws_re = WS_RE.get_or_init(|| regex::Regex::new(r"\s+").expect("static ws regex compiles"));

    let s = script_re.replace_all(input, " ");
    let s = style_re.replace_all(&s, " ");
    let s = comment_re.replace_all(&s, " ");
    let s = tag_re.replace_all(&s, " ");
    let s = decode_html_entities(&s);
    let s = ws_re.replace_all(&s, " ");
    s.trim().to_string()
}

/// Decode a small whitelist of HTML entities that show up in the
/// overwhelming majority of news-article bodies. Numeric character
/// references (`&#39;`, `&#x27;`) are handled for the ASCII range —
/// enough to surface curly-quote and dash codepoints without dragging
/// in a full entity table. Unknown entities pass through unchanged so
/// the operator can spot them if a future page uses something exotic.
fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            // Find the terminating ';' within a short window — entity
            // names cap at 8 chars in our whitelist, numeric refs at 7
            // digits. Beyond that, treat the `&` as literal.
            if let Some(rel) = bytes[i + 1..(i + 10).min(bytes.len())]
                .iter()
                .position(|&b| b == b';')
            {
                let entity = &input[i + 1..i + 1 + rel];
                if let Some(decoded) = lookup_named_entity(entity) {
                    out.push_str(decoded);
                    i += 1 + rel + 1;
                    continue;
                }
                if let Some(ch) = lookup_numeric_entity(entity) {
                    out.push(ch);
                    i += 1 + rel + 1;
                    continue;
                }
            }
        }
        // Fast path for the common case of all-ASCII bodies: push the
        // byte directly. The multi-byte branch falls through to push
        // the codepoint starting at `i`.
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            // Multi-byte UTF-8 codepoint: copy the full codepoint.
            let ch = input[i..].chars().next().expect("non-empty");
            let ch_len = ch.len_utf8();
            out.push(ch);
            i += ch_len;
        }
    }
    out
}

fn lookup_named_entity(name: &str) -> Option<&'static str> {
    match name {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some(" "),
        "ndash" => Some("–"),
        "mdash" => Some("—"),
        "hellip" => Some("…"),
        "lsquo" => Some("‘"),
        "rsquo" => Some("’"),
        "ldquo" => Some("“"),
        "rdquo" => Some("”"),
        "copy" => Some("©"),
        "reg" => Some("®"),
        "trade" => Some("™"),
        _ => None,
    }
}

fn lookup_numeric_entity(name: &str) -> Option<char> {
    let digits = name.strip_prefix('#')?;
    let code = if let Some(hex) = digits
        .strip_prefix('x')
        .or_else(|| digits.strip_prefix('X'))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    char::from_u32(code)
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
    fn body_preview_strips_short_html() {
        // Session 70: HTML body preview is now tag-stripped. The
        // pre-Session-70 contract preserved the raw markup; the new
        // contract preserves the visible text. `<html><body>hello</body></html>`
        // collapses to just `hello` — the operator-readable preview
        // we want in the dashboard tile.
        let html = b"<html><body>hello</body></html>";
        assert_eq!(body_preview("text/html", html), "hello");
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
        // Session 70: HTML bodies are tag-stripped at preview time.
        // The raw markup `<html><body>hi</body></html>` collapses
        // to the visible text `hi`.
        assert_eq!(doc.body, "hi");
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

    // --- Session 70: HTML-strip behaviour ---------------------------

    #[test]
    fn is_html_mime_matches_html_and_xhtml() {
        assert!(is_html_mime("text/html"));
        assert!(is_html_mime("text/html; charset=utf-8"));
        assert!(is_html_mime("TEXT/HTML"));
        assert!(is_html_mime("application/xhtml+xml"));
        assert!(!is_html_mime("application/json"));
        assert!(!is_html_mime("text/plain"));
        assert!(!is_html_mime("text/csv"));
    }

    #[test]
    fn strip_html_drops_script_payload() {
        let html = r#"<html><body><p>Headline.</p><script>window.x = "tracker pixel" + 42;</script><p>Body.</p></body></html>"#;
        let out = strip_html_for_preview(html);
        assert!(!out.contains("tracker"), "script payload leaked: {out}");
        assert!(!out.contains("window.x"), "script payload leaked: {out}");
        assert!(out.contains("Headline"));
        assert!(out.contains("Body"));
    }

    #[test]
    fn strip_html_drops_style_payload() {
        let html = r#"<html><head><style>.foo { color: hotpink; } /* don't show me */</style></head><body>Visible.</body></html>"#;
        let out = strip_html_for_preview(html);
        assert!(!out.contains("hotpink"), "style payload leaked: {out}");
        assert!(!out.contains("don't show me"), "style payload leaked: {out}");
        assert_eq!(out, "Visible.");
    }

    #[test]
    fn strip_html_drops_html_comments() {
        let html = "<p>Visible.</p><!-- analytics: hidden --><p>Also visible.</p>";
        let out = strip_html_for_preview(html);
        assert!(!out.contains("analytics"), "comment leaked: {out}");
        assert_eq!(out, "Visible. Also visible.");
    }

    #[test]
    fn strip_html_drops_tags_keeps_text() {
        let html = r#"<!doctype html><html lang="en-US" theme="auto" data-color-theme-enabled="true"><body><h1 class="hero">TSLA closes at $312</h1><p>Tesla Inc. reported earnings...</p></body></html>"#;
        let out = strip_html_for_preview(html);
        assert!(!out.contains('<'), "unstripped angle bracket: {out}");
        assert!(!out.contains('>'), "unstripped angle bracket: {out}");
        assert!(out.contains("TSLA closes at $312"));
        assert!(out.contains("Tesla Inc."));
    }

    #[test]
    fn strip_html_decodes_named_entities() {
        let html = "<p>Foo &amp; Bar &mdash; &ldquo;hello&rdquo;&nbsp;world.</p>";
        let out = strip_html_for_preview(html);
        // Single-space between "hello"-quote and "world" (collapsed
        // from the &nbsp;); the em-dash and curly quotes decode.
        assert_eq!(out, "Foo & Bar — “hello” world.");
    }

    #[test]
    fn strip_html_decodes_numeric_entities() {
        let html = "<p>It&#39;s also &#x2014; here.</p>";
        let out = strip_html_for_preview(html);
        assert_eq!(out, "It's also — here.");
    }

    #[test]
    fn strip_html_collapses_whitespace() {
        let html = "<p>One</p>\n\n\n  <p>two</p>\n<p>three</p>";
        let out = strip_html_for_preview(html);
        // Tags are replaced with single spaces and surrounding
        // whitespace is collapsed, so the run between "One" and "two"
        // becomes a single space.
        assert_eq!(out, "One two three");
    }

    #[test]
    fn strip_html_passes_through_unknown_entity() {
        // Unknown entities are left alone so the operator can notice
        // them if a future page uses something the whitelist doesn't
        // cover.
        let html = "<p>&euro; sign here, &middot; here.</p>";
        let out = strip_html_for_preview(html);
        assert!(out.contains("&euro;"));
        assert!(out.contains("&middot;"));
    }

    #[test]
    fn strip_html_preserves_ampersand_when_no_entity() {
        // A bare `&` followed by non-entity text must not eat the
        // surrounding bytes.
        let html = "<p>A & B</p>";
        let out = strip_html_for_preview(html);
        assert_eq!(out, "A & B");
    }

    #[test]
    fn body_preview_strips_a_realistic_homepage() {
        // The reported TESLA bug: raw HTML preview starts with
        // `<!doctype html> <html lang="en-US" theme="auto" data-color-...`.
        // After Session 70 the preview reads as visible article text.
        let html = r#"<!doctype html>
<html lang="en-US" theme="auto" data-color-theme-enabled="true">
<head><title>Tesla Q2 earnings</title>
<script src="/analytics.js"></script>
<style>.nav { display: none; }</style>
</head>
<body>
<header><nav>Home | About | Contact</nav></header>
<main>
<h1>Tesla beats Q2 expectations</h1>
<p>The company reported &dollar;25.4B in revenue.</p>
</main>
</body></html>"#;
        let out = body_preview("text/html", html.as_bytes());
        assert!(!out.contains('<'), "raw markup leaked: {out}");
        assert!(!out.contains("analytics.js"), "script src leaked: {out}");
        assert!(!out.contains("display: none"), "style leaked: {out}");
        assert!(out.contains("Tesla Q2 earnings"));
        assert!(out.contains("Tesla beats Q2 expectations"));
        assert!(out.contains("Home | About | Contact"));
    }

    #[test]
    fn body_preview_caps_after_strip_at_32_kib() {
        // A pathological HTML doc that's mostly tags. After stripping
        // we should land near the cap, not below it — the strip
        // input window is 4× the cap, so 50 KiB of text post-strip is
        // enough to fill 32 KiB.
        let mut html = String::from("<html><body>");
        let chunk = "<p>The quick brown fox jumps over the lazy dog. </p>";
        while html.len() < 4 * BODY_PREVIEW_CAP_BYTES {
            html.push_str(chunk);
        }
        html.push_str("</body></html>");
        let out = body_preview("text/html", html.as_bytes());
        assert!(out.len() <= BODY_PREVIEW_CAP_BYTES);
        // Should be close to the cap (within 1 KiB), not far below it.
        assert!(out.len() >= BODY_PREVIEW_CAP_BYTES - 1024, "preview too short: {}", out.len());
        assert!(!out.contains('<'));
    }

    #[test]
    fn body_preview_json_unchanged_by_strip() {
        // JSON Documents (data_feed kind) must not be touched by the
        // HTML strip — `RecordsDashboard.detectTimeSeriesShape` parses
        // the raw JSON to find time series.
        let json = br#"{"chart":{"result":[{"meta":{"symbol":"TSLA"},"timestamp":[1,2,3]}]}}"#;
        let out = body_preview("application/json", json);
        assert_eq!(
            out,
            r#"{"chart":{"result":[{"meta":{"symbol":"TSLA"},"timestamp":[1,2,3]}]}}"#
        );
    }

    #[test]
    fn body_preview_csv_unchanged_by_strip() {
        let csv = b"date,close\n2026-05-14,312.50\n2026-05-13,308.10\n";
        let out = body_preview("text/csv", csv);
        assert_eq!(out, "date,close\n2026-05-14,312.50\n2026-05-13,308.10\n");
    }

    #[test]
    fn truncate_at_char_boundary_drops_partial_codepoint() {
        // String with a 4-byte codepoint at index 30. Cap at 32 bytes
        // would split it; we should land at 30 instead.
        let s = format!("{}🦀", "a".repeat(30));
        assert_eq!(s.len(), 34); // 30 ASCII + 4-byte crab
        let out = truncate_at_char_boundary(&s, 32);
        // The crab starts at index 30. char_indices yields 0..29 then
        // 30. With max_bytes=32, idx 30 > 32 is false (30 < 32), so
        // last advances to 30. Next index would be 34 (after the
        // 4-byte char), which is > 32, so loop breaks. last=30.
        assert_eq!(out.len(), 30);
        assert!(out.chars().all(|c| c == 'a'));
    }
}
