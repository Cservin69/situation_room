-- Situation_room schema, version 0022.
--
-- Session 98 Bug 4 follow-on — entity rewire on plan-reject when an
-- accepted plan claims the same exemplar.
--
-- ## Background
--
-- Sn-97 migration 0021 cleaned up orphan `entity_synth` exemplar rows
-- whose claiming plan was rejected. The cleanup deliberately KEPT
-- rows when another accepted plan named the same exemplar in its
-- expectations — biasing toward keeping data — but did NOT rewire
-- the row's `source_id`. After Sn-97, an entity born to a rejected
-- plan and later re-claimed by an accepted plan still carries
-- `source_id = 'plan:{rejected}#entity_exemplar'`. The accepted
-- plan's per-plan dashboard view (`records_for_plan`'s LIKE filter
-- on `plan:{accepted}#%`) misses the row.
--
-- Sn-98 introduces a rewire step: for every entity row whose
-- current `source_id` points at a rejected plan AND at least one
-- accepted plan's expectations name the same `entity_id` as a
-- quoted JSON substring, UPDATE the `source_id` to point at the
-- earliest-created accepted plan that names it.
--
-- ## What gets rewired
--
-- Entity rows are mutated IRREVERSIBLY when:
--   (a) their current provenance `source_id` matches the
--       `plan:{x}#entity_exemplar` pattern for a plan x whose
--       status is `'rejected'`, AND
--   (b) at least one `'accepted'` plan's expectations JSON contains
--       the entity_id as a quoted string.
--
-- The new `source_id` points at the earliest-created accepted plan
-- that names the entity_id (deterministic tiebreaker).
--
-- ## Closed-vocabulary discipline (ADR 0003 / 0017)
--
-- This migration touches the `entity_synth` exemplar provenance
-- shape only. The `plan:{plan_id}#entity_exemplar` format is the
-- Sn-76 contract; nothing else in the schema produces a source_id
-- with that suffix. The closed-vocab guarantee is in the shape of
-- the pattern, not the host string.
--
-- ## Why a separate migration from 0021
--
-- 0021 already shipped. Sn-97 production binaries have stamped
-- schema_migrations.version=21 with `source_id` un-rewired. Editing
-- 0021 to add the rewire would never run on those installations.
-- A new 0022 closes the gap.

------------------------------------------------------------------------
-- Rewire orphan-but-claimed entities to their earliest-accepted
-- claimer. Single UPDATE; the correlated subquery picks the
-- deterministic destination plan.
------------------------------------------------------------------------

UPDATE entities AS e
   SET source_id = (
     SELECT 'plan:' || CAST(rp_acc.id AS VARCHAR) || '#entity_exemplar'
       FROM research_plans rp_acc
      WHERE rp_acc.status = 'accepted'
        AND CAST(rp_acc.expectations AS VARCHAR)
              LIKE '%"' || e.entity_id || '"%'
      ORDER BY rp_acc.created_at ASC
      LIMIT 1
   )
 WHERE e.source_id LIKE 'plan:%#entity_exemplar'
   AND EXISTS (
     SELECT 1 FROM research_plans rp_rej
      WHERE rp_rej.status = 'rejected'
        AND e.source_id =
              'plan:' || CAST(rp_rej.id AS VARCHAR) || '#entity_exemplar'
   )
   AND EXISTS (
     SELECT 1 FROM research_plans rp_acc
      WHERE rp_acc.status = 'accepted'
        AND CAST(rp_acc.expectations AS VARCHAR)
              LIKE '%"' || e.entity_id || '"%'
   );

------------------------------------------------------------------------
-- Record this migration.
------------------------------------------------------------------------

INSERT INTO schema_migrations (version, description)
    VALUES (22, 'Sn-98: rewire orphan-but-claimed entity_exemplar source_id to earliest-accepted claimer');
