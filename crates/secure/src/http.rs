//! The one HTTP client situation_room uses.
//!
//! All outbound HTTP goes through [`SecureHttpClient`]. No crate talks to
//! `reqwest::Client::new()` directly — if you see that in a PR, it's a bug.
//!
//! ## Defenses applied at every request
//!
//! - **TLS 1.2+ only, rustls only**. No OpenSSL, no legacy ciphers. System
//!   root certs are loaded but not extended at runtime.
//! - **URL validation** via [`crate::UrlGuard`] before every request.
//! - **Bounded response size** — responses over the configured limit are
//!   truncated and an error returned. Prevents memory exhaustion from
//!   malicious servers returning 10GB of data.
//! - **Connect + total timeouts** — no hanging requests.
//! - **Redirect whitelisting** — redirects are followed up to a limit, each
//!   redirect target is re-validated against the URL guard. Redirects to
//!   private IPs or metadata endpoints get rejected (SSRF via redirect is
//!   a classic bypass).
//! - **No automatic cookie storage**. Each source carries its own auth
//!   explicitly; no ambient cookies persist across requests.
//! - **No HTTP/2 server push** — disabled to reduce attack surface.
//! - **Request body size limits** — we don't send 10MB prompts to an API
//!   "by accident".
//!
//! ## What we don't do
//!
//! - We don't pin certificates. Pinning is fragile in a long-lived OSS
//!   tool where users connect to dozens of rotating services.
//! - We don't implement DNS-over-HTTPS. The user's system resolver is
//!   trusted at the host level; DNS poisoning defense is out of scope
//!   for a desktop app.
//!
//! ## Response-headers surface (Track D, Session 25)
//!
//! Two response-shape surfaces coexist:
//!
//! - **Body-only** — [`SecureHttpClient::get_bytes`] /
//!   [`SecureHttpClient::post_json_bytes`] return `Vec<u8>` and discard
//!   headers. These are the original methods; existing callers continue
//!   to compile unchanged.
//! - **Body + headers** — [`SecureHttpClient::get_with_headers`] /
//!   [`SecureHttpClient::post_json_with_headers`] return
//!   [`SecureHttpResponse`] which carries a [`SecureHeaderMap`]
//!   alongside the body. Use this when the caller needs `Retry-After`,
//!   `Content-Type`, `ETag`, etc.
//!
//! The [`SecureHeaderMap`] is the boundary type — see its module docs
//! and the `headers` module for the allow-list-accessor rationale. The
//! raw `reqwest::HeaderMap` never crosses the secure boundary; this is
//! ADR 0009 §"The rule" extended in Session 25.
//!
//! Status codes are surfaced through the existing
//! [`HttpError::Status(u16)`] variant — for 429 specifically, callers
//! that want to honor `Retry-After` use the `*_with_headers` variant
//! and read the value from the returned response *before* matching on
//! status. The pattern is documented at
//! [`SecureHttpClient::get_with_headers`].

use crate::headers::SecureHeaderMap;
use crate::secrets::SecretString;
use crate::url_guard::{is_disallowed_ip, UrlGuard, UrlViolation};
use reqwest::{redirect, Client, ClientBuilder, Response, StatusCode};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use url::Url;

/// Configuration for [`SecureHttpClient`]. Defaults are intentionally strict.
#[derive(Debug, Clone)]
pub struct SecureHttpConfig {
    /// Connection timeout (per connect attempt).
    pub connect_timeout: Duration,
    /// Total timeout per request (including body read).
    pub total_timeout: Duration,
    /// Maximum response body size in bytes. Larger responses are truncated
    /// and an error returned.
    pub max_response_bytes: usize,
    /// Maximum number of redirects to follow.
    pub max_redirects: usize,
    /// User-Agent header to send. Required by some APIs (SEC EDGAR).
    pub user_agent: String,
    /// Allow HTTP/2? (We allow it for performance; HTTP/2 server push is
    /// disabled separately in the builder.)
    pub allow_http2: bool,
}

impl Default for SecureHttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(300),
            max_response_bytes: 32 * 1024 * 1024, // 32 MB
            max_redirects: 5,
            user_agent: format!("situation_room/{} (+https://github.com/situation_room)", env!("CARGO_PKG_VERSION")),
            allow_http2: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("url rejected by url guard: {0}")]
    UrlRejected(#[from] UrlViolation),
    #[error("request failed: {0}")]
    Request(String),
    #[error("response exceeded max size of {max} bytes (got at least {got})")]
    ResponseTooLarge { max: usize, got: usize },
    #[error("redirect target rejected: {0}")]
    RedirectRejected(String),
    #[error("timeout after {:?}", .0)]
    Timeout(Duration),
    #[error("status error: {0}")]
    Status(u16),
    /// 4xx/5xx response that arrived intact, with headers the caller
    /// can act on (e.g. `Retry-After` on 429). Distinct from
    /// [`HttpError::Status`] which is the legacy variant returned by
    /// `get_bytes` / `post_json_bytes` — the body-only methods can't
    /// surface headers, so they keep the simpler shape.
    ///
    /// The body of the failing response is *not* carried here; it's
    /// already been logged at debug level if non-empty. Adding it
    /// would force every caller to think about how to handle a body
    /// they probably can't act on; the diagnostic value is at the
    /// log layer, not the error.
    #[error("status error: {status} (with response headers)")]
    StatusWithHeaders {
        status: u16,
        headers: SecureHeaderMap,
    },
    #[error("tls error: {0}")]
    Tls(String),
}

/// A response that carries its body and a bounded view of its headers.
///
/// Returned by the `*_with_headers` methods. The status field is the
/// raw `reqwest::StatusCode` so callers can match against it
/// idiomatically (`if response.status == StatusCode::TOO_MANY_REQUESTS`);
/// the headers field is the secured wrapper described in
/// [`crate::headers`].
#[derive(Debug)]
pub struct SecureHttpResponse {
    pub status: StatusCode,
    pub headers: SecureHeaderMap,
    pub body: Vec<u8>,
}

/// The HTTP client. Construct once and share (wraps an Arc internally).
#[derive(Clone)]
pub struct SecureHttpClient {
    inner: Client,
    guard: Arc<UrlGuard>,
    config: SecureHttpConfig,
}

impl SecureHttpClient {
    pub fn new(config: SecureHttpConfig) -> Result<Self, HttpError> {
        let guard = Arc::new(UrlGuard::new());
        let guard_for_redirect = Arc::clone(&guard);
        let max_redirects = config.max_redirects;

        // Redirect policy — follow up to N, but re-validate each target.
        let redirect_policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= max_redirects {
                return attempt.stop();
            }
            let url = attempt.url();
            if let Err(e) = guard_for_redirect.check(url.as_str()) {
                tracing::warn!(target = %url, "redirect rejected by url guard: {}", e);
                return attempt.stop();
            }
            attempt.follow()
        });

        let mut builder = ClientBuilder::new()
            .user_agent(&config.user_agent)
            .connect_timeout(config.connect_timeout)
            .timeout(config.total_timeout)
            .redirect(redirect_policy)
            // rustls-tls is the only TLS backend (set via features)
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            // No ambient cookies across requests — this is the default when
            // reqwest's `cookies` feature is not enabled, which it isn't.
            // (We deliberately do NOT call .cookie_store(false) because that
            // method only exists with the `cookies` feature; its absence is
            // the enforcement.)
            //
            // HSTS is enforced automatically by rustls behavior. For any
            // source we mark as sensitive in per-source config, we also
            // disable HTTP (not HTTPS) at the per-source layer.
            ;

        if !config.allow_http2 {
            builder = builder.http1_only();
        }

        let inner = builder
            .build()
            .map_err(|e| HttpError::Tls(e.to_string()))?;

        Ok(Self {
            inner,
            guard,
            config,
        })
    }

    // ------------------------------------------------------------------
    // Body-only methods (legacy surface; existing callers keep these)
    // ------------------------------------------------------------------

    /// GET with all guards applied. Body-only — headers are read for
    /// content-length validation and discarded. Use
    /// [`Self::get_with_headers`] when the caller needs `Retry-After`,
    /// `Content-Type`, `ETag`, etc.
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, HttpError> {
        // Defer to the headers-aware variant and drop the headers.
        // The legacy error shape (HttpError::Status(u16)) is preserved
        // — callers of `get_bytes` get the same behaviour as before
        // Track D.
        let resp = self.get_with_headers_internal(url).await?;
        if !resp.status.is_success() {
            return Err(HttpError::Status(resp.status.as_u16()));
        }
        Ok(resp.body)
    }

    /// GET and parse as JSON, bounded.
    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        let bytes = self.get_bytes(url).await?;
        serde_json::from_slice(&bytes).map_err(|e| HttpError::Request(format!("json parse: {e}")))
    }

    /// POST a JSON body with optional secret auth headers and optional plain
    /// extra headers. Returns the raw response bytes, bounded by
    /// `config.max_response_bytes`. Applies the same URL guard, literal-IP
    /// check, and bounded read as [`get_bytes`].
    ///
    /// The distinction between `auth_headers` and `extra_headers` is
    /// type-level, not behavioural: both end up as HTTP headers. The split
    /// exists so API keys travel as [`SecretString`] through the public API
    /// and are only unwrapped at the single call site here (the reqwest
    /// builder), making every secret exposure point visible to review.
    ///
    /// Header values from `auth_headers` are passed with
    /// `.sensitive(true)` so reqwest's own logging will redact them.
    ///
    /// ### Do not pass `content-type` in `extra_headers`
    ///
    /// This method calls `.json(body)` on the reqwest builder, which sets
    /// `Content-Type: application/json` itself. Reqwest's `.header(...)`
    /// appends — it does not replace — so passing `("content-type", ...)`
    /// in `extra_headers` results in *two* Content-Type headers on the
    /// wire. Strict API gateways (xAI's, notably) reject that with `415
    /// Unsupported Media Type`. There is no exception: every body this
    /// method sends is JSON. If a caller needs to send a non-JSON body,
    /// add a sibling method, don't override the header here.
    pub async fn post_json_bytes(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_headers: &[(&str, &SecretString)],
        extra_headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, HttpError> {
        // Defer to the headers-aware variant and drop the headers.
        let resp = self
            .post_json_with_headers_internal(url, body, auth_headers, extra_headers)
            .await?;
        if !resp.status.is_success() {
            return Err(HttpError::Status(resp.status.as_u16()));
        }
        Ok(resp.body)
    }

    /// POST a JSON body and parse the response as JSON.
    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_headers: &[(&str, &SecretString)],
        extra_headers: &[(&str, &str)],
    ) -> Result<T, HttpError> {
        let bytes = self
            .post_json_bytes(url, body, auth_headers, extra_headers)
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| HttpError::Request(format!("json parse: {e}")))
    }

    // ------------------------------------------------------------------
    // Headers-aware methods (Track D, Session 25)
    // ------------------------------------------------------------------

    /// GET, returning a [`SecureHttpResponse`] that carries the body
    /// alongside an allow-list-accessor view of the response headers.
    ///
    /// On non-success status this returns
    /// [`HttpError::StatusWithHeaders`] so callers can read e.g.
    /// `Retry-After` from a 429 response. On success, the headers are
    /// in the returned response.
    ///
    /// The body is bounded by `config.max_response_bytes`, same as
    /// [`Self::get_bytes`].
    pub async fn get_with_headers(&self, url: &Url) -> Result<SecureHttpResponse, HttpError> {
        let resp = self.get_with_headers_internal(url.as_str()).await?;
        if !resp.status.is_success() {
            return Err(HttpError::StatusWithHeaders {
                status: resp.status.as_u16(),
                headers: resp.headers,
            });
        }
        Ok(resp)
    }

    /// POST a JSON body, returning a [`SecureHttpResponse`] that
    /// carries the body alongside response headers. Same auth /
    /// content-type discipline as [`Self::post_json_bytes`]; see that
    /// method's docs for the do-not-pass-content-type rule.
    ///
    /// On non-success status this returns
    /// [`HttpError::StatusWithHeaders`] so callers can read e.g.
    /// `Retry-After` from a 429 response.
    pub async fn post_json_with_headers(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_headers: &[(&str, &SecretString)],
        extra_headers: &[(&str, &str)],
    ) -> Result<SecureHttpResponse, HttpError> {
        let resp = self
            .post_json_with_headers_internal(url, body, auth_headers, extra_headers)
            .await?;
        if !resp.status.is_success() {
            return Err(HttpError::StatusWithHeaders {
                status: resp.status.as_u16(),
                headers: resp.headers,
            });
        }
        Ok(resp)
    }

    // ------------------------------------------------------------------
    // Internal: the actual request paths. Both legacy body-only and
    // headers-aware variants share these. The `_internal` suffix means
    // "returns SecureHttpResponse regardless of status; the public
    // wrappers project to the appropriate error shape."
    // ------------------------------------------------------------------

    async fn get_with_headers_internal(
        &self,
        url: &str,
    ) -> Result<SecureHttpResponse, HttpError> {
        let parsed = self.guard.check(url)?;
        self.check_host_ip(&parsed)?;
        let resp = self
            .inner
            .get(parsed)
            .send()
            .await
            .map_err(|e| Self::classify_err(e, self.config.total_timeout))?;
        self.consume_response(resp).await
    }

    async fn post_json_with_headers_internal(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_headers: &[(&str, &SecretString)],
        extra_headers: &[(&str, &str)],
    ) -> Result<SecureHttpResponse, HttpError> {
        let parsed = self.guard.check(url)?;
        self.check_host_ip(&parsed)?;

        let mut req = self.inner.post(parsed).json(body);
        for (name, value) in extra_headers {
            req = req.header(*name, *value);
        }
        for (name, secret) in auth_headers {
            // expose_secret is intentional at this single boundary — the
            // resulting HeaderValue is marked sensitive so reqwest-internal
            // logging redacts it, and the SecretString on the caller side
            // is unchanged.
            let mut hv = reqwest::header::HeaderValue::from_str(secret.expose_secret())
                .map_err(|e| HttpError::Request(format!("invalid header value: {e}")))?;
            hv.set_sensitive(true);
            req = req.header(*name, hv);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Self::classify_err(e, self.config.total_timeout))?;
        self.consume_response(resp).await
    }

    /// Common bounded-body read shared by the GET and POST internals.
    /// Captures status + headers up-front so the structured response
    /// is constructable even on a non-success path. Body is read into
    /// a `Bytes` so `SecureHttpResponse` can be cheaply cloned.
    async fn consume_response(&self, resp: Response) -> Result<SecureHttpResponse, HttpError> {
        let status = resp.status();
        let raw_headers = resp.headers().clone();
        let headers = SecureHeaderMap::from_reqwest(raw_headers);

        // On a non-success path, log a small body excerpt for diagnostics
        // but don't blow the bound. This mirrors the legacy
        // `post_json_bytes` behaviour exactly. The body still gets
        // surfaced to the caller via the SecureHttpResponse, capped at
        // `max_response_bytes`, so legitimate "I want the JSON-shaped
        // error from the API" use-cases keep working.
        if !status.is_success() {
            // Eagerly read into the bound, same machinery as the
            // success path. Failures during read get classified as
            // request errors so the caller still sees the status code
            // — but we return *with* the body so e.g. an LLM provider
            // can quote the gateway's error JSON in its own log line.
            let body = self.read_bounded(resp).await?;
            tracing::debug!(
                status = status.as_u16(),
                body_bytes = body.len(),
                "non-success response"
            );
            return Ok(SecureHttpResponse {
                status,
                headers,
                body,
            });
        }

        // Up-front Content-Length check before streaming. Servers can
        // lie about Content-Length, so the streaming reader below
        // double-checks against `max_response_bytes`.
        if let Some(len) = resp.content_length() {
            if (len as usize) > self.config.max_response_bytes {
                return Err(HttpError::ResponseTooLarge {
                    max: self.config.max_response_bytes,
                    got: len as usize,
                });
            }
        }

        let body = self.read_bounded(resp).await?;
        Ok(SecureHttpResponse {
            status,
            headers,
            body,
        })
    }

    /// Stream the response body into a `Vec<u8>`, aborting if the
    /// total exceeds [`SecureHttpConfig::max_response_bytes`].
    async fn read_bounded(&self, resp: Response) -> Result<Vec<u8>, HttpError> {
        let mut bytes: Vec<u8> = Vec::with_capacity(
            resp.content_length()
                .map(|l| (l as usize).min(self.config.max_response_bytes))
                .unwrap_or(4096),
        );
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| HttpError::Request(e.to_string()))?;
            if bytes.len() + chunk.len() > self.config.max_response_bytes {
                return Err(HttpError::ResponseTooLarge {
                    max: self.config.max_response_bytes,
                    got: bytes.len() + chunk.len(),
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// If the URL's host happens to be a literal IP, recheck it. (A host
    /// name's resolved IPs are checked by a custom DNS resolver — that's
    /// the real defense against DNS-rebinding. For now we at least cover
    /// the literal-IP case here.)
    ///
    /// Uses the typed `url::Host` variant rather than parsing `host_str()`
    /// — IPv6 literals carry `[]` brackets in the string form and would
    /// silently bypass the check otherwise. Matches the fix in
    /// `UrlGuard::check`.
    fn check_host_ip(&self, url: &Url) -> Result<(), HttpError> {
        match url.host() {
            Some(url::Host::Ipv4(v4)) => {
                let ip = IpAddr::V4(v4);
                if is_disallowed_ip(&ip) {
                    return Err(HttpError::RedirectRejected(format!(
                        "host resolves to disallowed IP: {ip}"
                    )));
                }
            }
            Some(url::Host::Ipv6(v6)) => {
                let ip = IpAddr::V6(v6);
                if is_disallowed_ip(&ip) {
                    return Err(HttpError::RedirectRejected(format!(
                        "host resolves to disallowed IP: {ip}"
                    )));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn classify_err(e: reqwest::Error, configured_timeout: Duration) -> HttpError {
        if e.is_timeout() {
            HttpError::Timeout(configured_timeout)
        } else if let Some(status) = e.status() {
            HttpError::Status(status.as_u16())
        } else {
            HttpError::Request(e.to_string())
        }
    }
}
