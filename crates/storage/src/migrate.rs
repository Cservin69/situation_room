//! Migration runner.
//!
//! Migrations are embedded via `include_str!` from the top-level
//! `migrations/` directory. On every startup, [`apply_all`] iterates
//! the known versions in order, skips any already recorded in
//! `schema_migrations`, and applies the rest.
//!
//! The migrations themselves create the `schema_migrations` table on
//! their first run (with `IF NOT EXISTS`), so the bootstrap case works
//! the same way as the steady-state case: read what's applied, apply
//! what isn't.

use duckdb::Connection;

use crate::{Result, StorageError};

/// Each migration is a (version, description, sql) triple.
///
/// `version` must be monotonically increasing. Descriptions are free-
/// form but should be short. The SQL script is responsible for
/// inserting its own row into `schema_migrations`.
struct Migration {
    version: i32,
    description: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "initial schema",
        sql: include_str!("../../../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        description: "indexes",
        sql: include_str!("../../../migrations/0002_indexes.sql"),
    },
];

/// Apply every migration whose version is not yet in
/// `schema_migrations`. Safe to call on every startup.
pub fn apply_all(conn: &Connection) -> Result<()> {
    // Ensure the bookkeeping table exists before we query it. The
    // 0001 migration creates it, but we also need to *query* it
    // before 0001 has run on a fresh database. `CREATE IF NOT EXISTS`
    // makes this a no-op on subsequent runs.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version     INTEGER PRIMARY KEY,
            applied_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            description TEXT NOT NULL
         );",
    )
    .map_err(StorageError::DuckDb)?;

    let applied: std::collections::HashSet<i32> = {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations")
            .map_err(StorageError::DuckDb)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i32>(0))
            .map_err(StorageError::DuckDb)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(StorageError::DuckDb)?
    };

    for m in MIGRATIONS {
        if applied.contains(&m.version) {
            tracing::debug!(version = m.version, "migration already applied, skipping");
            continue;
        }
        tracing::info!(version = m.version, description = m.description, "applying migration");
        conn.execute_batch(m.sql).map_err(|e| {
            StorageError::Migration(format!(
                "migration {} ({}) failed: {}",
                m.version, m.description, e
            ))
        })?;
    }

    Ok(())
}
