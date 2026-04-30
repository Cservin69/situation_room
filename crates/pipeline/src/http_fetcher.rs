//! HTTP-fetch abstraction for the fetch executor.
//!
//! The runtime executor (`fetch_executor`) needs to fetch URLs but
//! must remain testable without a real network. This module defines
//! a single-method trait the executor depends on, and provides a
//! blanket impl for [`stockpile_secure::http::SecureHttpClient`] so
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

use async_trait::async_trait;
use thiserror::Error;

use stockpile_secure::http::{HttpError, SecureHttpClient};

/// Error raised by an [`HttpFetcher`] implementation.
#[derive(Debug, Error)]
pub enum FetchError {
    /// Wraps the production `secure::HttpError`. The Display impl
    /// preserves the underlying detail for logs.
    #[error("http error: {0}")]
    Http(String),

    /// Test/mock implementations use this for "no fixture for this
    /// URL". The real `SecureHttpClient` impl never returns this.
    #[error("no fixture configured for url: {0}")]
    NoFixture(String),
}

impl From<HttpError> for FetchError {
    fn from(e: HttpError) -> Self {
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
        self.get_bytes(url).await.map_err(FetchError::from)
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
            }
        }

        pub fn with(mut self, url: &str, bytes: &[u8]) -> Self {
            self.fixtures
                .get_mut()
                .unwrap()
                .insert(url.to_string(), bytes.to_vec());
            self
        }
    }

    #[async_trait]
    impl HttpFetcher for StaticFetcher {
        async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
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
