//! Runtime URL normalisation for paginated API endpoints.
//!
//! Session 68 — addresses Session 67's 11 cap-exceeded apply
//! failures on `www.fema.gov`. The LLM authors a JSON URL like
//! `https://www.fema.gov/api/open/v2/DisasterDeclarationsSummaries`,
//! the OpenFEMA v2 endpoint defaults to a 1000-row page, the runtime
//! caps at [`crate::recipe_apply::MAX_RECORDS_PER_RECIPE`] and
//! aborts the recipe before producing any records (see
//! `docs/failure_cases/class_b/2026-05-13_api_fema_gov_jsonpath_iterator_cap_exceeded.md`).
//!
//! This module sits between the recipe's `source_url` and the actual
//! HTTP fetch in [`crate::fetch_executor::fetch_recipe_bytes`]: when
//! the URL is OData-shaped (a published convention used by Microsoft
//! services, OpenFEMA, SAP, GitLab, etc.) and either has no `$top`
//! parameter or has one above our cap, we inject/lower `$top` so the
//! response stays within the cap.
//!
//! ## Why URL-shape, not host
//!
//! `feedback_no_easy_wins` + `project_sr_no_source_routing`: the
//! codebase forbids host-string routing. OData detection here is
//! purely shape-based — presence of any `$select | $filter | $orderby
//! | $expand | $format | $top | $skip | $count` query parameter (the
//! formal OData v4 system-query keys). Any host that exposes that
//! shape benefits; no host strings appear here.
//!
//! ## Why not just teach the LLM
//!
//! The LLM had visibility to FEMA's `metadata.count == 1000` at
//! prefetch (Session 49+) and didn't reach for `$top`. This is a
//! prompt-quality gap that will eventually close, but the runtime
//! defence applies to 100% of recipes immediately and doesn't
//! depend on a future prompt revision landing.

use std::borrow::Cow;
use url::Url;

/// OData v4 system-query keys. Presence of any one of these (or
/// `$top`/`$skip`/`$count`) marks the URL as OData-shaped. The set
/// is small and stable; OData has not added new system-query keys
/// since the v4 spec (2014).
const ODATA_KEYS: &[&str] = &[
    "$select", "$filter", "$orderby", "$expand", "$format",
    "$top", "$skip", "$count",
];

/// Some hosts (notably OpenFEMA) expose OData-shaped responses
/// without requiring any OData query parameter on the request — the
/// default response carries `metadata.top`, `metadata.skip`,
/// `metadata.count`, and the response key matches the path's last
/// segment. We detect those by URL path shape: an `/api/open/v\d+/`
/// segment is the OpenFEMA convention; over time additional
/// shape-based heuristics can land here.
///
/// Path-shape detection is still closed-vocabulary-safe: it acts on
/// URL structure, not on host strings. Any host that adopts the
/// `/api/open/vN/` URL convention (the OData-friendly OpenFEMA
/// shape) will benefit.
fn path_implies_odata(url: &Url) -> bool {
    let path = url.path();
    // /api/open/v1/, /api/open/v2/, …
    if let Some(rest) = path.strip_prefix("/api/open/v") {
        if let Some(idx) = rest.find('/') {
            return rest[..idx].chars().all(|c| c.is_ascii_digit())
                && !rest[..idx].is_empty();
        }
    }
    false
}

/// Returns `true` if the URL's query string contains any OData
/// system-query parameter. Case-sensitive — OData spec is
/// case-sensitive on these keys.
fn query_implies_odata(url: &Url) -> bool {
    url.query_pairs()
        .any(|(k, _)| ODATA_KEYS.contains(&k.as_ref()))
}

/// Result of inspecting a URL for pagination normalisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationCap {
    /// URL is not OData-shaped; passes through unchanged.
    NotApplicable,
    /// URL is OData-shaped and already has a `$top` within the cap.
    /// No rewrite needed.
    AlreadyCapped,
    /// URL was rewritten — a `$top` was injected (no prior `$top`)
    /// or lowered (prior `$top` exceeded the cap).
    Rewritten {
        /// Original `$top` value if one was present and changed.
        prior_top: Option<u64>,
        /// New `$top` value the rewrite installed.
        new_top: u64,
    },
}

/// Apply OData `$top` capping to a URL. Returns the URL unchanged
/// (zero-allocation `Cow::Borrowed`) when the URL isn't OData-shaped
/// or already has an in-bounds `$top`; returns a rewritten owned
/// string when a `$top` was injected or lowered.
///
/// `cap` is the maximum permitted `$top` value — callers should pass
/// the runtime's record cap so the response cannot exceed it.
///
/// Invalid URLs pass through unchanged (the downstream fetch will
/// surface the parse error with its existing message; this function
/// doesn't claim ownership of URL validation).
///
/// ## Why we splice strings instead of round-tripping through `Url::query_pairs_mut`
///
/// The `url` crate's `query_pairs_mut().append_pair(...)` and
/// `extend_pairs(...)` apply form-urlencoded encoding to both keys
/// and values. That re-encodes the literal `$` of `$top`, `$select`,
/// `$filter`, etc. as `%24` — technically valid (servers decode
/// either form) but cosmetically wrong (every other OData client in
/// the world emits the literal `$`) and breaks tests/log inspection
/// that look for the literal substring `$top=`. The first ship
/// (Session 68 cap_pagination v1) used the round-trip and tripped
/// 7 of its own tests. We splice on the raw query string instead so
/// the operator-visible URL matches the OData convention exactly.
pub fn cap_pagination<'a>(url: &'a str, cap: u64) -> (Cow<'a, str>, PaginationCap) {
    let parsed = match Url::parse(url) {
        Ok(p) => p,
        Err(_) => return (Cow::Borrowed(url), PaginationCap::NotApplicable),
    };

    let is_odata = query_implies_odata(&parsed) || path_implies_odata(&parsed);
    if !is_odata {
        return (Cow::Borrowed(url), PaginationCap::NotApplicable);
    }

    let query = parsed.query().unwrap_or("");
    let top_pos = find_top_position(query);

    let (new_query, kind) = match top_pos {
        Some((_, _, Some(t))) if t <= cap => {
            return (Cow::Borrowed(url), PaginationCap::AlreadyCapped);
        }
        Some((start, end, parsed_top)) => {
            // Existing `$top=...` — replace its value in place. Bytes
            // before `start` and after `end` are preserved verbatim,
            // which keeps any other OData params (`$select`, `$filter`)
            // exactly as the caller authored them.
            let mut new_q = String::with_capacity(query.len() + 8);
            new_q.push_str(&query[..start]);
            new_q.push_str("$top=");
            new_q.push_str(&cap.to_string());
            new_q.push_str(&query[end..]);
            (
                new_q,
                PaginationCap::Rewritten {
                    prior_top: parsed_top,
                    new_top: cap,
                },
            )
        }
        None => {
            // No `$top=` at all — append. Empty-query case ("?$top=N")
            // is distinguished so we don't end up with "?&$top=N".
            let new_q = if query.is_empty() {
                format!("$top={cap}")
            } else {
                format!("{query}&$top={cap}")
            };
            (
                new_q,
                PaginationCap::Rewritten {
                    prior_top: None,
                    new_top: cap,
                },
            )
        }
    };

    (
        Cow::Owned(rewrite_query_in_url(url, &parsed, &new_query)),
        kind,
    )
}

/// Find the FIRST `$top=...` pair in a raw query string. Returns
/// `(start_byte, end_byte, parsed_value)` where the byte positions
/// span the entire pair (`$top=...`) within the input slice and
/// `parsed_value` is `Some(u64)` when the value parses as a
/// non-negative integer, `None` for unparseable garbage.
///
/// Operates on the raw (percent-encoded-as-found) query because
/// the splice in `cap_pagination` needs byte positions in the same
/// string. The downside is we won't detect a `%24top=` (literal-
/// percent-encoded `$top`) — but no real OData client emits that
/// form, and the LLM author prompt teaches the literal-`$` shape.
fn find_top_position(query: &str) -> Option<(usize, usize, Option<u64>)> {
    let mut idx = 0usize;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("$top=") {
            let start = idx;
            let end = idx + pair.len();
            return Some((start, end, value.parse::<u64>().ok()));
        }
        idx += pair.len() + 1; // +1 for the '&' delimiter we split on
    }
    None
}

/// Replace the query portion of `original` with `new_query`, keeping
/// the scheme, authority, path, and fragment byte-for-byte. The
/// parsed URL is passed in so `query()`/`fragment()` answers
/// authoritative "is there one" questions; we then locate the
/// markers in the original string by find/rfind so percent-encoded
/// sequences elsewhere stay untouched.
///
/// Always emits a `?{new_query}` segment — callers only invoke this
/// when they have a non-empty replacement to install.
fn rewrite_query_in_url(original: &str, parsed: &Url, new_query: &str) -> String {
    // Position of '#' in the original string, only when there's a
    // fragment. (`Url::parse` accepts a literal '#' inside a
    // fragment's percent-decoded form via `%23`, but at the wire
    // level the first '#' marks the fragment.)
    let h_pos: Option<usize> = parsed.fragment().and_then(|_| original.find('#'));

    // Position of '?' that opened the query, only when there's a
    // query. Search bounded by the fragment start so a stray '?'
    // inside a fragment can't be mistaken for the query marker.
    let q_pos: Option<usize> = parsed.query().and_then(|_| {
        let search_end = h_pos.unwrap_or(original.len());
        original[..search_end].find('?')
    });

    // Prefix = everything before the existing query (or fragment, if
    // no query existed). Suffix = the fragment, including its '#'.
    let before: &str = match (q_pos, h_pos) {
        (Some(q), _) => &original[..q],
        (None, Some(h)) => &original[..h],
        (None, None) => original,
    };
    let fragment: &str = h_pos.map(|p| &original[p..]).unwrap_or("");

    format!("{before}?{new_query}{fragment}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: u64 = 500;

    #[test]
    fn passthrough_for_non_odata_url() {
        let url = "https://example.com/data.json";
        let (out, kind) = cap_pagination(url, CAP);
        assert_eq!(out, url);
        assert_eq!(kind, PaginationCap::NotApplicable);
        assert!(matches!(out, Cow::Borrowed(_)), "must not allocate");
    }

    #[test]
    fn passthrough_for_invalid_url() {
        let url = "not-a-url";
        let (out, kind) = cap_pagination(url, CAP);
        assert_eq!(out, url);
        assert_eq!(kind, PaginationCap::NotApplicable);
    }

    #[test]
    fn detects_odata_by_select_param() {
        let url = "https://service.example.com/api?$select=name";
        let (out, kind) = cap_pagination(url, CAP);
        assert!(out.contains("$top=500"), "got {out}");
        assert!(out.contains("$select=name"), "preserves $select");
        assert!(matches!(
            kind,
            PaginationCap::Rewritten {
                prior_top: None,
                new_top: 500
            }
        ));
    }

    #[test]
    fn detects_odata_by_filter_param() {
        let url = "https://service.example.com/api?$filter=year%20eq%202025";
        let (out, _) = cap_pagination(url, CAP);
        assert!(out.contains("$top=500"));
    }

    #[test]
    fn detects_odata_by_orderby_param() {
        let url = "https://service.example.com/api?$orderby=date";
        let (out, _) = cap_pagination(url, CAP);
        assert!(out.contains("$top=500"));
    }

    #[test]
    fn detects_openfema_by_path_shape() {
        // The smoking-gun case from Session 67 (FEMA hunt).
        let url = "https://www.fema.gov/api/open/v2/DisasterDeclarationsSummaries";
        let (out, kind) = cap_pagination(url, CAP);
        assert!(
            out.contains("$top=500"),
            "FEMA path shape should trigger rewrite, got {out}"
        );
        assert!(matches!(
            kind,
            PaginationCap::Rewritten {
                prior_top: None,
                new_top: 500
            }
        ));
    }

    #[test]
    fn passthrough_for_path_with_api_open_but_no_version() {
        let url = "https://example.com/api/open/data";
        let (out, kind) = cap_pagination(url, CAP);
        assert_eq!(out, url);
        assert_eq!(kind, PaginationCap::NotApplicable);
    }

    #[test]
    fn keeps_existing_top_when_within_cap() {
        let url = "https://service.example.com/api?$select=x&$top=200";
        let (out, kind) = cap_pagination(url, CAP);
        assert_eq!(out, url, "no rewrite needed");
        assert_eq!(kind, PaginationCap::AlreadyCapped);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn lowers_top_above_cap() {
        let url = "https://service.example.com/api?$select=x&$top=2000";
        let (out, kind) = cap_pagination(url, CAP);
        assert!(out.contains("$top=500"), "got {out}");
        assert!(!out.contains("$top=2000"));
        assert!(out.contains("$select=x"), "preserves siblings");
        assert!(matches!(
            kind,
            PaginationCap::Rewritten {
                prior_top: Some(2000),
                new_top: 500
            }
        ));
    }

    #[test]
    fn fema_with_existing_filter_keeps_filter_and_adds_top() {
        let url = "https://www.fema.gov/api/open/v2/DisasterDeclarationsSummaries?$filter=fyDeclared%20eq%202025";
        let (out, kind) = cap_pagination(url, CAP);
        assert!(out.contains("$filter=fyDeclared"), "preserves filter");
        assert!(out.contains("$top=500"), "appends top");
        assert!(matches!(
            kind,
            PaginationCap::Rewritten {
                prior_top: None,
                new_top: 500
            }
        ));
    }

    #[test]
    fn unparseable_top_treated_as_no_top() {
        // OData spec says $top is a non-negative integer. A garbage
        // value means we can't trust it; the rewriter overwrites it
        // with the cap rather than appending a second $top= (which
        // would leave the server to pick which one wins). The garbage
        // is gone; `prior_top` reports None because we couldn't parse
        // the prior value.
        let url = "https://service.example.com/api?$select=x&$top=lots";
        let (out, kind) = cap_pagination(url, CAP);
        assert!(out.contains("$top=500"), "got {out}");
        assert!(!out.contains("$top=lots"), "garbage value should be replaced, got {out}");
        assert!(matches!(
            kind,
            PaginationCap::Rewritten {
                prior_top: None,
                new_top: 500
            }
        ));
    }

    #[test]
    fn case_sensitive_on_odata_keys() {
        // OData spec is case-sensitive; $SELECT is not a system key.
        let url = "https://example.com/api?$SELECT=name";
        let (out, kind) = cap_pagination(url, CAP);
        assert_eq!(out, url);
        assert_eq!(kind, PaginationCap::NotApplicable);
    }
}
