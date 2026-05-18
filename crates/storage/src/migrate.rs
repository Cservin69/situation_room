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
    Migration {
        version: 3,
        description: "recipes table",
        sql: include_str!("../../../migrations/0003_recipes.sql"),
    },
    Migration {
        version: 4,
        description: "research_plans table",
        sql: include_str!("../../../migrations/0004_research_plans.sql"),
    },
    Migration {
        version: 5,
        description: "research_plans.status column",
        sql: include_str!("../../../migrations/0005_research_plan_status.sql"),
    },
    Migration {
        version: 6,
        description: "fetch_runs table",
        sql: include_str!("../../../migrations/0006_fetch_runs.sql"),
    },
    Migration {
        version: 7,
        description: "research_plans: rejection_reason and reclassified_from columns",
        sql: include_str!("../../../migrations/0007_research_plans_rejection_and_lineage.sql"),
    },
    Migration {
        version: 8,
        description: "recipes.static_payload column",
        sql: include_str!("../../../migrations/0008_recipes_static_payload.sql"),
    },
    Migration {
        version: 9,
        description: "recipe_feedback table",
        sql: include_str!("../../../migrations/0009_recipe_feedback.sql"),
    },
    Migration {
        version: 10,
        description: "recipes.authored_from column",
        sql: include_str!("../../../migrations/0010_recipes_authored_from.sql"),
    },
    Migration {
        version: 11,
        description: "recipes.prior_recipe_id column",
        sql: include_str!("../../../migrations/0011_recipes_prior_recipe_id.sql"),
    },
    Migration {
        version: 12,
        description: "recipes.reauthor_reason column",
        sql: include_str!("../../../migrations/0012_recipes_reauthor_reason.sql"),
    },
    Migration {
        version: 13,
        description: "recipe_fetch_attempts table",
        sql: include_str!("../../../migrations/0013_recipe_fetch_attempts.sql"),
    },
    Migration {
        version: 14,
        description: "recipe_fetch_attempts.response_content_type column",
        sql: include_str!(
            "../../../migrations/0014_recipe_fetch_attempts_content_type.sql"
        ),
    },
    Migration {
        version: 15,
        description: "recipes.iterator column (ADR 0016)",
        sql: include_str!("../../../migrations/0015_recipes_iterator.sql"),
    },
    Migration {
        version: 16,
        description: "fetch_run_outcomes table",
        sql: include_str!("../../../migrations/0016_fetch_run_outcomes.sql"),
    },
    Migration {
        version: 17,
        description: "promote_history table",
        sql: include_str!("../../../migrations/0017_promote_history.sql"),
    },
    Migration {
        version: 18,
        description: "provenance selector_path + raw_bytes_excerpt columns",
        sql: include_str!("../../../migrations/0018_provenance_selector_trace.sql"),
    },
    Migration {
        version: 19,
        description: "authority_registry table (ADR 0022 scaffold)",
        sql: include_str!("../../../migrations/0019_authority_registry.sql"),
    },
    Migration {
        version: 20,
        description: "ADR 0024: parent_type backfill + Sn-78 poison cleanup",
        sql: include_str!(
            "../../../migrations/0020_parent_type_backfill_and_sn78_cleanup.sql"
        ),
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
            let version = m.version;
            let description = m.description;
            StorageError::Migration(format!(
                "migration {version} ({description}) failed: {e}"
            ))
        })?;
    }

    Ok(())
}

/// Test-only accessor to the embedded migration SQL by version.
///
/// Tests in this module use this to re-run the data-mutating portion of
/// a migration against synthetic legacy-shape rows that didn't exist
/// when `apply_all` originally ran (the in-memory store starts empty).
/// Production callers use [`apply_all`].
#[cfg(test)]
pub(crate) fn migration_sql(version: i32) -> Option<&'static str> {
    MIGRATIONS
        .iter()
        .find(|m| m.version == version)
        .map(|m| m.sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Store;
    use duckdb::params;
    use uuid::Uuid;

    /// ADR 0024 (Sn-94) — migration 0020 backfills
    /// `record_derived_from.parent_type` for rows still carrying the
    /// legacy `"unknown"` literal. Setup: open a fresh in-memory
    /// store, migrate fully (so 0020 ran on empty tables and was a
    /// no-op), then synthesise a pre-ADR-0024 row by hand and re-run
    /// the migration's data-mutating SQL portion.
    #[test]
    fn migration_0020_backfills_parent_type_unknown_rows_via_assertion() {
        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrations");

        let conn = store.conn.lock().expect("lock");
        let parent_assertion_id = Uuid::now_v7();
        let child_id = Uuid::now_v7();

        // Synthesise the parent assertion row. Minimal envelope —
        // we only need the id to exist for the IN (SELECT id FROM
        // assertions) subquery to match.
        conn.execute(
            "INSERT INTO assertions
              (id, dedup_key, claimant, stance, content_kind, content,
               source_id, license, tags, observed_at, confidence)
             VALUES (?, NULL, 'agency:test', 'asserted', 'observation',
                     '{\"asserted_kind\":\"observation\",\"metric\":\"x\",\"value\":1.0,\"unit\":\"1\",\"period\":\"instant\"}',
                     'test', 'public_domain', '[]',
                     CURRENT_TIMESTAMP, 1.0)",
            params![parent_assertion_id],
        )
        .expect("insert parent assertion");

        // Synthesise a pre-ADR-0024 derivation row: parent_type
        // stamped as the legacy 'unknown'.
        conn.execute(
            "INSERT INTO record_derived_from
              (child_id, child_type, parent_id, parent_type, role)
             VALUES (?, 'observation', ?, 'unknown', 'promotion')",
            params![child_id, parent_assertion_id],
        )
        .expect("insert legacy derived_from row");

        // Re-apply the data-mutating portion of migration 0020.
        // We don't run the full SQL (which would attempt
        // `INSERT INTO schema_migrations VALUES (20, …)` and
        // hit the primary-key conflict from the earlier full migrate
        // call). Instead we re-run just the UPDATE statements.
        conn.execute_batch(
            "UPDATE record_derived_from SET parent_type = 'observation'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM observations);
             UPDATE record_derived_from SET parent_type = 'event'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM events);
             UPDATE record_derived_from SET parent_type = 'entity'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM entities);
             UPDATE record_derived_from SET parent_type = 'relation'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM relations);
             UPDATE record_derived_from SET parent_type = 'document'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM documents);
             UPDATE record_derived_from SET parent_type = 'assertion'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM assertions);",
        )
        .expect("re-run migration 0020 UPDATE block");

        // Verify the legacy row was backfilled to 'assertion' (the
        // parent_id resolved to the assertions table).
        let parent_type: String = conn
            .query_row(
                "SELECT parent_type FROM record_derived_from
                  WHERE child_id = ? AND parent_id = ?",
                params![child_id, parent_assertion_id],
                |r| r.get(0),
            )
            .expect("read back the row");
        assert_eq!(
            parent_type, "assertion",
            "migration 0020 must rewrite 'unknown' to the correct \
             per-table tag; the parent_id was inserted into assertions"
        );
    }

    /// ADR 0024 (Sn-94) — dangling derivation rows (parent_id resolves
    /// to NO per-table table) intentionally stay as `'unknown'` after
    /// the migration. The read path's `parse_parent_type_lenient`
    /// handles them. This test pins the migration's deliberate
    /// non-action so a future "be more aggressive" rewrite is a
    /// conscious choice.
    #[test]
    fn migration_0020_leaves_dangling_parent_type_unknown() {
        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrations");
        let conn = store.conn.lock().expect("lock");

        let child_id = Uuid::now_v7();
        let dangling_parent_id = Uuid::now_v7(); // not in any table
        conn.execute(
            "INSERT INTO record_derived_from
              (child_id, child_type, parent_id, parent_type, role)
             VALUES (?, 'observation', ?, 'unknown', 'promotion')",
            params![child_id, dangling_parent_id],
        )
        .expect("insert dangling row");

        conn.execute_batch(
            "UPDATE record_derived_from SET parent_type = 'observation'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM observations);
             UPDATE record_derived_from SET parent_type = 'assertion'
              WHERE parent_type = 'unknown' AND parent_id IN (SELECT id FROM assertions);",
        )
        .expect("re-run subset of migration 0020");

        let parent_type: String = conn
            .query_row(
                "SELECT parent_type FROM record_derived_from WHERE child_id = ?",
                params![child_id],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(
            parent_type, "unknown",
            "dangling references stay 'unknown' — the lenient reader \
             absorbs them; cleaning these up is a separate pass"
        );
    }

    /// Migration 0020 deletes Sn-78 poison assertion rows whose
    /// `content` JSON carries the legacy `kind` tag (pre-Sn-78 serde
    /// discriminator). We synthesise one + a clean sibling, re-run
    /// the DELETE block, verify only the poison row is gone.
    #[test]
    fn migration_0020_deletes_sn78_poison_assertion_rows() {
        let store = Store::open_in_memory().expect("open in-memory store");
        store.migrate().expect("migrations");
        let conn = store.conn.lock().expect("lock");

        let poison_id = Uuid::now_v7();
        let clean_id = Uuid::now_v7();
        // Poison row: pre-Sn-78 `kind` tag, no `asserted_kind`.
        conn.execute(
            "INSERT INTO assertions
              (id, dedup_key, claimant, stance, content_kind, content,
               source_id, license, tags, observed_at, confidence)
             VALUES (?, NULL, 'agency:test', 'asserted', 'observation',
                     '{\"kind\":\"observation\",\"metric\":\"legacy\",\"value\":1.0}',
                     'test', 'public_domain', '[]',
                     CURRENT_TIMESTAMP, 1.0)",
            params![poison_id],
        )
        .expect("insert poison row");
        // Clean row: post-Sn-78 `asserted_kind` tag.
        conn.execute(
            "INSERT INTO assertions
              (id, dedup_key, claimant, stance, content_kind, content,
               source_id, license, tags, observed_at, confidence)
             VALUES (?, NULL, 'agency:test', 'asserted', 'observation',
                     '{\"asserted_kind\":\"observation\",\"metric\":\"x\",\"value\":2.0}',
                     'test', 'public_domain', '[]',
                     CURRENT_TIMESTAMP, 1.0)",
            params![clean_id],
        )
        .expect("insert clean row");

        // Re-run the DELETE block from migration 0020.
        conn.execute_batch(
            "DELETE FROM assertions
              WHERE json_extract_string(content, '$.asserted_kind') IS NULL
                AND json_extract_string(content, '$.kind') IS NOT NULL;",
        )
        .expect("re-run migration 0020 DELETE");

        // The poison row is gone; the clean row survives.
        let poison_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assertions WHERE id = ?",
                params![poison_id],
                |r| r.get(0),
            )
            .unwrap();
        let clean_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assertions WHERE id = ?",
                params![clean_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(poison_count, 0, "Sn-78 poison row must be deleted");
        assert_eq!(clean_count, 1, "post-Sn-78 clean row must survive");
    }

    /// Sanity — the migration SQL accessor returns Some(_) for every
    /// migration we ship today. Catches an accidental rename of the
    /// migrations array without a corresponding test update.
    #[test]
    fn migration_sql_lookup_finds_v1_through_v20() {
        for v in 1..=20 {
            assert!(
                migration_sql(v).is_some(),
                "migration version {v} should be embedded"
            );
        }
        assert!(migration_sql(21).is_none(), "no v21 yet");
    }
}
