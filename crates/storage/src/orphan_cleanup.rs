//! Storage-layer orphan-cleanup helpers (Session 97).
//!
//! Sibling to the one-shot cleanup logic in
//! `migrations/0021_sn97_orphan_cleanup.sql`. The migration handles
//! data that pre-dated the cleanup; this module handles data that
//! becomes orphaned at runtime (today: plans rejected after Sn-97).
//!
//! ## Scope
//!
//! - **[`Store::cleanup_orphan_entities_for_rejected_plan`]** —
//!   Sn-97 Bug 4 runtime path. Called from `reject_plan` after the
//!   plan's status flips to `Rejected`. Deletes Entity rows whose
//!   provenance points at this plan AND whose entity_id is not
//!   reused by any other accepted plan's expectations.
//!
//! ## What this module does NOT do
//!
//! - Rewire provenance. When a rejected plan's exemplar is *also*
//!   claimed by an accepted plan, the Entity row stays put with its
//!   original (now-rejected-plan) `source_id`. The cleanup pass is
//!   delete-or-keep; it never edits surviving rows. This keeps the
//!   helper auditable: every changed row is either gone or
//!   untouched.
//! - Touch the recipe / fetch_run trail. Rejected plans never spawn
//!   recipes (the executor is gated on `Accepted`), so a rejected
//!   plan has no fetched documents, no recipe-produced records.
//!   Only the `entity_synth` exemplar rows are at issue.
//! - Run on every reject_plan call when there are no exemplars to
//!   clean. The body executes the DELETE statements unconditionally
//!   — DuckDB's planner skips them when the predicate is empty in
//!   well under a millisecond. We keep the helper unconditional
//!   rather than threading an early-out for an empty exemplar list,
//!   since the expectation JSON parse adds more cost than the
//!   no-op DELETEs.

use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Summary of one cleanup pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanEntityCleanupReport {
    /// Entity rows removed.
    pub entities_deleted: u64,
    /// `record_subjects_entities` / `_places` / `_topics` join rows
    /// removed across the three tables (one summed count, since the
    /// caller doesn't need them split — they all belong to the
    /// deleted entities).
    pub subject_rows_deleted: u64,
    /// `record_derived_from` rows removed across both sides
    /// (`child_type='entity'` and `parent_type='entity'`).
    pub derivation_rows_deleted: u64,
}

impl Store {
    /// Delete Entity rows that this rejected plan created via
    /// `entity_synth::materialize_entity_exemplars` AND that no
    /// other accepted plan still claims via its expectations.
    ///
    /// This is the Sn-97 Bug 4 runtime path. Migration 0021 covers
    /// data that already existed at upgrade time; this helper is
    /// called from `reject_plan` for every subsequent rejection.
    ///
    /// Returns an [`OrphanEntityCleanupReport`] for observability.
    /// Errors propagate as `StorageError::DuckDb`; per-statement
    /// failures rollback the transaction so the cleanup is
    /// all-or-nothing per call.
    ///
    /// ## SQL shape
    ///
    /// Mirrors `migrations/0021_sn97_orphan_cleanup.sql`'s (1)
    /// section but with the rejected plan id bound at the call
    /// site rather than joined from `research_plans`. The
    /// equivalent membership predicate `source_id = 'plan:' ||
    /// CAST(plan_id AS VARCHAR) || '#entity_exemplar'` is built in
    /// Rust to keep the SQL parameter-shaped instead of literal-
    /// substring-built.
    ///
    /// The "not claimed by any accepted plan" check stays as a
    /// `LIKE '%"entity_id"%'` literal-substring scan over the
    /// expectations column. Same rationale as the migration: in our
    /// schema, entity_ids only appear inside
    /// `entity_kinds[*].exemplars[*]`, so a literal substring
    /// match is unambiguous. False positives leave the row in
    /// place — bias toward keeping data.
    pub fn cleanup_orphan_entities_for_rejected_plan(
        &self,
        plan_id: Uuid,
    ) -> Result<OrphanEntityCleanupReport> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let source_id_pattern =
            format!("plan:{}#entity_exemplar", plan_id);

        // Build the orphan-id selector once (we reference it from
        // each DELETE). We use a parameterised subquery so the
        // pattern is bound, not concatenated.
        //
        // The `NOT EXISTS` clause walks every accepted plan's
        // expectations and asks: does any of them carry this
        // entity_id as a quoted JSON string? If yes, the entity is
        // still claimed — keep it.
        let orphan_selector = r#"
            SELECT e.id FROM entities e
            WHERE e.source_id = ?
              AND NOT EXISTS (
                SELECT 1 FROM research_plans rp2
                WHERE rp2.status = 'accepted'
                  AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
              )
        "#;

        let mut subject_rows_deleted: u64 = 0;
        let mut derivation_rows_deleted: u64 = 0;

        // Subjects join rows — entity, place, topic. Sum the counts
        // for the caller; per-table breakdown isn't operator-useful.
        for table in ["record_subjects_entities", "record_subjects_places", "record_subjects_topics"] {
            let sql = format!(
                "DELETE FROM {table}
                  WHERE record_type = 'entity'
                    AND record_id IN ({orphan_selector})"
            );
            let n = tx
                .execute(&sql, params![source_id_pattern])
                .map_err(StorageError::DuckDb)?;
            subject_rows_deleted += n as u64;
        }

        // Derivation join rows — child side (entities-derived-from)
        // and parent side (something-derived-from-this-entity).
        for (col_type, col_id) in [("child_type", "child_id"), ("parent_type", "parent_id")] {
            let sql = format!(
                "DELETE FROM record_derived_from
                  WHERE {col_type} = 'entity'
                    AND {col_id} IN ({orphan_selector})"
            );
            let n = tx
                .execute(&sql, params![source_id_pattern])
                .map_err(StorageError::DuckDb)?;
            derivation_rows_deleted += n as u64;
        }

        // Finally the entity rows themselves.
        let entity_sql = format!("DELETE FROM entities WHERE id IN ({orphan_selector})");
        let entities_deleted = tx
            .execute(&entity_sql, params![source_id_pattern])
            .map_err(StorageError::DuckDb)? as u64;

        tx.commit().map_err(StorageError::DuckDb)?;

        Ok(OrphanEntityCleanupReport {
            entities_deleted,
            subject_rows_deleted,
            derivation_rows_deleted,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research_plans::{PlanStatus, ResearchPlanRow};
    use chrono::Utc;
    use duckdb::params;
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, EntityId};
    use situation_room_core::Entity;

    fn plan_row(id: Uuid, exemplars: &[&str], status: PlanStatus) -> ResearchPlanRow {
        // Minimal expectations JSON shaped to mirror the real
        // serialization — entity_kinds[].exemplars[] as quoted
        // strings. Other fields stay empty/default.
        let exemplars_json = exemplars
            .iter()
            .map(|e| format!("\"{}\"", e))
            .collect::<Vec<_>>()
            .join(",");
        let expectations = format!(
            r#"{{"observation_metrics":[],"event_types":[],"entity_kinds":[{{"kind":"company","exemplars":[{exemplars_json}],"attributes":[],"rationale":""}}],"relation_kinds":[],"document_sources":[],"assertion_guidance":null}}"#
        );
        ResearchPlanRow {
            id,
            topic: "test".into(),
            interpretation: "test".into(),
            topic_tags_json: "[]".into(),
            geographic_scope_json: "[]".into(),
            historical_window_days: 30,
            expectations_json: expectations,
            created_at: Utc::now(),
            classified_by: "test".into(),
            status,
            rejection_reason: None,
            reclassified_from: None,
        }
    }

    fn entity_for_plan(plan_id: Uuid, entity_id_s: &str) -> Entity {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!("plan:{}#entity_exemplar", plan_id),
                source_url: None,
                source_published_at: None,
                license: "classifier-emitted".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects {
                entities: vec![EntityId::new(entity_id_s).unwrap()],
                places: vec![],
                time: None,
                topics: vec![],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Entity::new(
            EntityId::new(entity_id_s).unwrap(),
            "company",
            entity_id_s.split_once(':').map(|(_, s)| s).unwrap_or(entity_id_s),
            envelope,
        )
    }

    #[test]
    fn cleanup_deletes_orphan_when_no_accepted_plan_claims_the_exemplar() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // Rejected plan with one exemplar that nobody else claims.
        let plan_id = Uuid::now_v7();
        let plan = plan_row(plan_id, &["company:orphan"], PlanStatus::Rejected);
        store.insert_research_plan(&plan).unwrap();

        let entity = entity_for_plan(plan_id, "company:orphan");
        store.insert_entity(&entity).unwrap();

        let report = store
            .cleanup_orphan_entities_for_rejected_plan(plan_id)
            .unwrap();

        assert_eq!(report.entities_deleted, 1, "orphan entity must be deleted");
        // Subject rows: one each for entity (envelope.subjects.entities)
        // and topic (Sn-76 propagates plan.topic_tags; in this fixture
        // topic_tags is empty so only the entity-subject row exists).
        assert!(report.subject_rows_deleted >= 1);
        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:orphan").unwrap())
            .is_err());
    }

    #[test]
    fn cleanup_keeps_entity_when_another_accepted_plan_claims_it() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // Rejected plan that synthesised the entity originally.
        let rejected_id = Uuid::now_v7();
        let rejected = plan_row(rejected_id, &["company:tsla"], PlanStatus::Rejected);
        store.insert_research_plan(&rejected).unwrap();

        // Accepted plan that ALSO names the same exemplar in its
        // expectations. The entity_synth idempotent skip-existing
        // path means the entity's source_id still points at the
        // rejected plan, even though the accepted plan needs it.
        let accepted_id = Uuid::now_v7();
        let accepted = plan_row(accepted_id, &["company:tsla"], PlanStatus::Accepted);
        store.insert_research_plan(&accepted).unwrap();

        let entity = entity_for_plan(rejected_id, "company:tsla");
        store.insert_entity(&entity).unwrap();

        let report = store
            .cleanup_orphan_entities_for_rejected_plan(rejected_id)
            .unwrap();

        assert_eq!(report.entities_deleted, 0, "claimed entity must survive");
        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .is_ok());
    }

    #[test]
    fn cleanup_is_idempotent_when_already_clean() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let plan = plan_row(plan_id, &[], PlanStatus::Rejected);
        store.insert_research_plan(&plan).unwrap();

        let first = store
            .cleanup_orphan_entities_for_rejected_plan(plan_id)
            .unwrap();
        let second = store
            .cleanup_orphan_entities_for_rejected_plan(plan_id)
            .unwrap();

        assert_eq!(first, OrphanEntityCleanupReport::default());
        assert_eq!(second, OrphanEntityCleanupReport::default());
    }

    #[test]
    fn cleanup_only_touches_entities_owned_by_the_named_plan() {
        // Two rejected plans each own one orphan. Cleaning up plan A
        // must NOT touch plan B's orphan even though both are
        // rejected.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();
        store
            .insert_research_plan(&plan_row(plan_a, &["company:a"], PlanStatus::Rejected))
            .unwrap();
        store
            .insert_research_plan(&plan_row(plan_b, &["company:b"], PlanStatus::Rejected))
            .unwrap();

        store.insert_entity(&entity_for_plan(plan_a, "company:a")).unwrap();
        store.insert_entity(&entity_for_plan(plan_b, "company:b")).unwrap();

        let report = store
            .cleanup_orphan_entities_for_rejected_plan(plan_a)
            .unwrap();

        assert_eq!(report.entities_deleted, 1);
        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:a").unwrap())
            .is_err());
        // Plan B's orphan stays put until its own cleanup runs.
        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:b").unwrap())
            .is_ok());
    }

    #[test]
    fn migration_0021_section_1_deletes_pre_existing_orphans() {
        // Replay the migration's Bug 4 cleanup against a synthesised
        // pre-Sn-97 store: a rejected plan with an entity that has
        // no other accepted-plan claimant. Equivalent to upgrading
        // an existing installation.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let plan_id = Uuid::now_v7();
        let plan = plan_row(plan_id, &["company:legacy"], PlanStatus::Rejected);
        store.insert_research_plan(&plan).unwrap();
        store
            .insert_entity(&entity_for_plan(plan_id, "company:legacy"))
            .unwrap();

        // Re-run the data-mutating portion of the migration.
        let sql = crate::migrate::migration_sql(21).expect("0021 migration must exist");
        let conn = store.conn.lock().unwrap();
        conn.execute_batch(sql).unwrap();
        drop(conn);

        assert!(store
            .get_entity_by_business_id(&EntityId::new("company:legacy").unwrap())
            .is_err());
    }

    #[test]
    fn migration_0021_section_2_deletes_dangling_parent_derivations() {
        // Bug 5 replay: insert a record_derived_from row whose
        // parent_id resolves to no per-table id. The migration's
        // section (2) must remove it.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let child_id = Uuid::now_v7();
        let dangling_parent_id = Uuid::now_v7(); // never inserted anywhere

        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO record_derived_from
                (child_id, child_type, parent_id, parent_type, role)
             VALUES (?, 'observation', ?, 'unknown', 'support')",
            params![child_id, dangling_parent_id],
        )
        .unwrap();
        drop(conn);

        // Re-run the migration. The conn.execute_batch path skips
        // the schema_migrations INSERT (PRIMARY KEY conflict) but
        // the DELETE statements still run.
        let sql = crate::migrate::migration_sql(21).expect("0021 migration must exist");
        let conn = store.conn.lock().unwrap();
        // Strip the trailing INSERT INTO schema_migrations so the
        // replay doesn't fail on PK collision (mirror of 0020's
        // test pattern).
        let cleanup_only: String = sql
            .lines()
            .filter(|l| !l.trim_start().to_lowercase().starts_with("insert into schema_migrations"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute_batch(&cleanup_only).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM record_derived_from WHERE parent_id = ?",
                params![dangling_parent_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "dangling parent-side derivation row must be deleted");
    }
}
