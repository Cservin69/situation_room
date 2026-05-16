//! DB-backed authoritative-source registry (Session 88, ADR 0022).
//!
//! Stage-1 scaffold per ADR 0022: this module exposes a typed
//! storage row + CRUD methods on [`Store`] backed by migration
//! `0019_authority_registry.sql`. The runtime registry
//! ([`situation_room_pipeline::authoritative::AuthorityRegistry`] +
//! [`situation_room_pipeline::authoritative_live::LiveAuthorityRegistry`])
//! is **unchanged** in Session 88 and continues to read the TOML at
//! boot — these methods exist for the next session's two-stage
//! migration (seed the table from TOML on first boot, then switch
//! the runtime read to the DB).
//!
//! See `docs/adr/0022-authority-registry-db-backed.md` for the full
//! plan and the Stage 1 vs. Stage 2 split.

use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Closed-vocab provenance tag matching the `provenance` column on
/// `authority_registry`. Two variants today (`toml_seed` and
/// `operator`); a future variant would require both an enum addition
/// and an ADR amendment to keep the closed-vocab discipline auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorityProvenance {
    /// Row was imported from `config/vocab/authoritative_sources.toml`
    /// at boot (Stage 2's seed-on-empty pass).
    TomlSeed,
    /// Row was added by an operator via a future TUI / CLI surface.
    Operator,
}

impl AuthorityProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TomlSeed => "toml_seed",
            Self::Operator => "operator",
        }
    }

    /// Parse from the TEXT column, returning [`StorageError::Other`]
    /// on an unknown variant. Closed-vocab discipline: the migration
    /// SQL pins the default to `'toml_seed'`, and rows are written
    /// exclusively through [`Store::upsert_authority_entry`] which
    /// goes through [`Self::as_str`], so this path only fires if the
    /// DB was hand-edited.
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "toml_seed" => Ok(Self::TomlSeed),
            "operator" => Ok(Self::Operator),
            other => Err(StorageError::Other(format!(
                "unknown authority_registry.provenance: `{other}`"
            ))),
        }
    }
}

/// One row from the `authority_registry` table.
///
/// Mirrors the in-memory
/// [`situation_room_pipeline::authoritative::AuthorityEntry`]
/// one-for-one + a row id + provenance tag + created/updated stamps.
/// `metric` and `topic` are nullable to match the TOML schema's
/// "optional scoping field" shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityRegistryRow {
    pub id: Uuid,
    pub source_id: String,
    pub metric: Option<String>,
    pub topic: Option<String>,
    pub consensus_quorum: Option<u32>,
    pub provenance: AuthorityProvenance,
}

impl Store {
    /// List every row in the `authority_registry` table, in insertion
    /// order. Stage 2 will call this from the runtime boot path;
    /// Stage 1 ships the method for the seed-on-empty integration
    /// test to consume.
    pub fn authority_registry_entries(&self) -> Result<Vec<AuthorityRegistryRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, metric, topic, consensus_quorum, provenance
                 FROM authority_registry
                 ORDER BY created_at, id",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Uuid>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, source_id, metric, topic, quorum_i, prov_s) =
                row.map_err(StorageError::DuckDb)?;
            out.push(AuthorityRegistryRow {
                id,
                source_id,
                metric,
                topic,
                consensus_quorum: quorum_i.and_then(|q| u32::try_from(q).ok()),
                provenance: AuthorityProvenance::from_str(&prov_s)?,
            });
        }
        Ok(out)
    }

    /// Upsert one row keyed on `(source_id, metric, topic)`. Insert
    /// if no matching row exists, otherwise update
    /// `consensus_quorum` + `provenance` + bump `updated_at`. The
    /// `(source_id, metric, topic)` triple is the natural key the
    /// promote-stage lookup matches on.
    pub fn upsert_authority_entry(
        &self,
        source_id: &str,
        metric: Option<&str>,
        topic: Option<&str>,
        consensus_quorum: Option<u32>,
        provenance: AuthorityProvenance,
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // DuckDB doesn't have a portable `ON CONFLICT` for non-PK
        // composite keys, so we do an explicit SELECT-then-INSERT-or-
        // UPDATE inside a transaction. The pattern matches
        // `sources_memory.rs`'s upsert.
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let existing_id: Option<Uuid> = tx
            .query_row(
                "SELECT id FROM authority_registry
                 WHERE source_id = ?
                   AND ((metric IS NULL AND ? IS NULL) OR metric = ?)
                   AND ((topic IS NULL AND ? IS NULL) OR topic = ?)",
                params![source_id, metric, metric, topic, topic],
                |r| r.get(0),
            )
            .ok();

        let quorum_i: Option<i64> = consensus_quorum.map(|q| q as i64);

        match existing_id {
            Some(id) => {
                tx.execute(
                    "UPDATE authority_registry
                     SET consensus_quorum = ?,
                         provenance = ?,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?",
                    params![quorum_i, provenance.as_str(), id],
                )
                .map_err(StorageError::DuckDb)?;
            }
            None => {
                tx.execute(
                    "INSERT INTO authority_registry
                       (id, source_id, metric, topic, consensus_quorum, provenance)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        Uuid::new_v4(),
                        source_id,
                        metric,
                        topic,
                        quorum_i,
                        provenance.as_str(),
                    ],
                )
                .map_err(StorageError::DuckDb)?;
            }
        }
        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    /// Empty the table. Used by the (future) seed-on-empty boot path
    /// when the operator requests a "reset to TOML seed" from the
    /// dashboard. No-op safe.
    pub fn clear_authority_registry(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        conn.execute("DELETE FROM authority_registry", [])
            .map_err(StorageError::DuckDb)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_store() -> Store {
        let store = Store::open_in_memory().expect("open_in_memory");
        store.migrate().expect("migrations");
        store
    }

    #[test]
    fn authority_provenance_roundtrips_through_string() {
        assert_eq!(AuthorityProvenance::from_str("toml_seed").unwrap(), AuthorityProvenance::TomlSeed);
        assert_eq!(AuthorityProvenance::from_str("operator").unwrap(), AuthorityProvenance::Operator);
        assert!(AuthorityProvenance::from_str("unknown").is_err());
    }

    #[test]
    fn empty_table_returns_empty_vec() {
        let store = fresh_store();
        let entries = store.authority_registry_entries().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn upsert_inserts_new_row() {
        let store = fresh_store();
        store
            .upsert_authority_entry(
                "epa:fred",
                Some("unemployment_rate"),
                None,
                Some(2),
                AuthorityProvenance::TomlSeed,
            )
            .unwrap();
        let entries = store.authority_registry_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_id, "epa:fred");
        assert_eq!(entries[0].metric.as_deref(), Some("unemployment_rate"));
        assert_eq!(entries[0].topic, None);
        assert_eq!(entries[0].consensus_quorum, Some(2));
        assert_eq!(entries[0].provenance, AuthorityProvenance::TomlSeed);
    }

    #[test]
    fn upsert_updates_existing_row_on_natural_key_match() {
        let store = fresh_store();
        store
            .upsert_authority_entry(
                "agency:reuters",
                None,
                Some("federal_reserve"),
                Some(2),
                AuthorityProvenance::TomlSeed,
            )
            .unwrap();
        let first_id = store.authority_registry_entries().unwrap()[0].id;
        store
            .upsert_authority_entry(
                "agency:reuters",
                None,
                Some("federal_reserve"),
                Some(1),
                AuthorityProvenance::Operator,
            )
            .unwrap();
        let entries = store.authority_registry_entries().unwrap();
        assert_eq!(entries.len(), 1, "natural-key match should update, not insert");
        assert_eq!(entries[0].id, first_id, "id should be preserved across upserts");
        assert_eq!(entries[0].consensus_quorum, Some(1));
        assert_eq!(entries[0].provenance, AuthorityProvenance::Operator);
    }

    #[test]
    fn clear_removes_all_rows() {
        let store = fresh_store();
        store
            .upsert_authority_entry("a", None, None, Some(2), AuthorityProvenance::TomlSeed)
            .unwrap();
        store
            .upsert_authority_entry("b", None, None, Some(3), AuthorityProvenance::TomlSeed)
            .unwrap();
        assert_eq!(store.authority_registry_entries().unwrap().len(), 2);
        store.clear_authority_registry().unwrap();
        assert_eq!(store.authority_registry_entries().unwrap().len(), 0);
    }

    #[test]
    fn null_metric_and_topic_distinguish_separate_rows() {
        // (source_id, NULL, NULL) and (source_id, "production", NULL)
        // are different natural-key tuples — the SQL `IS NULL` guard
        // in upsert keeps them distinct.
        let store = fresh_store();
        store
            .upsert_authority_entry("usgs", None, None, Some(2), AuthorityProvenance::TomlSeed)
            .unwrap();
        store
            .upsert_authority_entry(
                "usgs",
                Some("production"),
                None,
                Some(1),
                AuthorityProvenance::TomlSeed,
            )
            .unwrap();
        let entries = store.authority_registry_entries().unwrap();
        assert_eq!(entries.len(), 2);
    }
}
