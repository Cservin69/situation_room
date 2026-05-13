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
//! let store = Store::open("situation_room.duckdb")?;
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

/// A handle to the situation_room DuckDB store.
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

    /// Force DuckDB to flush the current buffer pool to disk.
    ///
    /// DuckDB's Rust binding holds writes in an in-memory buffer pool
    /// and only writes them to the on-disk `.duckdb` file at checkpoint
    /// boundaries: explicit `CHECKPOINT;`, transaction commit on a
    /// long-running connection, or on `Connection::drop`. The Drop
    /// path is the one we'd normally rely on, but it never runs when
    /// the process is killed by an un-handled SIGTERM (which is what
    /// `run_desktop.sh`'s trap sends when the operator Ctrl-C's the
    /// terminal). Session 65 traced today's intermittent
    /// "writes-vanish-between-desktop-sessions" bug to exactly that
    /// path.
    ///
    /// Calling this method from a signal handler — *before*
    /// `AppHandle::exit(0)` — guarantees the data is durable even if
    /// the AppState Drop chain is somehow short-circuited later. It's
    /// also a safe no-op when there are no pending writes.
    ///
    /// Cheap when there's nothing to flush; not free when there is, so
    /// don't call it on the hot path. Today's call sites are signal
    /// shutdown only.
    pub fn checkpoint(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        conn.execute_batch("CHECKPOINT;")
            .map_err(StorageError::DuckDb)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `checkpoint()` is a no-op on an empty in-memory store — should
    /// not error. The Session 66 signal-shutdown path calls it
    /// unconditionally, even on quick boot-then-exit cycles where
    /// nothing was written, so this case must succeed.
    #[test]
    fn checkpoint_on_fresh_in_memory_store_succeeds() {
        let store = Store::open_in_memory().expect("open in-memory store");
        // No migrate, no inserts — just verify CHECKPOINT doesn't reject.
        store.checkpoint().expect("checkpoint on fresh store");
    }

    /// `checkpoint()` on a migrated, written-to store flushes without
    /// error and the written bytes survive a fresh `Store::open` of the
    /// same path. This is the realistic Session-66 shape: schema_migrations
    /// populated, file-backed, an INSERT in flight, then `checkpoint()`
    /// before what would be a process exit. We use a tempdir-backed
    /// file (not `:memory:`) because CHECKPOINT semantics on the
    /// in-memory database don't exercise the disk write the bug fix
    /// is about. No `tempfile` dep — keep storage's deps narrow.
    #[test]
    fn checkpoint_durably_flushes_buffer_pool() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sr-checkpoint-{nonce}-{seq}.duckdb"
        ));
        // Best-effort cleanup of any prior run on this path.
        let _ = std::fs::remove_file(&path);

        {
            let store = Store::open(&path).expect("open file-backed store");
            store.migrate().expect("migrate");
            {
                let conn = store.conn.lock().expect("lock");
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS _sr_checkpoint_test (k TEXT);
                     INSERT INTO _sr_checkpoint_test (k) VALUES ('session66');",
                )
                .expect("seed write");
            }
            store.checkpoint().expect("checkpoint after write");
            // Drop normally. We can't `mem::forget` here without
            // leaking DuckDB's exclusive file lock — the reopen below
            // would fail. This means the assertion below is a smoke
            // test (Drop also checkpoints), not a strict proof. The
            // strict proof is the operator's live verification on Mac:
            // SIGTERM the binary mid-write and confirm the row
            // survives. See `SESSION_66_HANDOFF.md` for the runbook.
            drop(store);
        }

        // Re-open. The seeded row must be readable — same path,
        // separate Connection, separate buffer pool. If checkpoint
        // didn't actually flush, this read would return 0 rows.
        let reopened = Store::open(&path).expect("reopen file-backed store");
        let count: u64 = {
            let conn = reopened.conn.lock().expect("lock");
            conn.query_row(
                "SELECT COUNT(*) FROM _sr_checkpoint_test WHERE k = 'session66'",
                [],
                |row| row.get(0),
            )
            .expect("count seeded rows")
        };
        assert_eq!(
            count, 1,
            "checkpoint() must persist writes to disk before process exit"
        );

        // Best-effort cleanup.
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }
}
