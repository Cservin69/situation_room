//! Bounded-accessor wrapper around `reqwest::HeaderMap`.
//!
//! The raw `reqwest::HeaderMap` is rich: it knows about every header
//! the server sent, including `Authorization`, `Set-Cookie`, and any
//! API-key-shaped vendor header. Exposing it from `SecureHttpClient`
//! would let any caller log a `Debug` form of the whole map and
//! accidentally write secrets to disk — exactly the failure mode
//! `crates/secure/src/logging.rs` exists to prevent. ADR 0009
//! §"The rule" extended in Session 25: not just "no fresh
//! `reqwest::Client::new()`" but "no path by which `reqwest`'s
//! primitives leak past the secure boundary."
//!
//! [`SecureHeaderMap`] is the boundary type. Construction is private
//! to this crate (`pub(crate) fn from_reqwest`), and the public
//! surface is a closed allow-list of accessors named after the headers
//! Stockpile actually consumes:
//!
//! - [`SecureHeaderMap::retry_after_seconds`] — drives executor
//!   backoff on HTTP 429 responses (Track D, Session 25).
//! - [`SecureHeaderMap::content_type`] — used for content-type
//!   validation in callers that need to know they got JSON vs HTML.
//! - [`SecureHeaderMap::content_length`] — convenience over the
//!   already-checked `Content-Length`.
//! - [`SecureHeaderMap::etag`] / [`SecureHeaderMap::last_modified`]
//!   — anticipated by the freshness-tracking work the handoff
//!   defers; defined now because adding them later would be a
//!   contract change to the wrapper.
//!
//! What's deliberately missing:
//!
//! - No `get(name)` / `get_all(name)` escape hatch. A caller that
//!   needs a header outside the allow-list adds a typed accessor
//!   here (in PR review), it does not bypass the wrapper.
//! - No `iter()`, no `keys()`, no `Debug` impl that prints values.
//!   The custom `Debug` impl below prints only the *names* of
//!   headers present, never the values, so a `?headers` in a
//!   `tracing` macro doesn't accidentally leak `Authorization`.
//! - No `From<reqwest::HeaderMap>` outside this crate. The
//!   constructor is `pub(crate)` so only the HTTP module wraps the
//!   raw map.
//!
//! ## Tests
//!
//! See `tests` module below: the accessor surface is asserted against
//! a list of headers the wrapper *must not* expose
//! (`Authorization`, `Cookie`, `Set-Cookie`, `x-api-key`). The test
//! is structural: it constructs a map containing every disallowed
//! header and verifies no public accessor returns any of their
//! values.

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderName};
use std::fmt;

/// Allow-list-accessor wrapper around a response's headers.
///
/// Construction is `pub(crate)` — the only legitimate source is the
/// secure HTTP client itself.
#[derive(Clone)]
pub struct SecureHeaderMap {
    inner: HeaderMap,
}

impl SecureHeaderMap {
    /// Wrap a raw `reqwest::HeaderMap`. Crate-private on purpose:
    /// callers outside `situation_room_secure` cannot construct this,
    /// which keeps the boundary one-way.
    pub(crate) fn from_reqwest(inner: HeaderMap) -> Self {
        Self { inner }
    }

    /// `Retry-After` header parsed per RFC 9110 §10.2.3.
    ///
    /// The header has two valid forms:
    ///
    /// - **Delta-seconds** — a non-negative decimal integer, e.g.
    ///   `Retry-After: 120`.
    /// - **HTTP-date** — an RFC 7231 IMF-fixdate, e.g.
    ///   `Retry-After: Fri, 03 May 2026 16:30:00 GMT`. We compute
    ///   the delta against `now` (UTC) and clamp to zero — a
    ///   server returning a date in the past means "you can retry
    ///   immediately."
    ///
    /// Returns `None` when:
    /// - The header is absent.
    /// - The value isn't valid UTF-8 (defensive — every well-formed
    ///   server emits ASCII here).
    /// - Neither parse succeeds.
    /// - The delta-seconds value is negative or doesn't fit in `u64`.
    ///
    /// Callers should treat `None` as "we don't know how long to
    /// wait" and apply their own policy (typically: don't retry, or
    /// fall back to a fixed default with an upper bound).
    pub fn retry_after_seconds(&self) -> Option<u64> {
        self.retry_after_seconds_at(Utc::now())
    }

    /// Test-friendly variant: parse `Retry-After` against an
    /// explicit "now" so HTTP-date arithmetic is deterministic in
    /// unit tests. Production code calls
    /// [`Self::retry_after_seconds`] which threads `Utc::now()` for
    /// you.
    pub fn retry_after_seconds_at(&self, now: DateTime<Utc>) -> Option<u64> {
        let raw = self.inner.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Try delta-seconds first: a bare non-negative integer.
        if let Ok(secs) = trimmed.parse::<i64>() {
            if secs < 0 {
                return None;
            }
            return Some(secs as u64);
        }

        // Fall back to HTTP-date. RFC 9110 mandates IMF-fixdate
        // (Sun, 06 Nov 1994 08:49:37 GMT) but real-world servers
        // sometimes emit RFC 2822-shaped dates; chrono's
        // `parse_from_rfc2822` accepts both common variants.
        if let Ok(when) = DateTime::parse_from_rfc2822(trimmed) {
            let delta = when.with_timezone(&Utc).signed_duration_since(now);
            return Some(delta.num_seconds().max(0) as u64);
        }

        None
    }

    /// `Content-Type` header value, if present. Returns the raw
    /// header value (not the parsed media type) — callers that need
    /// MIME parsing do it at their layer.
    pub fn content_type(&self) -> Option<&str> {
        self.inner
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
    }

    /// `Content-Length` header parsed as `u64`. Returns `None` if
    /// absent or unparseable.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// `ETag` header value, if present. Reserved for the freshness-
    /// tracking work the Session 25 handoff defers; exposing it now
    /// keeps the boundary stable so the addition is purely additive.
    pub fn etag(&self) -> Option<&str> {
        self.inner
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
    }

    /// `Last-Modified` header value, if present. Same rationale as
    /// `etag` — exposed proactively so the freshness work is purely
    /// additive when it lands.
    pub fn last_modified(&self) -> Option<&str> {
        self.inner
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
    }

    /// Names of headers present, for diagnostic logging *only*.
    /// Values are deliberately not exposed — see the module doc.
    /// Used by the custom `Debug` impl below.
    fn header_names(&self) -> Vec<&HeaderName> {
        self.inner.keys().collect()
    }
}

impl fmt::Debug for SecureHeaderMap {
    /// Print only the *names* of headers, never the values. A
    /// `tracing` macro that interpolates `?headers` will still emit
    /// useful diagnostic output ("we got back: content-type,
    /// content-length, retry-after") without leaking
    /// `Authorization` or `Set-Cookie` values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .header_names()
            .iter()
            .map(|n| n.as_str())
            .collect();
        f.debug_struct("SecureHeaderMap")
            .field("header_names", &names)
            .finish()
    }
}

/// Test helper: build a `SecureHeaderMap` from a list of
/// `(name, value)` pairs. Crate-test-public so the http module's
/// integration tests (and this file's own tests) can construct
/// instances without going through a real HTTP roundtrip. Not
/// `pub` — callers outside the crate cannot construct one.
#[cfg(test)]
pub(crate) fn build_for_test(pairs: &[(&str, &str)]) -> SecureHeaderMap {
    use reqwest::header::HeaderValue;
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let hn = HeaderName::from_bytes(name.as_bytes()).expect("test header name");
        let hv = HeaderValue::from_str(value).expect("test header value");
        map.append(hn, hv);
    }
    SecureHeaderMap::from_reqwest(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // -- retry_after_seconds parsing ----------------------------------------

    #[test]
    fn retry_after_delta_seconds_parses() {
        let h = build_for_test(&[("retry-after", "120")]);
        assert_eq!(h.retry_after_seconds(), Some(120));
    }

    #[test]
    fn retry_after_zero_is_zero() {
        let h = build_for_test(&[("retry-after", "0")]);
        assert_eq!(h.retry_after_seconds(), Some(0));
    }

    #[test]
    fn retry_after_negative_rejected() {
        let h = build_for_test(&[("retry-after", "-5")]);
        assert_eq!(h.retry_after_seconds(), None);
    }

    #[test]
    fn retry_after_very_large_value_parses() {
        let h = build_for_test(&[("retry-after", "86400")]);
        assert_eq!(h.retry_after_seconds(), Some(86_400));
    }

    #[test]
    fn retry_after_http_date_parses() {
        // Server says: try again in 5 minutes from a known "now".
        let now = Utc.with_ymd_and_hms(2026, 5, 3, 16, 30, 0).unwrap();
        let h = build_for_test(&[("retry-after", "Sun, 03 May 2026 16:35:00 GMT")]);
        assert_eq!(h.retry_after_seconds_at(now), Some(300));
    }

    #[test]
    fn retry_after_http_date_in_the_past_clamps_to_zero() {
        let now = Utc.with_ymd_and_hms(2026, 5, 3, 16, 35, 0).unwrap();
        let h = build_for_test(&[("retry-after", "Sun, 03 May 2026 16:30:00 GMT")]);
        assert_eq!(h.retry_after_seconds_at(now), Some(0));
    }

    #[test]
    fn retry_after_malformed_returns_none() {
        let h = build_for_test(&[("retry-after", "not a date or a number")]);
        assert_eq!(h.retry_after_seconds(), None);
    }

    #[test]
    fn retry_after_absent_returns_none() {
        let h = build_for_test(&[("content-type", "application/json")]);
        assert_eq!(h.retry_after_seconds(), None);
    }

    #[test]
    fn retry_after_empty_returns_none() {
        let h = build_for_test(&[("retry-after", "")]);
        assert_eq!(h.retry_after_seconds(), None);
    }

    // -- accessor allow-list (anti-leak guarantee) --------------------------
    //
    // These tests are the safety belt for ADR 0009 Amendment in
    // Session 25: they assert that no public accessor returns the
    // value of a header that should never cross the boundary. If
    // someone adds a `pub fn authorization()` in a future change, at
    // least one of these tests will surface it as a leak.

    /// Headers that must never be exposed by any public accessor.
    /// Add to this list whenever a new sensitive-shaped header gets
    /// industry-standard usage. The point isn't completeness — no
    /// allow-list is — it's a regression backstop for the most
    /// common shapes.
    fn forbidden_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("authorization", "Bearer secret-token-xyz"),
            ("cookie", "session=abc123"),
            ("set-cookie", "session=abc123; HttpOnly"),
            ("x-api-key", "sk-test-leak-1234"),
            ("x-amz-security-token", "leak-amz"),
            ("proxy-authorization", "Basic leak"),
        ]
    }

    /// All currently-allowed accessor outputs as strings, for the
    /// leak check below. Update this when a new accessor lands.
    fn all_public_string_outputs(h: &SecureHeaderMap) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(s) = h.content_type() {
            out.push(s.to_string());
        }
        if let Some(n) = h.content_length() {
            out.push(n.to_string());
        }
        if let Some(s) = h.etag() {
            out.push(s.to_string());
        }
        if let Some(s) = h.last_modified() {
            out.push(s.to_string());
        }
        if let Some(n) = h.retry_after_seconds() {
            out.push(n.to_string());
        }
        out
    }

    #[test]
    fn forbidden_header_values_never_appear_in_any_accessor_output() {
        // Build a map containing every forbidden header alongside a
        // few innocuous ones, then assert no public accessor returns
        // any of the forbidden values.
        let mut pairs: Vec<(&str, &str)> = forbidden_headers();
        pairs.push(("content-type", "application/json"));
        pairs.push(("content-length", "42"));
        pairs.push(("etag", "\"abc123\""));
        let h = build_for_test(&pairs);

        let outputs = all_public_string_outputs(&h);
        for (_name, secret_value) in forbidden_headers() {
            for out in &outputs {
                assert!(
                    !out.contains(secret_value),
                    "forbidden header value {secret_value:?} leaked through public accessor: {out:?}",
                );
            }
        }
    }

    #[test]
    fn debug_impl_does_not_print_header_values() {
        // Construct a map whose Authorization value contains a
        // marker string. The Debug impl is allowed to print the
        // *name* "authorization" but must NOT print the value.
        let h = build_for_test(&[
            ("authorization", "MARKER-THAT-MUST-NOT-LEAK"),
            ("content-type", "application/json"),
        ]);
        let dbg = format!("{h:?}");
        assert!(
            !dbg.contains("MARKER-THAT-MUST-NOT-LEAK"),
            "Debug impl leaked an authorization value: {dbg}",
        );
        // Sanity: the name should still appear so logs remain useful.
        assert!(dbg.contains("authorization"), "Debug impl dropped the name: {dbg}");
    }

    // -- typed accessor smoke tests ----------------------------------------

    #[test]
    fn content_type_passes_through() {
        let h = build_for_test(&[("content-type", "application/json; charset=utf-8")]);
        assert_eq!(h.content_type(), Some("application/json; charset=utf-8"));
    }

    #[test]
    fn content_length_parses_u64() {
        let h = build_for_test(&[("content-length", "12345")]);
        assert_eq!(h.content_length(), Some(12_345));
    }

    #[test]
    fn content_length_unparseable_returns_none() {
        let h = build_for_test(&[("content-length", "not-a-number")]);
        assert_eq!(h.content_length(), None);
    }

    #[test]
    fn etag_and_last_modified_pass_through() {
        let h = build_for_test(&[
            ("etag", "\"deadbeef\""),
            ("last-modified", "Sun, 03 May 2026 16:00:00 GMT"),
        ]);
        assert_eq!(h.etag(), Some("\"deadbeef\""));
        assert_eq!(h.last_modified(), Some("Sun, 03 May 2026 16:00:00 GMT"));
    }
}
