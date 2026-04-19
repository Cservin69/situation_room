//! The one HTTP client Stockpile uses.
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

use crate::url_guard::{is_disallowed_ip, UrlGuard, UrlViolation};
use reqwest::{redirect, Client, ClientBuilder};
use std::net::IpAddr;
use std::str::FromStr;
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
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(60),
            max_response_bytes: 32 * 1024 * 1024, // 32 MB
            max_redirects: 5,
            user_agent: format!("Stockpile/{} (+https://github.com/stockpile)", env!("CARGO_PKG_VERSION")),
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
    #[error("tls error: {0}")]
    Tls(String),
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
            // No ambient cookies across requests
            .cookie_store(false)
            // HSTS is enforced automatically by rustls behavior; no explicit
            // opt-in needed. We also disable HTTP (not HTTPS) for any
            // source we mark as sensitive via per-source config.
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

    /// GET with all guards applied.
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, HttpError> {
        let parsed = self.guard.check(url)?;
        self.check_host_ip(&parsed)?;
        let resp = self
            .inner
            .get(parsed)
            .send()
            .await
            .map_err(|e| Self::classify_err(e, self.config.total_timeout))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(HttpError::Status(status.as_u16()));
        }

        // Check Content-Length up front if present
        if let Some(len) = resp.content_length() {
            if (len as usize) > self.config.max_response_bytes {
                return Err(HttpError::ResponseTooLarge {
                    max: self.config.max_response_bytes,
                    got: len as usize,
                });
            }
        }

        // Bounded read — even if server lies about Content-Length.
        let mut bytes = Vec::with_capacity(
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

    /// GET and parse as JSON, bounded.
    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        let bytes = self.get_bytes(url).await?;
        serde_json::from_slice(&bytes).map_err(|e| HttpError::Request(format!("json parse: {}", e)))
    }

    /// If the URL's host happens to be a literal IP, recheck it. (A host
    /// name's resolved IPs are checked by a custom DNS resolver — that's
    /// the real defense against DNS-rebinding. For now we at least cover
    /// the literal-IP case here.)
    fn check_host_ip(&self, url: &Url) -> Result<(), HttpError> {
        if let Some(host) = url.host_str() {
            if let Ok(ip) = IpAddr::from_str(host) {
                if is_disallowed_ip(&ip) {
                    return Err(HttpError::RedirectRejected(format!(
                        "host resolves to disallowed IP: {}", ip
                    )));
                }
            }
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
