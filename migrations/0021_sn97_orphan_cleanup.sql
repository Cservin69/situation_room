-- Situation_room schema, version 0021.
--
-- Bundled cleanup pass — Session 97. Two independent data-integrity
-- fixes carried forward from Sn-94/96 handoffs. Both touch row
-- contents only — no column shapes, no new tables, no new indices.
--
--   (1) Bug 4 — orphan entity_synth exemplars. Sn-76 wired
--       `entity_synth::materialize_entity_exemplars` to write Entity
--       rows at plan-accept time with provenance
--       `source_id = 'plan:{plan_id}#entity_exemplar'`. When the
--       plan is later rejected, the Entity rows linger — they
--       inflate `records_recent_global`'s entities count even though
--       the rejected plan's per-plan panel correctly shows zero.
--       This migration deletes orphan exemplars left behind by
--       already-rejected plans. Going forward, `reject_plan` calls
--       the same cleanup at the Rust layer
--       (`Store::cleanup_orphan_entities_for_rejected_plan`).
--
--   (2) Bug 5 — dangling parent-side `record_derived_from` rows.
--       Sn-94's migration 0020 deliberately left rows whose
--       `parent_id` resolves to no per-table match as
--       `parent_type = 'unknown'` (see the closing comment of
--       0020). The Sn-94 handoff named the cleanup as a future pass.
--       After migration 0020 ran, the read path's
--       `parse_parent_type_lenient` treats these as Assertion + a
--       debug-log; deleting them tightens `n_derived_from`
--       aggregates without affecting any traceable record.
--
-- Bundled because both touch operator-visible row counts and both
-- are one-shot pure-SQL passes; the Sn-94 precedent of bundling
-- shape-preserving cleanups into a single migration applies.
--
-- ## What gets deleted
--
-- (1) Entity rows are deleted IRREVERSIBLY when:
--       a) their provenance `source_id` matches the
--          `plan:{x}#entity_exemplar` pattern for a plan x whose
--          status is `'rejected'`, AND
--       b) no `'accepted'` plan's expectations JSON contains the
--          entity_id as a quoted string.
--     Predicate (b) is a literal-substring check against the
--     serialized expectations column. False positives are
--     structurally implausible because entity_ids have the
--     `prefix:slug` shape and only appear in entity_kinds[*].
--     exemplars[*] in our schema; if a different field ever
--     legitimately carries the same string, the safety bias is
--     toward keeping the entity row (skip the delete).
--     Matching record_subjects_entities/_places/_topics rows and
--     parent-side / child-side record_derived_from rows are
--     deleted in the same transaction to keep joins consistent.
--
-- (2) record_derived_from rows are deleted IRREVERSIBLY when their
--     `parent_id` resolves to no row in any of the six per-record
--     tables. These edges are unreachable from any detail panel —
--     the dashboard already shows them as the lenient Assertion
--     fallback with a debug-log. The Sn-94 0020 tail comment
--     explicitly framed this as a Sn-95+ candidate; Sn-97 picks
--     it up.
--
-- ## Closed-vocabulary discipline (ADR 0003 / 0017)
--
-- The six per-table NOT IN predicates in (2) name the six closed
-- vocab record types verbatim, matching 0020's posture. Adding a
-- seventh record type would require an ADR amendment AND a new
-- predicate here. There is no catch-all branch.

------------------------------------------------------------------------
-- (1) Bug 4 — orphan entity_synth exemplars from rejected plans.
--
-- Strategy: build a CTE (`orphan_entities`) that names every Entity
-- row meeting both conditions, then DELETE from each side-table that
-- references those ids before the final DELETE on `entities`. CTEs
-- are scoped per statement in DuckDB so we re-state the same SELECT
-- in each statement; the planner re-uses the join shape.
------------------------------------------------------------------------

-- Subjects join rows — entities.
DELETE FROM record_subjects_entities
 WHERE record_type = 'entity'
   AND record_id IN (
     SELECT e.id FROM entities e
     JOIN research_plans rp
       ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
     WHERE rp.status = 'rejected'
       AND NOT EXISTS (
         SELECT 1 FROM research_plans rp2
         WHERE rp2.status = 'accepted'
           AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
       )
   );

-- Subjects join rows — places (envelope.subjects.places).
DELETE FROM record_subjects_places
 WHERE record_type = 'entity'
   AND record_id IN (
     SELECT e.id FROM entities e
     JOIN research_plans rp
       ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
     WHERE rp.status = 'rejected'
       AND NOT EXISTS (
         SELECT 1 FROM research_plans rp2
         WHERE rp2.status = 'accepted'
           AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
       )
   );

-- Subjects join rows — topics.
DELETE FROM record_subjects_topics
 WHERE record_type = 'entity'
   AND record_id IN (
     SELECT e.id FROM entities e
     JOIN research_plans rp
       ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
     WHERE rp.status = 'rejected'
       AND NOT EXISTS (
         SELECT 1 FROM research_plans rp2
         WHERE rp2.status = 'accepted'
           AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
       )
   );

-- Derivation child-side (entities derived from something else).
DELETE FROM record_derived_from
 WHERE child_type = 'entity'
   AND child_id IN (
     SELECT e.id FROM entities e
     JOIN research_plans rp
       ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
     WHERE rp.status = 'rejected'
       AND NOT EXISTS (
         SELECT 1 FROM research_plans rp2
         WHERE rp2.status = 'accepted'
           AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
       )
   );

-- Derivation parent-side (something derived from a to-be-deleted entity).
DELETE FROM record_derived_from
 WHERE parent_type = 'entity'
   AND parent_id IN (
     SELECT e.id FROM entities e
     JOIN research_plans rp
       ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
     WHERE rp.status = 'rejected'
       AND NOT EXISTS (
         SELECT 1 FROM research_plans rp2
         WHERE rp2.status = 'accepted'
           AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
       )
   );

-- Finally delete the entity rows themselves.
DELETE FROM entities
 WHERE id IN (
   SELECT e.id FROM entities e
   JOIN research_plans rp
     ON e.source_id = 'plan:' || CAST(rp.id AS VARCHAR) || '#entity_exemplar'
   WHERE rp.status = 'rejected'
     AND NOT EXISTS (
       SELECT 1 FROM research_plans rp2
       WHERE rp2.status = 'accepted'
         AND rp2.expectations LIKE '%"' || e.entity_id || '"%'
     )
 );

------------------------------------------------------------------------
-- (2) Bug 5 — dangling parent-side record_derived_from rows.
--
-- After (1) runs, any record_derived_from row whose parent_id no
-- longer resolves to any per-table id is dangling. Includes:
--   - rows left as parent_type='unknown' by migration 0020 (Sn-78
--     poison delete plus any prior manual cull),
--   - rows whose parent entity was just deleted by section (1) above
--     and that we missed because they were already referencing a
--     different deleted parent (defensive coverage),
--   - rows from any future delete path that doesn't sweep its
--     derivation edges.
------------------------------------------------------------------------

DELETE FROM record_derived_from
 WHERE parent_id NOT IN (SELECT id FROM observations)
   AND parent_id NOT IN (SELECT id FROM events)
   AND parent_id NOT IN (SELECT id FROM entities)
   AND parent_id NOT IN (SELECT id FROM relations)
   AND parent_id NOT IN (SELECT id FROM documents)
   AND parent_id NOT IN (SELECT id FROM assertions);

------------------------------------------------------------------------
-- Record this migration.
------------------------------------------------------------------------

INSERT INTO schema_migrations (version, description)
    VALUES (21, 'Sn-97: orphan entity_exemplar + dangling parent derivation cleanup');
