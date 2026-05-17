//! Apply-time index-page detector — Session 93, ADR 0017 follow-on.
//!
//! ## What this module does
//!
//! Given fetched bytes + MIME + URL, return an [`IndexPageSignal`]
//! that tells the caller whether the page is plausibly an *article*
//! (high prose-to-link ratio, single main heading), an *index* (mostly
//! a link list with little prose — `/topic/`, `/category/`, `/tag/`,
//! `/section/`, `/archive/` shapes; or a body whose `<a>` density
//! dominates the text), or *unknown* (not HTML, MIME unparseable,
//! bytes too short to decide).
//!
//! The caller (the apply path in `fetch_executor::run_css_recipe`)
//! uses this to short-circuit selector evaluation against an index
//! page — instead of asking the LLM to write a relation/event/obs
//! recipe whose extraction will find topic-listing chrome rather
//! than article prose, the apply path stamps the outcome as
//! [`FetchOutcomeClass::IndexPageDetected`] and the proposer's
//! retry loop reads that class on the next attempt.
//!
//! ## Closed-vocabulary discipline
//!
//! This module **names no hosts**. Every signal is structural:
//!
//!   - HTML `<a>` link density (anchors × avg anchor text length,
//!     divided by total body text after HTML strip)
//!   - URL path-segment tokens (`/topic/`, `/category/`, `/tag/`,
//!     `/section/`, `/archive/`, `/index`, `/all/`) — these are
//!     *generic web patterns*, not source-specific routes
//!   - Body prose floor (text length after HTML strip with anchor
//!     text excluded) — short body ⇒ no prose to extract ⇒ Index
//!     regardless of link density
//!
//! Hosts appear in [`crate::fetch_classes::HOST_CLASS_OVERRIDES`]
//! and nowhere else; this module continues that discipline.
//!
//! ## What this module does NOT do
//!
//! - **It does not classify non-HTML payloads.** JSON / CSV / PDF
//!   bytes always score `Unknown`. The detector is a defence against
//!   the article-vs-index-listing failure shape that lives on HTML
//!   pages specifically.
//! - **It does not fetch.** Bytes are passed in; the caller owns the
//!   network boundary.
//! - **It does not retry, back off, or re-author.** Classification is
//!   read-only. The fetch executor stamps the apply outcome with
//!   `IndexPageDetected` when this module returns `Index`; the
//!   recipe-author prompt (v1.24's "Diagnosis-driven re-authoring"
//!   section) is what turns the next apply-failure attempt into a
//!   follow-the-link recipe.
//! - **It does not promise zero false positives.** A long-form
//!   article with an end-of-page "Related stories" link block can
//!   tip into `Index` under the link-density signal alone. The path-
//!   token signal disambiguates when the URL itself carries a
//!   generic listing token; absent that signal, the threshold is
//!   tuned conservatively so prose-heavy bodies score `Article`
//!   even with a moderate link block.
//!
//! ## Thresholds — first cut, tunable
//!
//! Two thresholds determine the score. Both are tuned conservatively
//! so a "borderline" page scores `Unknown` (which the caller treats
//! as "proceed with apply") rather than `Index` (which short-circuits
//! apply). Bumping either is a deliberate trade-off:
//!
//!   - `LINK_DENSITY_INDEX_THRESHOLD = 0.5` — if anchor-text bytes
//!     are ≥50% of total body bytes after HTML strip, the page is
//!     mostly links. Lowering this catches more index pages at the
//!     cost of false positives on link-heavy article pages.
//!   - `BODY_PROSE_FLOOR_CHARS = 400` — if total body bytes after
//!     HTML strip + anchor-text exclusion fall below this floor, the
//!     page has no prose to extract regardless of link density.
//!     Lowering this lets thinner pages through; raising it catches
//!     more sparse-listing pages but rejects short-form articles.
//!
//! Both numbers come from Sn-91's miningweekly aluminium-topic page
//! observation; they have not been swept against a corpus yet. A
//! follow-on session that exposes the detector's output on the
//! dashboard (per-fetch `IndexPageSignal` chip) would let the
//! operator validate the thresholds against live traffic.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The signal vocabulary
// ---------------------------------------------------------------------------

/// Closed enum describing the detector's read of fetched bytes.
///
/// `serde_json` representation is `snake_case` so the signal can
/// appear in log fields and on the future per-fetch dashboard chip
/// without further transformation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPageSignal {
    /// Body looks article-shaped: prose dominates, link density is
    /// below the threshold, URL path has no generic listing token.
    Article,
    /// Body looks index-shaped: either the URL path carries a generic
    /// listing token (`/topic/`, `/category/`, …) or the link-density
    /// signal trips, or the body has too little prose to extract.
    Index,
    /// Bytes aren't HTML, MIME is opaque, or the body is too short to
    /// reach a confident classification.
    Unknown,
}

impl IndexPageSignal {
    /// Short snake_case label for log fields. Mirrors the serde
    /// representation; provided as a const-friendly accessor so call
    /// sites that don't pull serde can still spell the signal.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::Index => "index",
            Self::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Link-density threshold above which a body is read as Index.
///
/// Anchor-text bytes / total-body-text bytes ≥ this number ⇒ Index.
/// 0.5 = "half the visible text is link labels".
const LINK_DENSITY_INDEX_THRESHOLD: f32 = 0.5;

/// Minimum non-anchor prose body length (in characters) below which
/// the page is read as Index regardless of link density. A page with
/// no extractable prose can't carry the article that a relation /
/// event / observation recipe wants.
const BODY_PROSE_FLOOR_CHARS: usize = 400;

/// Generic URL path tokens that indicate a listing / index page,
/// regardless of host. The match is case-insensitive and substring-
/// anchored on segment boundaries inside the path (so `/topic/x`
/// matches but `/microtopic/x` does not).
///
/// Closed vocabulary: these are *web patterns* (`/topic/` exists on
/// many news sites) not source routes (which would be host-specific
/// strings, forbidden by ADR 0007).
const INDEX_PATH_TOKENS: &[&str] = &[
    "/topic/",
    "/topics/",
    "/category/",
    "/categories/",
    "/tag/",
    "/tags/",
    "/section/",
    "/sections/",
    "/archive/",
    "/archives/",
    "/index",
    "/all/",
    "/all-",
    "/listing/",
    "/browse/",
];

/// Classify fetched bytes against the apply-time index-page signal.
///
/// `bytes` is the body the executor fetched, `mime` is the
/// response's `Content-Type` (or empty string when the server didn't
/// send one), and `source_url` is the URL that was fetched.
///
/// Returns `Unknown` for non-HTML MIME types — JSON / CSV / PDF / RSS
/// payloads have their own apply-time failure modes and don't use
/// this detector.
pub fn classify_fetched_bytes(
    bytes: &[u8],
    mime: &str,
    source_url: &str,
) -> IndexPageSignal {
    // 1. MIME gate — only HTML / XHTML payloads run the detector.
    //    Empty MIME ⇒ try anyway (the executor doesn't always have
    //    a Content-Type header); other MIMEs short-circuit.
    let mime_lc = mime.to_ascii_lowercase();
    let mime_ok = mime_lc.is_empty()
        || mime_lc.starts_with("text/html")
        || mime_lc.starts_with("application/xhtml");
    if !mime_ok {
        return IndexPageSignal::Unknown;
    }

    // 2. Parse the bytes as UTF-8. If they aren't UTF-8 we don't try
    //    to recover encoding here (apply path's HTML scrape would
    //    also fail); return Unknown so the caller proceeds with the
    //    existing failure path rather than mis-classifying.
    let html_str = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return IndexPageSignal::Unknown,
    };

    // 3. Source URL path-token signal. If the URL itself carries a
    //    generic listing token, that's a strong signal regardless of
    //    body shape. Strip the scheme + host so a `/topic/` segment
    //    in the path matches but a `/topic/` substring in the host
    //    (rare but possible) doesn't.
    let url_lc = source_url.to_ascii_lowercase();
    let url_path: &str = match url_lc.find("://") {
        Some(i) => match url_lc[i + 3..].find('/') {
            Some(j) => &url_lc[i + 3 + j..],
            None => "/",
        },
        None => &url_lc,
    };
    let url_token_signal = INDEX_PATH_TOKENS.iter().any(|tok| url_path.contains(tok));

    // 4. Compute body-text + anchor-text byte counts. This is a
    //    *structural* scan, not a full HTML parse — we count chars
    //    inside `<a ...>...</a>` blocks separately from the
    //    rest of the visible text, then HTML-strip both buckets.
    let (anchor_chars, body_chars_excluding_anchors) = scan_anchor_density(html_str);
    let total_body_chars = anchor_chars + body_chars_excluding_anchors;

    // 5. Body-prose floor — if the body excluding anchor text is too
    //    short, the page has no article-shaped prose to extract.
    if body_chars_excluding_anchors < BODY_PROSE_FLOOR_CHARS {
        // Sub-case: very short body AND no URL token AND no
        // anchors ⇒ probably an interstitial / error page / empty
        // shell, not an index per se. Return Unknown so the caller
        // doesn't short-circuit a recoverable apply.
        if !url_token_signal && anchor_chars == 0 {
            return IndexPageSignal::Unknown;
        }
        return IndexPageSignal::Index;
    }

    // 6. Link-density signal.
    let density = if total_body_chars == 0 {
        0.0
    } else {
        anchor_chars as f32 / total_body_chars as f32
    };
    let density_signal = density >= LINK_DENSITY_INDEX_THRESHOLD;

    if url_token_signal || density_signal {
        IndexPageSignal::Index
    } else {
        IndexPageSignal::Article
    }
}

/// Scan HTML for `<a>` blocks and count visible-text chars inside
/// vs outside them. Returns `(anchor_text_chars, other_text_chars)`.
///
/// Closed-vocab structural scan: no DOM parser, no scraper crate
/// dependency — the executor already pulls in `scraper` for apply,
/// but here we want a faster O(n) walk that doesn't allocate. The
/// scan is purposely loose:
///
///   - `<a` opens an anchor scope (matched case-insensitively).
///   - The first `>` after `<a` closes the open tag.
///   - `</a` ends the anchor scope (matched case-insensitively).
///   - All other tags (`<script>`, `<style>`, `<div>`, …) are
///     skipped via a generic angle-bracket strip.
///
/// We deliberately don't decode entities — the byte counts are an
/// approximation, not exact rendered text. The thresholds are tuned
/// against the approximation, so adding a decoding pass would shift
/// the calibration.
fn scan_anchor_density(html: &str) -> (usize, usize) {
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut in_anchor = false;
    let mut in_tag = false;
    let mut in_script_or_style = false;
    let mut script_style_close: Option<&[u8]> = None;
    let mut anchor_chars: usize = 0;
    let mut other_chars: usize = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Inside <script>/<style>: skip everything until the matching
        // close tag, then resume normal scanning.
        if in_script_or_style {
            if let Some(close) = script_style_close {
                if i + close.len() <= bytes.len()
                    && bytes[i..i + close.len()].eq_ignore_ascii_case(close)
                {
                    in_script_or_style = false;
                    script_style_close = None;
                    i += close.len();
                    continue;
                }
            }
            i += 1;
            continue;
        }

        if in_tag {
            if b == b'>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }

        if b == b'<' {
            // Detect tag kind.
            let rest = &bytes[i + 1..];
            if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case(b"script") {
                in_script_or_style = true;
                script_style_close = Some(b"</script>");
                i += 1;
                continue;
            }
            if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(b"style") {
                in_script_or_style = true;
                script_style_close = Some(b"</style>");
                i += 1;
                continue;
            }
            if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"!--") {
                // HTML comment — skip to -->.
                if let Some(end) = find_subseq(&bytes[i + 4..], b"-->") {
                    i += 4 + end + 3;
                    continue;
                } else {
                    // Unterminated comment — consume rest.
                    break;
                }
            }
            // <a ...>  opens an anchor scope.
            if !in_anchor
                && rest.len() >= 1
                && (rest[0].eq_ignore_ascii_case(&b'a'))
                && (rest.len() == 1
                    || rest[1] == b' '
                    || rest[1] == b'>'
                    || rest[1] == b'\t'
                    || rest[1] == b'\n'
                    || rest[1] == b'/')
            {
                in_anchor = true;
                in_tag = true;
                i += 1;
                continue;
            }
            // </a> closes.
            if in_anchor
                && rest.len() >= 2
                && rest[0] == b'/'
                && rest[1].eq_ignore_ascii_case(&b'a')
            {
                in_anchor = false;
                in_tag = true;
                i += 1;
                continue;
            }
            // Any other tag — generic skip.
            in_tag = true;
            i += 1;
            continue;
        }

        // Outside tags + scripts: count one visible char.
        // Skip whitespace runs to one slot so a multi-newline gap
        // doesn't inflate either bucket disproportionately.
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
            // Collapse runs of whitespace to one char by advancing
            // past consecutive whitespace bytes.
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if c == b' ' || c == b'\n' || c == b'\r' || c == b'\t' {
                    j += 1;
                } else {
                    break;
                }
            }
            // One char for the collapsed whitespace run.
            if in_anchor {
                anchor_chars += 1;
            } else {
                other_chars += 1;
            }
            i = j;
            continue;
        }

        if in_anchor {
            anchor_chars += 1;
        } else {
            other_chars += 1;
        }
        i += 1;
    }

    (anchor_chars, other_chars)
}

/// Tiny `Vec::windows`-free subsequence search for the comment-skip
/// path. Returns the byte offset where `needle` starts in `haystack`,
/// or `None` if not present.
fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let max = haystack.len() - needle.len() + 1;
    for i in 0..max {
        if haystack[i..i + needle.len()] == *needle {
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- MIME gate -----------------------------------------------------

    #[test]
    fn non_html_mime_returns_unknown() {
        let bytes = b"{\"price\": 123}";
        assert_eq!(
            classify_fetched_bytes(bytes, "application/json", "https://api.example/v1/x"),
            IndexPageSignal::Unknown
        );
    }

    #[test]
    fn pdf_mime_returns_unknown() {
        let bytes = b"%PDF-1.7 fake";
        assert_eq!(
            classify_fetched_bytes(bytes, "application/pdf", "https://files.example/x.pdf"),
            IndexPageSignal::Unknown
        );
    }

    #[test]
    fn empty_mime_falls_through_to_html_detection() {
        // Empty MIME ⇒ run detector anyway. Body has prose, no URL
        // token, low link density ⇒ Article.
        let body = "<html><body><p>".to_string()
            + &"This is a long-form prose paragraph that runs well above the prose floor. "
                .repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(body.as_bytes(), "", "https://news.example/p/1234"),
            IndexPageSignal::Article
        );
    }

    // --- URL path-token signal -----------------------------------------

    #[test]
    fn url_with_topic_token_scores_index_even_on_prose_body() {
        // The /topic/ token in the URL is a strong signal. Body has
        // prose but the URL says listing.
        let body = "<html><body><p>".to_string()
            + &"Long article-shaped prose body well above the floor. ".repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html; charset=utf-8",
                "https://www.example.com/topic/aluminium",
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn url_with_category_token_scores_index() {
        let body = "<html><body><p>".to_string()
            + &"Long article-shaped prose body well above the floor. ".repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://www.example.com/category/business",
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn url_with_tag_token_scores_index() {
        let body = "<html><body><p>".to_string()
            + &"Long article-shaped prose body well above the floor. ".repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/tag/elections",
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn url_token_match_is_case_insensitive() {
        let body = "<html><body><p>".to_string()
            + &"Long article-shaped prose body well above the floor. ".repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://www.example.com/TOPIC/Aluminium",
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn url_token_does_not_partial_match_inside_segment() {
        // `/microtopic/x` does NOT match `/topic/`; segment boundaries
        // (leading `/`) protect against off-target matches.
        let body = "<html><body><p>".to_string()
            + &"Long article-shaped prose body well above the floor. ".repeat(20)
            + "</p></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/microtopic/aluminium",
            ),
            IndexPageSignal::Article
        );
    }

    // --- Link-density signal -------------------------------------------

    #[test]
    fn high_link_density_body_scores_index() {
        // 30 anchors of ~40 chars each = ~1200 chars of anchor text.
        // Surrounding prose totals ~50 chars. Density ≫ threshold.
        let mut body = String::from("<html><body><nav>");
        for i in 0..30 {
            body.push_str(&format!(
                "<a href=\"/article-{i}\">Headline number {i} about a topic that interests readers</a> "
            ));
        }
        body.push_str("more text</nav></body></html>");
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/some/landing"
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn low_link_density_prose_body_scores_article() {
        // Long prose body with a small navbar of 3 short links.
        let body = "<html><body><nav><a href=/a>Home</a> <a href=/b>About</a> <a href=/c>Contact</a></nav><article><p>".to_string()
            + &"This is a long-form prose paragraph that runs well above the prose floor. ".repeat(30)
            + "</p></article></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/2026/05/17/some-article"
            ),
            IndexPageSignal::Article
        );
    }

    // --- Body-prose floor ----------------------------------------------

    #[test]
    fn very_short_body_with_links_scores_index() {
        // 5 anchors, no surrounding prose ⇒ body excluding anchors
        // is below the floor ⇒ Index.
        let body = "<html><body>\
            <a href=/x1>One link</a>\
            <a href=/x2>Two link</a>\
            <a href=/x3>Three link</a>\
            <a href=/x4>Four link</a>\
            <a href=/x5>Five link</a>\
            </body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/landing"
            ),
            IndexPageSignal::Index
        );
    }

    #[test]
    fn very_short_empty_body_scores_unknown() {
        // No prose, no anchors, no URL token ⇒ Unknown. The caller
        // shouldn't short-circuit apply on something that might be
        // an interstitial / error page; the existing failure path
        // handles those.
        let body = "<html><body></body></html>";
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/landing"
            ),
            IndexPageSignal::Unknown
        );
    }

    // --- Script / style skipping ---------------------------------------

    #[test]
    fn script_blocks_do_not_inflate_body_chars() {
        // A page with a tiny prose body and a large JSON inside
        // <script> should still classify on the prose, not the
        // script payload. Without script-skipping, the JSON would
        // pad `other_chars` and tip the density math.
        let mut body = String::from("<html><body><script>");
        body.push_str(&"{\"x\": ".repeat(500));
        body.push_str("</script>\n");
        // 5 anchors with no prose around them.
        for i in 0..5 {
            body.push_str(&format!(
                "<a href=\"/p-{i}\">Headline number {i}</a> "
            ));
        }
        body.push_str("</body></html>");
        assert_eq!(
            classify_fetched_bytes(
                body.as_bytes(),
                "text/html",
                "https://news.example/landing"
            ),
            IndexPageSignal::Index
        );
    }

    // --- label() / serde round-trip ------------------------------------

    #[test]
    fn label_strings_are_stable_snake_case() {
        assert_eq!(IndexPageSignal::Article.label(), "article");
        assert_eq!(IndexPageSignal::Index.label(), "index");
        assert_eq!(IndexPageSignal::Unknown.label(), "unknown");
    }

    #[test]
    fn label_matches_serde_representation() {
        let cases = [
            (IndexPageSignal::Article, "\"article\""),
            (IndexPageSignal::Index, "\"index\""),
            (IndexPageSignal::Unknown, "\"unknown\""),
        ];
        for (sig, expected) in cases {
            let json = serde_json::to_string(&sig).unwrap();
            assert_eq!(json, expected);
            let unquoted = &json[1..json.len() - 1];
            assert_eq!(unquoted, sig.label());
        }
    }

    // --- non-UTF-8 bytes -----------------------------------------------

    #[test]
    fn non_utf8_bytes_score_unknown() {
        // 0xff is not a valid UTF-8 starter. Detector should return
        // Unknown rather than panic or mis-classify.
        let bytes: &[u8] = &[0xff, 0xfe, b'<', b'h', b'1', b'>', b'x', 0xff];
        assert_eq!(
            classify_fetched_bytes(bytes, "text/html", "https://example.com/x"),
            IndexPageSignal::Unknown
        );
    }
}
