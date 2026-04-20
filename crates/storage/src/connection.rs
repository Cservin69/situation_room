//! DuckDB connection management.
//!
//! A [`Store`] wraps a DuckDB connection and exposes the per-record-type
//! modules ([`super::observations`], etc.) as methods. We keep the
//! connection behind a `Mutex` because DuckDB's Rust binding is
//! `!Sync`, and we expect the storage layer to be called from async
//! code via `tokio::task::spawn_blocking` rather than contended
//! directly.
//!
//! ## Lifecycle
//!
//! ```ignore
//! let store = Store::open("stockpile.duckdb")?;
//! store.migrate()?;  // idempotent; safe to call on every startup
//! // ... use the store ...
//! ```
//!
//! [`Store::open_in_memory`] is provided for tests; it yields a
//! connection to DuckDB's in-memory mode with the same schema applied.

use duckdb::Connection;
use std::path::Path;
use std::sync::Mutex;

use crate::{Result, StorageError};

/// A handle to the Stockpile DuckDB store.
///
/// Clonable via `Arc<Store>` at the application layer; we don't derive
/// `Clone` on `Store` itself because the underlying connection is not
/// cheap to duplicate.
pub struct Store {
    pub(crate) conn: Mutex<Connection>,
}

impl Store {
    /// Open a file-backed store. The file is created if it doesn't exist.
    /// Does not apply migrations — call [`Self::migrate`] after opening.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(StorageError::DuckDb)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory store. Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(StorageError::DuckDb)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Apply all pending migrations. Idempotent — applied migrations
    /// are tracked in the `schema_migrations` table. See
    /// [`super::migrate::apply_all`].
    pub fn migrate(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        super::migrate::apply_all(&conn)
    }
}
