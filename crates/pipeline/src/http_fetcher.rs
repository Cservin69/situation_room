//! HTTP-fetch abstraction for the fetch executor.
//!
//! The runtime executor (`fetch_executor`) needs to fetch URLs but
//! must remain testable without a real network. This module defines
//! a single-method trait the executor depends on, and provides a
//! blanket impl for [`situation_room_secure::http::SecureHttpClient`] so
//! production callers pass the secure client unchanged.
//!
//! ## Why the trait lives here, not in `secure`
//!
//! The trait's *consumer* is the pipeline executor. Putting it next
//! to the consumer keeps the abstraction shaped by what the executor
//! needs (one byte-returning GET) rather than by what `secure`
//! happens to expose. `secure` stays a leaf crate; `pipeline` already
//! depends on `secure`, so the impl block here is fine.
//!
//! ## What this is NOT
//!
//! Not a general HTTP abstraction. The executor only needs `GET →
//! bytes`; anything fancier (POST, headers, retries) goes through
//! `SecureHttpClient` directly and isn't part of this trait. Tests
//! that need to simulate richer responses can't use the in-memory
//! mock — they should run against a local hyper test server through
//! the real `SecureHttpClient`.
//!
//! ## Why an explicit error type
//!
//! `secure::HttpError` is the production error and is rich enough to
//! distinguish URL-rejected from response-too-large from status-
//! errored. Wrapping it as the variant of [`FetchError`] keeps the
//! executor's error surface small (one `Fetch` arm to match on)
//! without losing the underlying detail when logged.
//!
//! ## Track D, Session 25 — `Retry-After` surfacing
//!
//! Multi-session evidence (Sessions 23, 24-A, 24-B): GDELT and other
//! rate-limited sources hit pre-fetch with a 429 and no surfacing of
//! `Retry-After`. The executor logged the failure and moved on; the
//! operator had no way to know whether the failure was transient or
//! whether to wait, and re-running the plan would just hit the same
//! ceiling.
//!
//! Track D adds [`FetchError::RateLimited`] which carries the
//! parsed-out `Retry-After` (if any). The blanket impl on
//! `SecureHttpClient` distinguishes 429 specifically and reads the
//! header via the new [`SecureHttpClient::get_with_headers`] surface.
//! Other status errors keep the legacy `FetchError::Http` shape so
//! existing match-arms continue to compile unchanged.
//!
//! ## Session 32 — Content-Type surfacing
//!
//! The Session-31 response-bytes affordance in `RecipesPanel.svelte`
//! had to fall back to a heuristic byte-sniffer because the runtime
//! discarded the `Content-Type` header on its way through this trait
//! (the only method was `fetch_bytes -> Vec<u8>`). The chip read
//! `JSON` for any body starting with `{` or `[` even when the server
//! claimed `application/javascript`; conversely an HTML SPA shell
//! reading `text/html` was correctly heuristically classified, but
//! the operator had no way to know whether the chip was authoritative
//! or guessed.
//!
//! Session 32 adds [`HttpFetcher::fetch_bytes_with_meta`], returning
//! [`FetchedBytes`] which carries the raw `Content-Type` value
//! alongside the body. The trait method has a default impl that
//! delegates to `fetch_bytes` and returns `content_type: None` — so
//! every existing test mock (the `StaticFetcher` in particular)
//! continues to compile unchanged. The production
//! `SecureHttpClient` impl overrides to pull the header via the
//! `SecureHeaderMap::content_type()` accessor that already exists
//! in the secure crate's allow-list.
//!
//! Callers that don't need the header (the LLM-side `post_json`
//! provider, the prefetch excerpt builder when it doesn't care
//! about the type) keep using `fetch_bytes` and pay nothing. The
//! one caller that needs it — the apply-failure capture path in
//! `fetch_executor::record_apply_failure_attempt` — routes through
//! `fetch_with_backoff`, which threads the header into
//! `BackoffOutcome::Bytes` so it lands in the
//! `recipe_fetch_attempts.response_content_type` column for the
//! chip to consume.

use async_trait::async_trait;
use thiserror::Error;
use url::Url;

use situation_room_secure::http::{HttpError, SecureHttpClient};

/// Error raised by an [`HttpFetcher`] implementation.
#[derive(Debug, Error)]
pub enum FetchError {
    /// Wraps the production `secure::HttpError`. The Display impl
    /// preserves the underlying detail for logs.
    #[error("http error: {0}")]
    Http(String),

    /// The server returned 429 Too Many Requests. Carries the parsed
    /// `Retry-After` value in seconds, when the header was present
    /// and parseable per RFC 9110 §10.2.3 (delta-seconds or
    /// HTTP-date). `None` means the server emitted 429 with no
    /// machine-readable hint.
    ///
    /// Session-25 callers (the fetch executor) inspect this to
    /// decide between a short in-line wait, a surfaced "rate-limited;
    /// retry in Nm Ss" outcome, or a generic failure when the value
    /// is absent.
    #[error("rate-limited (retry_after_seconds={retry_after_seconds:?})")]
    RateLimited { retry_after_seconds: Option<u64> },

    /// Test/mock implementations use this for "no fixture for this
    /// URL". The real `SecureHttpClient` impl never returns this.
    #[error("no fixture configured for url: {0}")]
    NoFixture(String),
}

impl From<HttpError> for FetchError {
    fn from(e: HttpError) -> Self {
        // 429 with headers gets lifted to RateLimited; other shapes
        // collapse to the generic Http variant the codebase has had
        // since Session 8. Preserves backward-compat for every
        // non-429 caller.
        if let HttpError::StatusWithHeaders { status, headers } = &e {
            if *status == 429 {
                return FetchError::RateLimited {
                    retry_after_seconds: headers.retry_after_seconds(),
                };
            }
        }
        FetchError::Http(e.to_string())
    }
}

/// A fetched response body, plus the response `Content-Type` header
/// when the underlying transport surfaced it.
///
/// Returned by [`HttpFetcher::fetch_bytes_with_meta`]. Session 32:
/// see the module docs for why the meta lives alongside the body
/// rather than in a parallel call. `content_type` is the raw header
/// value (e.g. `application/json; charset=utf-8`) — the consumer
/// parses out the type/subtype if it cares.
///
/// `content_type` is `None` for two indistinguishable reasons:
///
///   - The transport didn't surface a header (the test
///     [`testing::StaticFetcher`] never does; the default trait impl
///     of `fetch_bytes_with_meta` in terms of `fetch_bytes` doesn't
///     either).
///   - The server returned no `Content-Type` header (some legacy
///     CSV endpoints, some misconfigured proxies).
///
/// The presentation layer that consumes this (the response-bytes
/// chip in `RecipesPanel.svelte`) treats `None` as "fall back to
/// the heuristic byte-sniffer." Honest about the absence rather
/// than papering over it.
#[derive(Debug, Clone)]
pub struct FetchedBytes {
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// Fetch a URL and return its body bytes. The single capability the
/// fetch executor needs from "the network".
///
/// Implementations must be `Send + Sync` so the executor can be
/// invoked from a Tauri command (which is `Send`-bounded across the
/// async boundary).
#[async_trait]
pub trait HttpFetcher: Send + Sync {
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError>;

    /// Fetch bytes plus the response `Content-Type` header value, when
    /// the underlying transport surfaces it. Session 32.
    ///
    /// Default impl wraps [`Self::fetch_bytes`] and returns
    /// `content_type: None` so every existing implementation —
    /// including the test [`testing::StaticFetcher`] — compiles
    /// unchanged. Implementations with header access (the production
    /// [`SecureHttpClient`] impl) override to populate.
    ///
    /// Why a sibling method rather than replacing `fetch_bytes`'s
    /// return type: the LLM-side caller (`crates/llm`) doesn't care
    /// about content-type and doesn't want to allocate/destructure a
    /// wrapper struct on every call. Adding a method preserves the
    /// happy-path call shape for callers that don't need the meta.
    async fn fetch_bytes_with_meta(&self, url: &str) -> Result<FetchedBytes, FetchError> {
        let body = self.fetch_bytes(url).await?;
        Ok(FetchedBytes {
            body,
            content_type: None,
        })
    }
}

#[async_trait]
impl HttpFetcher for SecureHttpClient {
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        // Track D: route GETs through the headers-aware path so a
        // 429 surfaces as `FetchError::RateLimited { retry_after_seconds }`
        // rather than collapsing into a bare `Http("status error: 429")`.
        // The body-only `get_bytes` would discard the header before
        // we could read it; the executor's backoff logic depends on
        // the parsed value being available.
        //
        // Session 32: callers that also need the response Content-Type
        // go through `fetch_bytes_with_meta` below; this method stays
        // body-only for the existing happy-path callers (the LLM
        // provider, the static-payload short-circuit verifier, and
        // any new caller that explicitly doesn't care).
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return Err(FetchError::Http(format!("invalid url: {e}"))),
        };
        match self.get_with_headers(&parsed).await {
            Ok(resp) => Ok(resp.body),
            Err(e) => Err(FetchError::from(e)),
        }
    }

    /// Override of the trait's default impl to populate
    /// `content_type` from the real response header. The header
    /// passes through `SecureHeaderMap::content_type()` — the closed
    /// allow-list accessor from the secure crate (ADR 0009 amendment
    /// in Session 25). The raw string is returned verbatim so the
    /// presentation layer can parse out parameters (`charset=`,
    /// `boundary=`) if it needs them; today only the type/subtype is
    /// consumed by the chip in `RecipesPanel.svelte`.
    async fn fetch_bytes_with_meta(&self, url: &str) -> Result<FetchedBytes, FetchError> {
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return Err(FetchError::Http(format!("invalid url: {e}"))),
        };
        match self.get_with_headers(&parsed).await {
            Ok(resp) => {
                let content_type = resp.headers.content_type().map(|s| s.to_string());
                Ok(FetchedBytes {
                    body: resp.body,
                    content_type,
                })
            }
            Err(e) => Err(FetchError::from(e)),
        }
    }
}

#[cfg(test)]
pub mod testing {
    //! In-memory fetcher for unit tests.
    //!
    //! Only compiled under `cfg(test)` because production code must
    //! never see a `HashMap`-backed fetcher. The fetch_executor's
    //! test module imports this through `super::http_fetcher::testing`.

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A `HttpFetcher` backed by a pre-populated map of URL → bytes.
    /// Calls to URLs not in the map return [`FetchError::NoFixture`].
    pub struct StaticFetcher {
        fixtures: Mutex<HashMap<String, Vec<u8>>>,
        rate_limited_urls: Mutex<HashMap<String, Option<u64>>>,
        /// Per-URL Content-Type override for the
        /// `fetch_bytes_with_meta` path (Session 32). When unset for
        /// a URL, the override returns `None` — matching the trait's
        /// default impl behaviour. When set, `fetch_bytes_with_meta`
        /// returns the override; `fetch_bytes` is unaffected. Tests
        /// exercising the apply-failure capture path use this to
        /// simulate "server told us text/html when the recipe
        /// expected JSON."
        content_types: Mutex<HashMap<String, String>>,
    }

    impl Default for StaticFetcher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl StaticFetcher {
        pub fn new() -> Self {
            Self {
                fixtures: Mutex::new(HashMap::new()),
                rate_limited_urls: Mutex::new(HashMap::new()),
                content_types: Mutex::new(HashMap::new()),
            }
        }

        pub fn with(mut self, url: &str, bytes: &[u8]) -> Self {
            self.fixtures
                .get_mut()
                .unwrap()
                .insert(url.to_string(), bytes.to_vec());
            self
        }

        /// Configure the `Content-Type` value
        /// `fetch_bytes_with_meta` will return for this URL. Used by
        /// the Session-32 apply-failure capture tests to verify the
        /// header lands in the storage row.
        #[allow(dead_code)]
        pub fn with_content_type(mut self, url: &str, content_type: &str) -> Self {
            self.content_types
                .get_mut()
                .unwrap()
                .insert(url.to_string(), content_type.to_string());
            self
        }

        /// Configure a URL to return [`FetchError::RateLimited`] with
        /// the given retry-after value. Used by Track D's executor
        /// backoff tests.
        #[allow(dead_code)]
        pub fn rate_limited(mut self, url: &str, retry_after_seconds: Option<u64>) -> Self {
            self.rate_limited_urls
                .get_mut()
                .unwrap()
                .insert(url.to_string(), retry_after_seconds);
            self
        }
    }

    #[async_trait]
    impl HttpFetcher for StaticFetcher {
        async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
            // Rate-limit fixtures take precedence so a URL can be
            // configured for both shapes (fixture present, but the
            // first fetch returns 429) — useful when the
            // backoff-and-retry test case lands.
            let rl = self
                .rate_limited_urls
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?;
            if let Some(retry_after_seconds) = rl.get(url) {
                return Err(FetchError::RateLimited {
                    retry_after_seconds: *retry_after_seconds,
                });
            }
            drop(rl);

            let map = self
                .fixtures
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?;
            map.get(url)
                .cloned()
                .ok_or_else(|| FetchError::NoFixture(url.to_string()))
        }

        /// Override of the trait default so a configured
        /// `with_content_type` value rides through to callers that
        /// route via `fetch_bytes_with_meta`. Without this override,
        /// the default impl would drop back to `fetch_bytes` and
        /// always return `None` — defeating the point of the
        /// configured value.
        async fn fetch_bytes_with_meta(&self, url: &str) -> Result<FetchedBytes, FetchError> {
            let body = self.fetch_bytes(url).await?;
            let content_type = self
                .content_types
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?
                .get(url)
                .cloned();
            Ok(FetchedBytes { body, content_type })
        }
    }
}
