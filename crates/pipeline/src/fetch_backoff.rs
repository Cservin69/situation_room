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

use std::time::Duration;
use tracing::{info, warn};

use crate::http_fetcher::{FetchError, HttpFetcher};

/// Maximum `Retry-After` value (in seconds) for which we sleep
/// inline and retry. Larger values surface to the operator without
/// the executor blocking.
const SHORT_BACKOFF_CEILING_SECS: u64 = 60;

/// Result of a backoff-aware fetch. Three shapes:
///
/// - `Bytes(...)` — the body, as if we'd never hit a rate limit
///   (either because we weren't, or because the inline retry
///   succeeded).
/// - `RateLimited { ... }` — the server returned 429 in a way
///   that's the operator's call, not the executor's: either no
///   `Retry-After` (no signal what to do) or a value above the
///   inline-backoff ceiling (too long to block on).
/// - `Failed(...)` — every other error class. Pass-through of
///   [`FetchError`] minus the `RateLimited` variant.
#[derive(Debug)]
pub enum BackoffOutcome {
    Bytes(Vec<u8>),
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
    match http.fetch_bytes(url).await {
        Ok(bytes) => BackoffOutcome::Bytes(bytes),
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
            match http.fetch_bytes(url).await {
                Ok(bytes) => {
                    info!(
                        context = %context,
                        url = %url,
                        retried_bytes = bytes.len(),
                        "rate-limit retry succeeded"
                    );
                    BackoffOutcome::Bytes(bytes)
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
            BackoffOutcome::Bytes(b) => assert_eq!(b, b"hello"),
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
}
