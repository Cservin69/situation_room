//! Rate-limit-aware fetch wrapper for the executor.
//!
//! Track D, Session 25. Wraps [`HttpFetcher::fetch_bytes`] with the
//! Retry-After-honoring backoff policy described in the Session-25
//! handoff:
//!
//! - **`Retry-After` present and ≤ 60s**: sleep that long, retry once.
//!   If the retry succeeds, the caller never sees the rate-limit; if
//!   it fails again (with a fresh `Retry-After`, even if shorter), we
//!   surface it as [`BackoffOutcome::RateLimited`] without further
//!   retry. This is the "transient throttling" case — most public
//!   APIs publish 1-60s windows for routine throttling.
//! - **`Retry-After` present and > 60s**: surface immediately as
//!   [`BackoffOutcome::RateLimited`]. Sleeping that long inside the
//!   executor would block the GUI's run-fetch button for a duration
//!   the operator can't see; surfacing the wait makes it the
//!   operator's choice (re-run later, switch sources, etc.).
//! - **`Retry-After` absent** (server returned 429 with no header):
//!   surface as [`BackoffOutcome::RateLimited { retry_after_seconds:
//!   None }`]. No retry — without a hint, blind retry is just noise.
//! - **Other errors**: pass through as [`BackoffOutcome::Failed`].
//!
//! The split between "retry inline" and "surface" is the only place
//! the executor takes a position on rate-limiting policy. The 60-second
//! threshold is a constant (`SHORT_BACKOFF_CEILING_SECS`); changing it
//! is a one-line change.
//!
//! ## Why a sibling module rather than inlining
//!
//! The four `run_X_recipe` paths all share this exact structure:
//!
//! 1. (optional) short-circuit on `static_payload`
//! 2. fetch bytes
//! 3. apply
//! 4. insert
//!
//! Step 2 is the one Track D needs to change. Inlining the backoff
//! logic into all four paths would duplicate ~30 lines four times;
//! extracting it as a helper preserves the
//! "duplication-with-comments over premature unification" Session-9
//! discipline at the recipe-runtime layer (each `run_X_recipe`
//! retains its own visible call to the helper at exactly one site)
//! while sharing the backoff policy itself.
//!
//! The helper is *not* a generic retry loop — it's specifically
//! about HTTP 429 with a parsed `Retry-After`. Other transient
//! failure classes (DNS, timeouts, 5xx) keep the no-retry behaviour
//! the executor has had since Session 8; widening retry policy is
//! its own design decision and would land alongside the
//! failure-mode taxonomy ADR 0012 deferred until 10 shapes
//! accumulate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::{info, warn};
use url::Url;

use crate::http_fetcher::{FetchError, FetchedBytes, HttpFetcher};

/// Maximum `Retry-After` value (in seconds) for which we sleep
/// inline and retry. Larger values surface to the operator without
/// the executor blocking.
const SHORT_BACKOFF_CEILING_SECS: u64 = 60;

/// Result of a backoff-aware fetch. Three shapes:
///
/// - `Bytes { body, content_type }` — the body, as if we'd never hit
///   a rate limit (either because we weren't, or because the inline
///   retry succeeded). Carries the response `Content-Type` header
///   value when the underlying fetcher surfaced one (Session 32);
///   `None` when it didn't.
/// - `RateLimited { ... }` — the server returned 429 in a way
///   that's the operator's call, not the executor's: either no
///   `Retry-After` (no signal what to do) or a value above the
///   inline-backoff ceiling (too long to block on).
/// - `Failed(...)` — every other error class. Pass-through of
///   [`FetchError`] minus the `RateLimited` variant.
#[derive(Debug)]
pub enum BackoffOutcome {
    /// Session 32: the body and the response Content-Type travel
    /// together. The struct-variant shape (rather than two-tuple)
    /// keeps callers honest about which field is which when
    /// destructuring; the chip-authority work in
    /// `recipe_fetch_attempts` depends on the header reaching the
    /// storage write site without being silently dropped.
    Bytes {
        body: Vec<u8>,
        content_type: Option<String>,
    },
    RateLimited {
        retry_after_seconds: Option<u64>,
    },
    Failed(FetchError),
}

/// Fetch with the Track-D backoff policy. See the module docs for
/// the exact policy.
///
/// `context` is a short label included in log lines so the operator
/// can tell pre-fetch backoff from runtime backoff (`"prefetch"` vs
/// `"runtime"`). It's not load-bearing for behaviour — purely a
/// human-legibility hook.
pub async fn fetch_with_backoff(
    http: &dyn HttpFetcher,
    url: &str,
    context: &str,
) -> BackoffOutcome {
    // Session 32: route through the meta-aware path so the response
    // Content-Type travels with the body. Implementations that don't
    // override `fetch_bytes_with_meta` get the trait's default impl,
    // which calls `fetch_bytes` and returns `content_type: None` —
    // backward-compat is byte-for-byte for those callers.
    match http.fetch_bytes_with_meta(url).await {
        Ok(FetchedBytes { body, content_type }) => BackoffOutcome::Bytes { body, content_type },
        Err(FetchError::RateLimited { retry_after_seconds }) => {
            handle_rate_limit(http, url, context, retry_after_seconds).await
        }
        Err(other) => BackoffOutcome::Failed(other),
    }
}

/// Apply the rate-limit policy: sleep-and-retry for short waits,
/// surface immediately otherwise. Pulled out of `fetch_with_backoff`
/// for testability — the policy is the part that benefits from being
/// independently exercisable.
async fn handle_rate_limit(
    http: &dyn HttpFetcher,
    url: &str,
    context: &str,
    retry_after_seconds: Option<u64>,
) -> BackoffOutcome {
    match retry_after_seconds {
        Some(secs) if secs <= SHORT_BACKOFF_CEILING_SECS => {
            info!(
                context = %context,
                url = %url,
                retry_after_seconds = secs,
                "rate-limited; sleeping for short Retry-After then retrying once"
            );
            tokio::time::sleep(Duration::from_secs(secs)).await;
            // Session 32: the inline retry also goes through
            // `fetch_bytes_with_meta` so a recovered fetch carries
            // the same Content-Type discipline as the first-try path.
            match http.fetch_bytes_with_meta(url).await {
                Ok(FetchedBytes { body, content_type }) => {
                    info!(
                        context = %context,
                        url = %url,
                        retried_bytes = body.len(),
                        "rate-limit retry succeeded"
                    );
                    BackoffOutcome::Bytes { body, content_type }
                }
                Err(FetchError::RateLimited {
                    retry_after_seconds: second_value,
                }) => {
                    // Two 429s in a row — surface and let the
                    // operator decide. Retrying again would just be
                    // noise, exactly the antipattern the handoff
                    // names ("otherwise we keep retrying noise").
                    warn!(
                        context = %context,
                        url = %url,
                        retry_after_seconds = ?second_value,
                        "rate-limit retry also rate-limited; surfacing to operator"
                    );
                    BackoffOutcome::RateLimited {
                        retry_after_seconds: second_value,
                    }
                }
                Err(other) => {
                    warn!(
                        context = %context,
                        url = %url,
                        error = %other,
                        "rate-limit retry failed with non-429 error; surfacing"
                    );
                    BackoffOutcome::Failed(other)
                }
            }
        }
        Some(secs) => {
            warn!(
                context = %context,
                url = %url,
                retry_after_seconds = secs,
                ceiling_seconds = SHORT_BACKOFF_CEILING_SECS,
                "rate-limited; Retry-After exceeds inline-backoff ceiling, surfacing to operator"
            );
            BackoffOutcome::RateLimited {
                retry_after_seconds: Some(secs),
            }
        }
        None => {
            warn!(
                context = %context,
                url = %url,
                "rate-limited with no Retry-After header; surfacing to operator without retry"
            );
            BackoffOutcome::RateLimited {
                retry_after_seconds: None,
            }
        }
    }
}

// ===========================================================================
// Per-host backoff state — Session 45
// ===========================================================================
//
// **Why this exists.** Sessions 23–32's backoff was reactive: the
// executor inspected each individual response's `Retry-After` header
// and decided whether to sleep-and-retry inline or surface to the
// operator. That worked for the per-request decision but left the
// system blind across requests: a host that 429'd the prefetch would
// be hit again immediately by the runtime fetch, and a host with
// pathological timeouts (the Session-40 `industry.gov.au` observation
// — 300s timeouts, possibly a geo-block) would tie up the executor
// for the configured timeout on every recipe.
//
// **What this is.** A per-host adaptation layer that lives underneath
// the existing `fetch_with_backoff` policy. Keyed at runtime on the
// URL's host string; parameters uniform across hosts; state derived
// entirely from observed signals (429, `Retry-After`, timeouts, and
// successes). No `[per_host."<hostname>"]` config table anywhere.
// No `if host == "industry.gov.au"` branch. The host string is a
// **runtime key**, not a static-config knob.
//
// **Where it sits.** [`BackoffFetcher`] is a decorator over any
// [`HttpFetcher`] that records and applies per-host backoff. The boot
// path constructs one [`HostBackoff`] (long-lived, shared across
// fetches) and wraps the production `SecureHttpClient` with
// `BackoffFetcher::new(http, host_backoff.clone())` before handing
// the wrapped fetcher to `ExecutorContext::http`. Tests pass the
// underlying `StaticFetcher` directly (skipping host adaptation) when
// they want to assert on the unadapted signal.
//
// **Policy.** A single uniform exponential schedule:
//
// - Successful request → reset the host's failure counter to 0.
//   `next_allowed_at` is left in the past (no future block).
// - 429 with `Retry-After` → push `next_allowed_at` to
//   `now + retry_after`, increment the failure counter (so a
//   subsequent failure without `Retry-After` doesn't fall back to
//   the smallest backoff). The Retry-After is honored verbatim,
//   even above the 60s ceiling that `fetch_with_backoff`'s inline-
//   retry path declines to sleep on; the per-host state is about
//   *what to do next time*, not *whether to block this caller*.
// - 429 without `Retry-After` or timeout → push `next_allowed_at`
//   to `now + exponential_backoff(failures)`, increment the failure
//   counter. The schedule is `1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s, …`
//   — capped at 60 so a host with sustained failures eventually
//   stabilizes at one request per minute rather than backing off
//   into infinity.
//
// **What about non-rate-limit, non-timeout errors (4xx, 5xx, DNS,
// TLS)?** No state change. The principle is "adapt on signals that
// the host is asking us to slow down." A 404 says "wrong URL," a 500
// says "server bug" — neither implies the host wants fewer requests.
// Widening the trigger set is its own design decision; the failure-
// mode taxonomy ADR 0012 deferred until enough shapes accumulate.
//
// **Why `Mutex<HashMap>` rather than `DashMap` or sharded.** The map
// is touched twice per request (pre-flight read, post-flight write)
// and the workload is bounded by the recipe parallelism the executor
// uses (currently sequential per-recipe; the GUI runs one
// `run_fetch_for_plan` at a time). Lock contention is not a concern
// at this scale; introducing a new dependency for parallel-hash-map
// gymnastics would be premature.
//
// **No persistence.** State lives in memory; restarting the binary
// resets it. Persisted backoff state would mean serializing observed-
// signal history into the DB — a reasonable future optimisation, but
// today's failure modes are short-lived (rate-limit windows,
// transient timeouts) so a fresh start on each binary restart is the
// right default.

/// Per-host adaptive backoff state. Construct once at boot, share via
/// `Arc` across [`BackoffFetcher`] instances.
///
/// The state is keyed on the URL's host string at runtime — see the
/// module-level Session 45 section for the principle (host is a
/// runtime key, not a config knob).
pub struct HostBackoff {
    /// Per-host map. `Mutex` (not `tokio::sync::Mutex`) because every
    /// critical section is a constant-time map lookup + write; the
    /// lock is never held across an `await`.
    state: Mutex<HashMap<String, HostState>>,
}

/// Per-host adaptive state. Private — callers route through
/// [`HostBackoff`]'s methods.
#[derive(Debug)]
struct HostState {
    /// Earliest `Instant` at which a request to this host may fire.
    /// Past values mean "no backoff in effect; request immediately."
    next_allowed_at: Instant,
    /// How many consecutive failures the host has produced since the
    /// last success. Drives the exponential schedule (see
    /// [`exponential_backoff`]).
    consecutive_failures: u32,
}

/// The uniform exponential schedule the per-host adaptation uses
/// when the server gave no `Retry-After` hint (429-without-header or
/// timeout). Capped at 60s so a sustained-failing host stabilises at
/// roughly one request per minute rather than diverging.
///
/// Public (`pub`) for the test module's assertions; the constants
/// are also useful in patch-note math when tuning the cap.
pub const HOST_BACKOFF_BASE_SECS: u64 = 1;
pub const HOST_BACKOFF_MAX_SECS: u64 = 60;

fn exponential_backoff(consecutive_failures: u32) -> Duration {
    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    // 2^(failures-1) * base, capped at MAX. The shift saturates at
    // 63 (u64::BITS - 1) and we cap further at HOST_BACKOFF_MAX_SECS,
    // so even pathological failure counts produce a sane bound.
    let exponent = (consecutive_failures.saturating_sub(1)).min(63) as u32;
    let multiplier: u64 = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let secs = HOST_BACKOFF_BASE_SECS
        .saturating_mul(multiplier)
        .min(HOST_BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

impl HostBackoff {
    /// Build an empty backoff state. Production callers construct one
    /// per binary boot; tests construct one per test.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// How long the caller should wait before issuing a request to
    /// `host`. Returns `Duration::ZERO` if no backoff is in effect.
    /// Pure read; does not mutate state.
    pub fn pre_flight_wait(&self, host: &str) -> Duration {
        // Lock-poison policy: a poisoned lock means a previous
        // critical section panicked. The map invariant is just
        // "next_allowed_at + counter per host" so we recover the
        // inner value and proceed — the alternative (refusing to
        // fetch) would amplify a one-off panic into a sustained
        // outage.
        let map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        match map.get(host) {
            Some(s) => s.next_allowed_at.saturating_duration_since(Instant::now()),
            None => Duration::ZERO,
        }
    }

    /// Record a successful response for `host`. Resets the failure
    /// counter; leaves `next_allowed_at` alone (already in the past
    /// by the time we arrive here, modulo a clock skew the
    /// `saturating_duration_since` call above already absorbs).
    pub fn record_success(&self, host: &str) {
        let mut map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(s) = map.get_mut(host) {
            s.consecutive_failures = 0;
        }
        // Hosts that have only ever succeeded never get a state
        // entry — they're indistinguishable from "no backoff" at
        // pre-flight time.
    }

    /// Record a 429 for `host`. If the server provided `Retry-After`,
    /// honor it verbatim (even above the inline-retry ceiling — this
    /// is per-host policy, not per-call). Otherwise fall through to
    /// the uniform exponential schedule.
    pub fn record_rate_limited(&self, host: &str, retry_after: Option<Duration>) {
        let mut map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        let s = map.entry(host.to_string()).or_insert_with(HostState::initial);
        s.consecutive_failures = s.consecutive_failures.saturating_add(1);
        let wait = retry_after.unwrap_or_else(|| exponential_backoff(s.consecutive_failures));
        s.next_allowed_at = Instant::now() + wait;
    }

    /// Record a timeout for `host`. Same policy as a 429 with no
    /// `Retry-After`: increment the failure counter, push
    /// `next_allowed_at` out by the uniform exponential backoff.
    pub fn record_timeout(&self, host: &str) {
        let mut map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        let s = map.entry(host.to_string()).or_insert_with(HostState::initial);
        s.consecutive_failures = s.consecutive_failures.saturating_add(1);
        let wait = exponential_backoff(s.consecutive_failures);
        s.next_allowed_at = Instant::now() + wait;
    }

    /// The current consecutive-failure counter for `host`. Pure read,
    /// 0 for hosts the state map has never seen. Public for test
    /// assertions; production code does not need it.
    pub fn consecutive_failures(&self, host: &str) -> u32 {
        let map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.get(host).map(|s| s.consecutive_failures).unwrap_or(0)
    }

    /// Enumerate the per-host state. Pure read; one entry per host the
    /// adaptation layer has ever recorded a signal for. Hosts whose
    /// only history is success (zero failures, no future block) are
    /// included with `wait_remaining = Duration::ZERO` and
    /// `consecutive_failures = 0` — the counter resets on success
    /// rather than evicting the row, so the row's existence is itself
    /// a signal that the host has been touched at least once this
    /// session.
    ///
    /// Session 46 — added as a drive-by to unblock the per-host
    /// backoff status surface (handoff piece B). Today no caller
    /// reads it; the API crate's introspection IPC will land on top
    /// of this.
    ///
    /// Returns owned strings rather than borrowed slices because the
    /// caller (a Tauri command marshalling into a Vec<DTO>) needs
    /// 'static-lifetime data and locking the Mutex for borrow
    /// duration would defeat the snapshot's purpose.
    pub fn snapshot(&self) -> Vec<HostBackoffSnapshot> {
        let map = match self.state.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        map.iter()
            .map(|(host, s)| HostBackoffSnapshot {
                host: host.clone(),
                consecutive_failures: s.consecutive_failures,
                wait_remaining: s.next_allowed_at.saturating_duration_since(now),
            })
            .collect()
    }
}

/// One per-host row from [`HostBackoff::snapshot`].
///
/// `wait_remaining` is `Duration::ZERO` when the host is currently
/// unblocked (its `next_allowed_at` is in the past). Callers
/// presenting this to operators should distinguish "zero wait but
/// counter > 0" (the host is recovering — backoff window expired but
/// the failure history is still in effect for the next failure) from
/// "zero wait and counter == 0" (clean state).
#[derive(Debug, Clone)]
pub struct HostBackoffSnapshot {
    pub host: String,
    pub consecutive_failures: u32,
    pub wait_remaining: Duration,
}

impl Default for HostBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl HostState {
    fn initial() -> Self {
        Self {
            next_allowed_at: Instant::now(),
            consecutive_failures: 0,
        }
    }
}

/// Decorator over any [`HttpFetcher`] that applies per-host backoff
/// pre-flight and records observed signals post-flight. See the
/// module-level Session 45 section for the policy.
///
/// The inner fetcher is borrowed (`&dyn HttpFetcher`) so the wrapper
/// can be constructed in the same scope as the underlying
/// `SecureHttpClient`. The state is shared via `Arc` so multiple
/// decorators (e.g. one per `run_fetch_for_plan` invocation) share
/// the same per-host history.
pub struct BackoffFetcher<'a> {
    inner: &'a dyn HttpFetcher,
    host_backoff: Arc<HostBackoff>,
}

impl<'a> BackoffFetcher<'a> {
    pub fn new(inner: &'a dyn HttpFetcher, host_backoff: Arc<HostBackoff>) -> Self {
        Self {
            inner,
            host_backoff,
        }
    }

    /// Pre-flight: parse the URL's host (best-effort) and sleep until
    /// the per-host `next_allowed_at`. Returns the host string for
    /// the post-flight recording step. URL-parse failures yield an
    /// empty host string — adaptation is keyed on hosts we can name,
    /// so a malformed URL skips the layer entirely (the underlying
    /// fetcher will surface the invalid-URL error itself).
    async fn before_request(&self, url: &str) -> String {
        let host = host_of(url);
        if host.is_empty() {
            return host;
        }
        let wait = self.host_backoff.pre_flight_wait(&host);
        if !wait.is_zero() {
            info!(
                host = %host,
                wait_secs = wait.as_secs(),
                "host-backoff pre-flight: sleeping before request"
            );
            tokio::time::sleep(wait).await;
        }
        host
    }

    /// Post-flight: inspect the result and update per-host state.
    /// Generic over the result shape so both `fetch_bytes` and
    /// `fetch_bytes_with_meta` route through the same recording
    /// logic.
    fn after_request<T>(&self, host: &str, result: &Result<T, FetchError>) {
        if host.is_empty() {
            return;
        }
        match result {
            Ok(_) => self.host_backoff.record_success(host),
            Err(FetchError::RateLimited { retry_after_seconds }) => {
                self.host_backoff.record_rate_limited(
                    host,
                    retry_after_seconds.map(Duration::from_secs),
                );
            }
            Err(FetchError::Timeout(_)) => {
                self.host_backoff.record_timeout(host);
            }
            Err(_) => {
                // Other error classes (Http, NoFixture) are not
                // host-adaptation signals — see the module-level
                // Session 45 section for the principle.
            }
        }
    }
}

#[async_trait]
impl<'a> HttpFetcher for BackoffFetcher<'a> {
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let host = self.before_request(url).await;
        let result = self.inner.fetch_bytes(url).await;
        self.after_request(&host, &result);
        result
    }

    async fn fetch_bytes_with_meta(&self, url: &str) -> Result<FetchedBytes, FetchError> {
        let host = self.before_request(url).await;
        let result = self.inner.fetch_bytes_with_meta(url).await;
        self.after_request(&host, &result);
        result
    }
}

/// Best-effort URL → host string for the per-host adaptation key.
/// Returns `""` on parse failure or when the URL has no host (e.g. a
/// `data:` URL). Lowercases for case-insensitive matching across
/// hosts that differ only in case.
///
/// **Visibility:** `pub(crate)` since Session 57 / ADR 0017 Piece B
/// — `fetch_executor.rs` reuses this helper to derive the host
/// passed into `fetch_classes::classify_error` when constructing
/// `PriorAttempt::class`. Single host-extraction definition keeps
/// the per-host backoff key and the proposer's class-override key
/// in lockstep.
pub(crate) fn host_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default()
}

/// Format a `Retry-After` value as a short human string ("2m 30s",
/// "45s"). Used by the executor when it surfaces a `RateLimited`
/// outcome's `message` field. `None` becomes "rate-limited; no
/// Retry-After provided".
///
/// Public so the api-crate's DTO conversion can produce the same
/// string client-side; keeping the formatter on the rust side means
/// the wire shape carries the seconds (machine-friendly) and any
/// presentation layer can reformat. The frontend has its own
/// formatter in `outcomes.ts` for richer styling — this one is the
/// canonical text used in tracing logs and the `Failed`-variant
/// fallback, kept consistent with the frontend's via convention.
pub fn format_retry_after(retry_after_seconds: Option<u64>) -> String {
    match retry_after_seconds {
        None => "rate-limited; no Retry-After provided".to_string(),
        Some(secs) => format!("rate-limited; retry after {}", format_duration(secs)),
    }
}

/// "1h 30m 5s" / "5m 0s" / "45s" — as concise as possible.
fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_fetcher::testing::StaticFetcher;

    // -- format_retry_after / format_duration --------------------------------

    #[test]
    fn format_retry_after_handles_none() {
        assert_eq!(
            format_retry_after(None),
            "rate-limited; no Retry-After provided"
        );
    }

    #[test]
    fn format_duration_seconds_only() {
        assert_eq!(format_duration(45), "45s");
    }

    #[test]
    fn format_duration_minutes_seconds() {
        assert_eq!(format_duration(150), "2m 30s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3725), "1h 2m 5s");
    }

    // -- backoff policy ------------------------------------------------------
    //
    // These tests exercise the pure-policy paths through
    // `fetch_with_backoff`. The inline-retry path uses a real
    // `tokio::time::sleep`; tests use values that fit inside
    // `tokio::test`'s runtime without the noise of `paused()` /
    // `advance()`.

    #[tokio::test]
    async fn happy_path_returns_bytes() {
        let f = StaticFetcher::new().with("https://example.com/x", b"hello");
        let out = fetch_with_backoff(&f, "https://example.com/x", "test").await;
        match out {
            BackoffOutcome::Bytes { body, content_type } => {
                assert_eq!(body, b"hello");
                // No content-type configured on the fetcher → None.
                assert_eq!(content_type, None);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn happy_path_threads_content_type_when_configured() {
        // Session 32: the StaticFetcher's `with_content_type` builder
        // is the test surface for the per-URL Content-Type override.
        // The backoff helper must thread the value through unchanged.
        let f = StaticFetcher::new()
            .with("https://example.com/api.json", b"{\"k\": 1}")
            .with_content_type("https://example.com/api.json", "application/json");
        let out = fetch_with_backoff(&f, "https://example.com/api.json", "test").await;
        match out {
            BackoffOutcome::Bytes { body, content_type } => {
                assert_eq!(body, b"{\"k\": 1}");
                assert_eq!(content_type.as_deref(), Some("application/json"));
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limited_with_no_retry_after_surfaces_immediately() {
        let f = StaticFetcher::new().rate_limited("https://example.com/x", None);
        let out = fetch_with_backoff(&f, "https://example.com/x", "test").await;
        match out {
            BackoffOutcome::RateLimited {
                retry_after_seconds,
            } => assert_eq!(retry_after_seconds, None),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limited_above_ceiling_surfaces_immediately() {
        // 600s is well above the 60s ceiling; we must not sleep.
        let f =
            StaticFetcher::new().rate_limited("https://example.com/x", Some(600));
        let started = std::time::Instant::now();
        let out = fetch_with_backoff(&f, "https://example.com/x", "test").await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "should have surfaced immediately, took {elapsed:?}"
        );
        match out {
            BackoffOutcome::RateLimited {
                retry_after_seconds,
            } => assert_eq!(retry_after_seconds, Some(600)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_fixture_returns_failed_with_underlying_error() {
        let f = StaticFetcher::new();
        let out = fetch_with_backoff(&f, "https://example.com/missing", "test").await;
        match out {
            BackoffOutcome::Failed(FetchError::NoFixture(url)) => {
                assert!(url.contains("missing"))
            }
            other => panic!("expected Failed(NoFixture), got {other:?}"),
        }
    }

    // -- Session 45: per-host backoff state ---------------------------------
    //
    // These tests exercise [`HostBackoff`]'s recording and pre-flight
    // policy directly — no decorator, no real I/O. The point is to pin
    // the policy in a way that can't drift: the schedule is uniform,
    // the host string is a runtime key, and successful requests reset
    // the failure counter. Decorator-level integration tests follow
    // (under "BackoffFetcher decorator").
    //
    // Time arithmetic uses `Instant::now()` and tolerates the jitter
    // a real clock introduces between record-and-read by asserting on
    // ranges (`<= expected + slop`) rather than equality.

    #[test]
    fn host_backoff_starts_with_no_wait_for_unknown_host_session_45() {
        let hb = HostBackoff::new();
        assert_eq!(
            hb.pre_flight_wait("example.com"),
            Duration::ZERO,
            "fresh state must not block any host"
        );
        assert_eq!(hb.consecutive_failures("example.com"), 0);
    }

    #[test]
    fn host_backoff_429_with_retry_after_pushes_next_allowed_session_45() {
        let hb = HostBackoff::new();
        hb.record_rate_limited("example.com", Some(Duration::from_secs(15)));

        // Wait should be near 15s. Allow up to 1s of slop for clock
        // jitter between the record-and-read.
        let wait = hb.pre_flight_wait("example.com");
        assert!(
            wait <= Duration::from_secs(15) && wait >= Duration::from_secs(14),
            "wait should be ~15s (got {wait:?})"
        );
        assert_eq!(hb.consecutive_failures("example.com"), 1);
    }

    #[test]
    fn host_backoff_429_without_retry_after_uses_exponential_schedule_session_45() {
        let hb = HostBackoff::new();

        // First failure: ~1s.
        hb.record_rate_limited("api.example.com", None);
        let wait_1 = hb.pre_flight_wait("api.example.com");
        assert!(
            wait_1 <= Duration::from_secs(1) && wait_1 >= Duration::from_millis(500),
            "first failure should yield ~1s wait (got {wait_1:?})"
        );

        // Second failure: ~2s.
        hb.record_rate_limited("api.example.com", None);
        let wait_2 = hb.pre_flight_wait("api.example.com");
        assert!(
            wait_2 <= Duration::from_secs(2) && wait_2 >= Duration::from_millis(1500),
            "second failure should yield ~2s wait (got {wait_2:?})"
        );

        // Third failure: ~4s.
        hb.record_rate_limited("api.example.com", None);
        let wait_3 = hb.pre_flight_wait("api.example.com");
        assert!(
            wait_3 <= Duration::from_secs(4) && wait_3 >= Duration::from_millis(3500),
            "third failure should yield ~4s wait (got {wait_3:?})"
        );

        assert_eq!(hb.consecutive_failures("api.example.com"), 3);
    }

    #[test]
    fn host_backoff_timeout_uses_same_schedule_as_429_no_header_session_45() {
        // The principle: timeouts and unhinted 429s are
        // indistinguishable host-adaptation signals. Both crank the
        // same exponential schedule.
        let hb = HostBackoff::new();
        hb.record_timeout("slow.example.com");
        let after_one_timeout = hb.pre_flight_wait("slow.example.com");

        let hb2 = HostBackoff::new();
        hb2.record_rate_limited("slow.example.com", None);
        let after_one_429 = hb2.pre_flight_wait("slow.example.com");

        // Both should be ~1s; tolerate sub-second drift between the
        // two calls. The point is they're in the same band, not byte
        // -equal — the absolute value depends on `Instant::now()`.
        let drift = after_one_timeout
            .checked_sub(after_one_429)
            .or_else(|| after_one_429.checked_sub(after_one_timeout))
            .unwrap_or(Duration::ZERO);
        assert!(
            drift < Duration::from_millis(100),
            "timeout and 429-no-header should produce the same schedule \
             (timeout: {after_one_timeout:?}, 429: {after_one_429:?})"
        );
    }

    #[test]
    fn host_backoff_success_resets_failure_counter_session_45() {
        let hb = HostBackoff::new();
        hb.record_rate_limited("example.com", None);
        hb.record_rate_limited("example.com", None);
        assert_eq!(hb.consecutive_failures("example.com"), 2);

        hb.record_success("example.com");
        assert_eq!(
            hb.consecutive_failures("example.com"),
            0,
            "success must reset the failure counter"
        );

        // A new failure after success should restart at the bottom of
        // the schedule, not pick up where we left off.
        hb.record_rate_limited("example.com", None);
        let wait = hb.pre_flight_wait("example.com");
        assert!(
            wait <= Duration::from_secs(1) && wait >= Duration::from_millis(500),
            "post-success failure must restart at ~1s (got {wait:?})"
        );
    }

    #[test]
    fn host_backoff_state_is_keyed_per_host_not_global_session_45() {
        let hb = HostBackoff::new();
        hb.record_rate_limited("rate-limited.example.com", Some(Duration::from_secs(30)));

        // A different host must remain unblocked. The principle in
        // the handoff: host string is a runtime key; backoff is
        // per-host, not global.
        assert_eq!(
            hb.pre_flight_wait("other.example.com"),
            Duration::ZERO,
            "unrelated host must not inherit a peer's backoff"
        );
        assert_eq!(hb.consecutive_failures("other.example.com"), 0);

        // The originating host stays blocked.
        let wait = hb.pre_flight_wait("rate-limited.example.com");
        assert!(
            wait > Duration::from_secs(28),
            "originating host should still be blocked (got {wait:?})"
        );
    }

    #[test]
    fn host_backoff_caps_at_max_secs_session_45() {
        // Many failures must converge to HOST_BACKOFF_MAX_SECS rather
        // than diverge. Without the cap, a host with sustained
        // failures would wait >1 hour after only a handful of
        // failures (2^16 = 65k seconds at failure 17).
        let hb = HostBackoff::new();
        for _ in 0..20 {
            hb.record_rate_limited("perma-bad.example.com", None);
        }
        let wait = hb.pre_flight_wait("perma-bad.example.com");
        assert!(
            wait <= Duration::from_secs(HOST_BACKOFF_MAX_SECS),
            "wait must cap at HOST_BACKOFF_MAX_SECS={HOST_BACKOFF_MAX_SECS} (got {wait:?})"
        );
        assert!(
            wait >= Duration::from_secs(HOST_BACKOFF_MAX_SECS - 2),
            "wait should still be in the cap band (got {wait:?})"
        );
    }

    // -- Session 46: snapshot accessor --------------------------------------

    #[test]
    fn host_backoff_snapshot_is_empty_on_fresh_state_session_46() {
        let hb = HostBackoff::new();
        let snap = hb.snapshot();
        assert!(snap.is_empty(), "fresh state has no snapshot rows");
    }

    #[test]
    fn host_backoff_snapshot_reports_recorded_failures_session_46() {
        let hb = HostBackoff::new();
        hb.record_rate_limited("a.example.com", Some(Duration::from_secs(15)));
        hb.record_timeout("b.example.com");

        let snap = hb.snapshot();
        assert_eq!(snap.len(), 2);

        let a = snap.iter().find(|r| r.host == "a.example.com").unwrap();
        let b = snap.iter().find(|r| r.host == "b.example.com").unwrap();

        assert_eq!(a.consecutive_failures, 1);
        // Honored Retry-After: ~15s remaining (allow slop).
        assert!(
            a.wait_remaining <= Duration::from_secs(15)
                && a.wait_remaining >= Duration::from_secs(14),
            "a wait should be ~15s, got {:?}",
            a.wait_remaining
        );

        assert_eq!(b.consecutive_failures, 1);
        // Timeout schedule: ~1s.
        assert!(
            b.wait_remaining <= Duration::from_secs(1),
            "b wait should be ~1s, got {:?}",
            b.wait_remaining
        );
    }

    #[test]
    fn host_backoff_snapshot_keeps_row_after_success_with_zero_counter_session_46() {
        // Counter resets on success but the row is preserved so the
        // operator can see "this host had failures earlier and is now
        // recovered." Pinning the behaviour explicitly here.
        //
        // We seed with `Some(Duration::ZERO)` rather than `None` so
        // `next_allowed_at` is set to ~now (not now + 1s from the
        // exponential schedule), keeping `wait_remaining` testable
        // without time-virtualisation.
        let hb = HostBackoff::new();
        hb.record_rate_limited("recovered.example.com", Some(Duration::ZERO));
        hb.record_success("recovered.example.com");

        let snap = hb.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].host, "recovered.example.com");
        assert_eq!(snap[0].consecutive_failures, 0);
        assert_eq!(snap[0].wait_remaining, Duration::ZERO);
    }

    #[test]
    fn host_backoff_lowercases_host_string_for_dedup_session_45() {
        // The principle: hosts that differ only in case share state.
        // `Example.COM` and `example.com` resolve to the same DNS
        // entry; treating them as separate would let a misbehaving
        // server escape backoff by URL casing variance. The
        // lowercasing happens in `host_of`; this test exercises it
        // through the real key path.
        assert_eq!(host_of("https://Example.COM/path"), "example.com");
        assert_eq!(host_of("https://EXAMPLE.com:8443/q"), "example.com");
    }

    #[test]
    fn host_of_returns_empty_for_unparseable_url_session_45() {
        // A malformed URL skips host adaptation entirely (the under-
        // lying fetcher will surface the invalid-URL error itself).
        // Pinning this prevents a future "let's fall back to the
        // path" change that would key state on something that isn't
        // a host.
        assert_eq!(host_of("not a url"), "");
        // Schemes without a host (e.g. `data:`) also yield empty.
        assert_eq!(host_of("data:text/plain,hello"), "");
    }

    // -- Session 45: BackoffFetcher decorator -------------------------------
    //
    // Integration tests over the decorator + state combo. The
    // `StaticFetcher` provides the synthetic responses; the decorator
    // applies its pre-flight wait (which is a no-op on first contact
    // for a host) and records the post-flight signal.

    #[tokio::test]
    async fn backoff_fetcher_passes_through_successful_fetch_session_45() {
        let inner = StaticFetcher::new().with("https://example.com/x", b"hello");
        let backoff = Arc::new(HostBackoff::new());
        let f = BackoffFetcher::new(&inner, backoff.clone());

        let bytes = f.fetch_bytes("https://example.com/x").await.unwrap();
        assert_eq!(bytes, b"hello");
        // No failure was recorded; counter stays 0.
        assert_eq!(backoff.consecutive_failures("example.com"), 0);
        assert_eq!(backoff.pre_flight_wait("example.com"), Duration::ZERO);
    }

    #[tokio::test]
    async fn backoff_fetcher_records_429_into_host_state_session_45() {
        let inner =
            StaticFetcher::new().rate_limited("https://throttled.example.com/x", Some(20));
        let backoff = Arc::new(HostBackoff::new());
        let f = BackoffFetcher::new(&inner, backoff.clone());

        let result = f.fetch_bytes("https://throttled.example.com/x").await;
        assert!(matches!(result, Err(FetchError::RateLimited { .. })));

        assert_eq!(
            backoff.consecutive_failures("throttled.example.com"),
            1,
            "decorator must record 429 as a per-host failure"
        );
        let wait = backoff.pre_flight_wait("throttled.example.com");
        assert!(
            wait > Duration::from_secs(18),
            "decorator must honor server-supplied Retry-After (got {wait:?})"
        );
    }

    #[tokio::test]
    async fn backoff_fetcher_records_timeout_into_host_state_session_45() {
        let inner = StaticFetcher::new()
            .timeout("https://slow.example.com/x", Duration::from_secs(5));
        let backoff = Arc::new(HostBackoff::new());
        let f = BackoffFetcher::new(&inner, backoff.clone());

        let result = f.fetch_bytes("https://slow.example.com/x").await;
        assert!(matches!(result, Err(FetchError::Timeout(_))));

        assert_eq!(
            backoff.consecutive_failures("slow.example.com"),
            1,
            "decorator must record Timeout as a per-host failure"
        );
        let wait = backoff.pre_flight_wait("slow.example.com");
        assert!(
            wait <= Duration::from_secs(1) && wait >= Duration::from_millis(500),
            "first timeout should yield ~1s schedule wait (got {wait:?})"
        );
    }

    // Note (Session 45): there is no end-to-end "decorator
    // round-trip resets a prior failure" test here. Such a test
    // would need to either (a) sleep through the decorator's
    // pre-flight wait between the failing and successful fetch
    // (1s of real wall-clock cost), or (b) virtualise time via
    // tokio's `start_paused`, which requires the `test-util`
    // feature on the workspace's `tokio` dep that we don't
    // otherwise need. The two halves are pinned separately at no
    // cost: the policy layer's `record_success` is exercised by
    // `host_backoff_success_resets_failure_counter_session_45`,
    // and the decorator's success-path success-recording is
    // exercised implicitly by
    // `backoff_fetcher_passes_through_successful_fetch_session_45`
    // (no failure recorded → counter stays at 0).

    #[tokio::test]
    async fn backoff_fetcher_does_not_block_unknown_host_session_45() {
        // First contact for a host has empty state → no pre-flight
        // sleep. Verifies the common case (cold start) doesn't pay
        // the backoff layer's cost.
        let inner = StaticFetcher::new().with("https://fresh.example.com/x", b"hi");
        let backoff = Arc::new(HostBackoff::new());
        let f = BackoffFetcher::new(&inner, backoff);

        let started = Instant::now();
        let bytes = f.fetch_bytes("https://fresh.example.com/x").await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(bytes, b"hi");
        assert!(
            elapsed < Duration::from_millis(100),
            "first-contact fetch must not block on backoff (took {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn backoff_fetcher_threads_content_type_through_decorator_session_45() {
        // The decorator must not silently strip the response
        // Content-Type that `fetch_bytes_with_meta` carries —
        // Session-32 work (apply-failure capture) depends on it.
        let inner = StaticFetcher::new()
            .with("https://example.com/api.json", b"{\"k\":1}")
            .with_content_type("https://example.com/api.json", "application/json");
        let backoff = Arc::new(HostBackoff::new());
        let f = BackoffFetcher::new(&inner, backoff);

        let meta = f
            .fetch_bytes_with_meta("https://example.com/api.json")
            .await
            .unwrap();
        assert_eq!(meta.body, b"{\"k\":1}");
        assert_eq!(meta.content_type.as_deref(), Some("application/json"));
    }

    // -- Session 45: exponential_backoff schedule ---------------------------

    #[test]
    fn exponential_backoff_zero_failures_is_zero_session_45() {
        assert_eq!(exponential_backoff(0), Duration::ZERO);
    }

    #[test]
    fn exponential_backoff_schedule_doubles_session_45() {
        assert_eq!(exponential_backoff(1), Duration::from_secs(1));
        assert_eq!(exponential_backoff(2), Duration::from_secs(2));
        assert_eq!(exponential_backoff(3), Duration::from_secs(4));
        assert_eq!(exponential_backoff(4), Duration::from_secs(8));
        assert_eq!(exponential_backoff(5), Duration::from_secs(16));
        assert_eq!(exponential_backoff(6), Duration::from_secs(32));
        // Cap kicks in at 7 (would be 64s otherwise).
        assert_eq!(
            exponential_backoff(7),
            Duration::from_secs(HOST_BACKOFF_MAX_SECS)
        );
        assert_eq!(
            exponential_backoff(50),
            Duration::from_secs(HOST_BACKOFF_MAX_SECS),
            "large failure counts must cap, not overflow"
        );
    }
}
