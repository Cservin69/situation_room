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
//! Other status errors kept the legacy `FetchError::Http` shape at
//! the time so existing match-arms continued to compile unchanged.
//! Session 49 lifts those out into [`FetchError::Status`] — see the
//! "typed status surfacing" section below.
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
//! ## Session 45 — typed timeout surfacing
//!
//! Pre-Session-45 the `From<HttpError>` impl flattened
//! `HttpError::Timeout(Duration)` into the catch-all `FetchError::Http`
//! variant alongside every other non-429 shape. That worked for the
//! recipe-failure-message path (the operator sees the same string
//! regardless of source) but blocked the per-host backoff layer from
//! distinguishing "timeout — adapt" from "404 — don't adapt." Session
//! 45 lifts `HttpError::Timeout` into [`FetchError::Timeout`], mirroring
//! the Track-D treatment of 429: the variants the host-backoff state
//! machine reacts to are typed, the rest collapse into the generic arm.
//!
//! The new variant is consumed by
//! [`crate::fetch_backoff::BackoffFetcher`] (Session 45) which records
//! a per-host backoff signal whenever a request returns `Timeout` or
//! `RateLimited`. The fetch-executor adds one new match-arm in
//! `fetch_recipe_bytes` to surface the timeout reason in the
//! per-recipe failure message.
//!
//! ## Session 49 — typed status surfacing
//!
//! The third typed lift after `RateLimited` (Session 25) and `Timeout`
//! (Session 45). Pre-Session-49, every non-429, non-timeout
//! [`HttpError`] variant collapsed into [`FetchError::Http(String)`] —
//! including the status-coded ones. The Session-48 live run on the
//! lithium plan exposed the cost: the fetch executor's `prefetch_excerpt`
//! path discarded the failure class entirely when it wrote the
//! "fetch failed" entry into a nomination's prior-attempts history. The
//! propose-URL prompt (v1.0) explicitly distinguishes
//! `fetch failed: 404` (wrong path on this host — try a different
//! path) from `fetch failed: 403/401` (host is blocking us — try a
//! different host or decline) from a generic timeout (slow host —
//! adapt or move on). Without the status code reaching the prompt,
//! every prefetch failure read the same and the proposer either
//! re-tried the same host blindly or declined too quickly.
//!
//! Session 49 lifts:
//!
//! - [`FetchError::Status(u16)`] — every non-429 4xx/5xx response,
//!   regardless of whether the underlying [`HttpError`] was the
//!   body-only `Status` or the headers-aware `StatusWithHeaders`.
//! - [`FetchError::TooLarge { max, got }`] — the response exceeded
//!   `SecureHttpConfig::max_response_bytes`. The numbers travel
//!   through so the executor can name them in the prompt input
//!   (`fetch failed: response too large (got at least N, max M)`)
//!   without re-parsing the Display string.
//!
//! Other shapes (DNS, TLS, redirect-rejected, URL-guard-rejected,
//! generic `Request(_)`) continue to collapse to the generic
//! [`FetchError::Http`] arm; the prompt has no host-class heuristic
//! for them and the message-string-as-reason carries enough detail
//! for the LLM's general knowledge to route around. Widening the
//! lift further is its own design decision.
//!
//! Both new variants follow the Session 45 pattern: typed because
//! callers need to react differently, not because the operator surface
//! benefits from a richer Display string.
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

use std::time::Duration;

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

    /// The request didn't complete within the configured total
    /// timeout. Session 45 — see the module-level "typed timeout
    /// surfacing" section. Carries the configured timeout so the
    /// failure message can name what the request was budgeted for
    /// rather than just "request failed".
    ///
    /// The host-backoff layer
    /// ([`crate::fetch_backoff::BackoffFetcher`]) reacts to this
    /// variant the same way it reacts to a 429 with no
    /// `Retry-After`: increment the per-host failure counter and
    /// extend `next_allowed_at` by the uniform exponential backoff.
    /// The variant is the *signal*; the per-host adaptation is the
    /// *policy*; both stay generic.
    #[error("timed out after {0:?}")]
    Timeout(Duration),

    /// The server returned a non-429 4xx/5xx HTTP status. Session 49
    /// — see the module-level "typed status surfacing" section. The
    /// host-backoff layer ([`crate::fetch_backoff::BackoffFetcher`])
    /// does **not** react to this variant: a 404 says "wrong path,"
    /// not "this host is asking us to slow down." The variant exists
    /// so the prefetch-failed path can record a status-coded reason
    /// in the propose-URL prior-attempts history; that history is
    /// what the prompt's v1.0 distinct-handling-by-status-class
    /// heuristics consume.
    ///
    /// 429 specifically still routes through [`Self::RateLimited`]
    /// (Session 25) — the From<HttpError> match arm checks for 429
    /// before falling through to this variant.
    #[error("status error: {0}")]
    Status(u16),

    /// The response body exceeded
    /// [`situation_room_secure::http::SecureHttpConfig::max_response_bytes`].
    /// Session 49 — typed alongside `Status` so the executor can
    /// render the numbers without re-parsing the Display string.
    /// Callers presenting this to the propose-URL prompt may use the
    /// numbers to ask for a smaller endpoint (paginated API, daily
    /// rather than annual export); the prompt's vocabulary handles
    /// the case generically through the "fetch failed: ..." idiom.
    #[error("response too large: got at least {got} bytes, max {max}")]
    TooLarge { max: usize, got: usize },

    /// Test/mock implementations use this for "no fixture for this
    /// URL". The real `SecureHttpClient` impl never returns this.
    #[error("no fixture configured for url: {0}")]
    NoFixture(String),
}

impl From<HttpError> for FetchError {
    fn from(e: HttpError) -> Self {
        // 429 with headers gets lifted to RateLimited; Timeout gets
        // lifted to FetchError::Timeout (Session 45 — the host-backoff
        // layer needs to distinguish timeout from generic failure);
        // Session 49 lifts non-429 status codes and oversized
        // responses out of the generic `Http(String)` arm so the
        // prefetch-failed path can build status-class-aware reasons
        // without string-parsing the Display.
        //
        // Other shapes (DNS, TLS, redirect-rejected, URL-guard,
        // generic `Request(_)`) collapse to the generic Http variant
        // the codebase has had since Session 8. The propose-URL
        // prompt has no host-class heuristic for those; the message-
        // -as-reason carries enough detail for the LLM's general
        // knowledge to route around.
        match &e {
            HttpError::StatusWithHeaders { status, headers, .. } if *status == 429 => {
                FetchError::RateLimited {
                    retry_after_seconds: headers.retry_after_seconds(),
                }
            }
            HttpError::Timeout(d) => FetchError::Timeout(*d),
            // Session 49: lift non-429 status codes (both the body-only
            // `Status` and the headers-aware `StatusWithHeaders`
            // shapes). The 429 arm above wins on order; for any other
            // status code we surface the typed variant.
            HttpError::Status(code) => FetchError::Status(*code),
            HttpError::StatusWithHeaders { status, .. } => FetchError::Status(*status),
            // Session 49: lift the oversized-response shape so the
            // numbers travel verbatim. The Display string is bounded
            // and human-readable, but the executor benefits from
            // typed access for prompt-input formatting and for any
            // future host-adaptation policy that wants to react
            // differently to "the host is shipping a 100MB landing
            // page" vs. "wrong path."
            HttpError::ResponseTooLarge { max, got } => FetchError::TooLarge {
                max: *max,
                got: *got,
            },
            _ => FetchError::Http(e.to_string()),
        }
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
        /// Per-URL configured timeout — when present, both
        /// `fetch_bytes` and `fetch_bytes_with_meta` return
        /// `FetchError::Timeout(duration)` for the configured URL.
        /// Session 45 — used by the `BackoffFetcher` tests in
        /// `fetch_backoff` to verify the per-host adaptation reacts
        /// to typed Timeouts.
        timeouts: Mutex<HashMap<String, Duration>>,
        /// Per-URL configured non-429 status — when present, both
        /// `fetch_bytes` and `fetch_bytes_with_meta` return
        /// `FetchError::Status(code)` for the configured URL.
        /// Session 49 — used by the executor tests to verify the
        /// prefetch-failed branch maps each status class into the
        /// propose-URL prompt's vocabulary.
        statuses: Mutex<HashMap<String, u16>>,
        /// Per-URL configured oversized-response — when present, both
        /// `fetch_bytes` and `fetch_bytes_with_meta` return
        /// `FetchError::TooLarge { max, got }` for the configured URL.
        /// Session 49.
        too_large: Mutex<HashMap<String, (usize, usize)>>,
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
                timeouts: Mutex::new(HashMap::new()),
                statuses: Mutex::new(HashMap::new()),
                too_large: Mutex::new(HashMap::new()),
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

        /// Configure a URL to return [`FetchError::Timeout`] with the
        /// given duration. Session 45 — used by the per-host backoff
        /// tests to verify `BackoffFetcher` records timeouts as
        /// host-adaptation signals.
        #[allow(dead_code)]
        pub fn timeout(mut self, url: &str, after: Duration) -> Self {
            self.timeouts
                .get_mut()
                .unwrap()
                .insert(url.to_string(), after);
            self
        }

        /// Configure a URL to return [`FetchError::Status`] with the
        /// given non-429 status code. Session 49. The `code == 429`
        /// case is intentionally excluded — that path goes through
        /// [`Self::rate_limited`] so the typed `RateLimited` shape
        /// surfaces correctly.
        #[allow(dead_code)]
        pub fn status(mut self, url: &str, code: u16) -> Self {
            assert_ne!(
                code, 429,
                "use rate_limited(...) for 429 — the typed RateLimited variant carries Retry-After"
            );
            self.statuses
                .get_mut()
                .unwrap()
                .insert(url.to_string(), code);
            self
        }

        /// Configure a URL to return [`FetchError::TooLarge`] with the
        /// given (max, got) byte counts. Session 49.
        #[allow(dead_code)]
        pub fn too_large(mut self, url: &str, max: usize, got: usize) -> Self {
            self.too_large
                .get_mut()
                .unwrap()
                .insert(url.to_string(), (max, got));
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

            // Session 45: typed-Timeout fixtures, same precedence
            // pattern as rate-limited. If both are configured for
            // the same URL, rate-limit wins (defined order above).
            let to = self
                .timeouts
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?;
            if let Some(d) = to.get(url) {
                return Err(FetchError::Timeout(*d));
            }
            drop(to);

            // Session 49: typed-Status fixtures. Defined precedence
            // is rate-limited → timeout → status → too-large → bytes;
            // tests configure exactly one shape per URL in practice.
            let st = self
                .statuses
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?;
            if let Some(code) = st.get(url) {
                return Err(FetchError::Status(*code));
            }
            drop(st);

            // Session 49: typed-TooLarge fixtures.
            let tl = self
                .too_large
                .lock()
                .map_err(|e| FetchError::Http(format!("test fixture lock poisoned: {e}")))?;
            if let Some((max, got)) = tl.get(url) {
                return Err(FetchError::TooLarge {
                    max: *max,
                    got: *got,
                });
            }
            drop(tl);

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

#[cfg(test)]
mod tests {
    //! Unit tests for the `From<HttpError>` lift. The trait, blanket
    //! impl on `SecureHttpClient`, and `StaticFetcher` are exercised
    //! by integration-shaped tests elsewhere in the crate (the
    //! fetch_executor and fetch_backoff modules); these tests pin the
    //! conversion shape so a future refactor doesn't silently
    //! collapse a typed variant back into the catch-all.
    //!
    //! Note: the 429 → `RateLimited` arm requires constructing a
    //! `SecureHeaderMap`, which is `pub(crate)` in the secure crate
    //! by design (ADR 0009 §"The rule" extended in Session 25).
    //! That arm is exercised end-to-end by the live integration tests
    //! that hit a real 429-emitting server through `SecureHttpClient`
    //! — we don't reach across the boundary to test it here.

    use super::*;

    /// Session 45. The typed `HttpError::Timeout(Duration)` lifts to
    /// `FetchError::Timeout(Duration)` rather than collapsing into the
    /// catch-all `Http(String)` variant. The host-backoff layer
    /// depends on this lift to distinguish "host is timing out"
    /// from "404, don't adapt."
    #[test]
    fn timeout_lifts_to_typed_variant_session_45() {
        let dur = Duration::from_secs(42);
        let lifted = FetchError::from(situation_room_secure::http::HttpError::Timeout(dur));
        match lifted {
            FetchError::Timeout(d) => assert_eq!(d, dur),
            other => panic!("expected FetchError::Timeout, got {other:?}"),
        }
    }

    /// Session 49. The typed `HttpError::Status(u16)` (body-only path)
    /// lifts to `FetchError::Status(u16)` rather than collapsing into
    /// the catch-all `Http(String)` variant. The prefetch-failed path
    /// in `fetch_executor::author_for_nomination` depends on this lift
    /// to record a status-coded reason in the propose-URL prior-
    /// attempts history; the prompt's v1.0 vocabulary distinguishes
    /// `fetch failed: 404` from `fetch failed: 403/401`.
    #[test]
    fn status_lifts_to_typed_variant_session_49() {
        for code in [400u16, 401, 403, 404, 410, 451, 500, 502, 503] {
            let lifted = FetchError::from(situation_room_secure::http::HttpError::Status(code));
            match lifted {
                FetchError::Status(s) => assert_eq!(s, code, "status code must round-trip"),
                other => panic!("expected FetchError::Status({code}), got {other:?}"),
            }
        }
    }

    /// Session 49. The 429-with-headers arm in `From<HttpError>` is
    /// checked before the catch-all status lift — Track-D's
    /// `RateLimited` path stays intact even after the new typed
    /// `Status` lift was added below it. The headers-aware
    /// `StatusWithHeaders` variant carries the `SecureHeaderMap` the
    /// `RateLimited` arm needs to parse `Retry-After`; we don't
    /// reach across the secure-crate boundary to construct one here
    /// (see this module's tests-section docstring), so this test
    /// asserts the body-only `Status(429)` path — which has no
    /// header to parse — lifts to the typed `Status(429)`. The
    /// production flow goes through `StatusWithHeaders` and continues
    /// to land in `RateLimited`; that arm is exercised by the live
    /// integration tests.
    #[test]
    fn body_only_429_lifts_to_status_session_49() {
        let lifted = FetchError::from(situation_room_secure::http::HttpError::Status(429));
        match lifted {
            FetchError::Status(429) => {
                // The honest shape for "server said 429 but we have
                // no header to parse." The executor's prefetch-failed
                // path renders this as "fetch failed: 429" into the
                // propose-URL prior-attempts history. The host-
                // backoff layer doesn't react: it only adapts on
                // `RateLimited` (with parsed Retry-After) and
                // `Timeout`. A header-less 429 is treated as a
                // generic upstream error rather than a fresh
                // throttling signal — the right call when the server
                // told us nothing about when to retry.
            }
            other => panic!("body-only 429 must lift to Status(429), got {other:?}"),
        }
    }

    /// Session 49. The typed `HttpError::ResponseTooLarge { max, got }`
    /// lifts to `FetchError::TooLarge { max, got }` with both numbers
    /// preserved verbatim. The prefetch-failed path renders these into
    /// the propose-URL prompt input.
    #[test]
    fn too_large_lifts_to_typed_variant_session_49() {
        let lifted =
            FetchError::from(situation_room_secure::http::HttpError::ResponseTooLarge {
                max: 32 * 1024 * 1024,
                got: 50 * 1024 * 1024,
            });
        match lifted {
            FetchError::TooLarge { max, got } => {
                assert_eq!(max, 32 * 1024 * 1024);
                assert_eq!(got, 50 * 1024 * 1024);
            }
            other => panic!("expected FetchError::TooLarge, got {other:?}"),
        }
    }

    /// Session 49. Non-typed shapes (DNS failure, TLS handshake,
    /// redirect rejection, malformed-URL) continue to collapse into
    /// the generic `Http(String)` arm. The propose-URL prompt has no
    /// status-class heuristic for these; the message-as-reason carries
    /// enough detail for the LLM's general knowledge.
    ///
    /// This test replaces the pre-Session-49
    /// `other_http_errors_still_collapse_to_http_variant_session_45`
    /// (which used `HttpError::Status(503)` — that case now lifts to
    /// the typed `Status` variant per the Session-49 module docs).
    #[test]
    fn dns_and_tls_errors_still_collapse_to_http_variant_session_49() {
        let dns = FetchError::from(situation_room_secure::http::HttpError::Request(
            "dns resolution failed".to_string(),
        ));
        match dns {
            FetchError::Http(msg) => {
                assert!(
                    msg.contains("dns"),
                    "message should preserve the underlying detail (got {msg:?})"
                );
            }
            other => panic!("DNS errors must still flatten to Http (got {other:?})"),
        }

        let tls = FetchError::from(situation_room_secure::http::HttpError::Tls(
            "handshake aborted".to_string(),
        ));
        match tls {
            FetchError::Http(msg) => {
                assert!(
                    msg.contains("handshake") || msg.contains("tls"),
                    "TLS message must surface (got {msg:?})"
                );
            }
            other => panic!("TLS errors must still flatten to Http (got {other:?})"),
        }
    }
}
