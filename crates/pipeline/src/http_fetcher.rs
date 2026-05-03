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

/// Fetch a URL and return its body bytes. The single capability the
/// fetch executor needs from "the network".
///
/// Implementations must be `Send + Sync` so the executor can be
/// invoked from a Tauri command (which is `Send`-bounded across the
/// async boundary).
#[async_trait]
pub trait HttpFetcher: Send + Sync {
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError>;
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
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return Err(FetchError::Http(format!("invalid url: {e}"))),
        };
        match self.get_with_headers(&parsed).await {
            Ok(resp) => Ok(resp.body),
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
            }
        }

        pub fn with(mut self, url: &str, bytes: &[u8]) -> Self {
            self.fixtures
                .get_mut()
                .unwrap()
                .insert(url.to_string(), bytes.to_vec());
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
    }
}
