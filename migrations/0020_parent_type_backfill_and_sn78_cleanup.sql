-- Situation_room schema, version 0020.
--
-- Bundled cleanup pass — Session 94. Two independent data-integrity
-- fixes that touch row contents but not column shapes:
--
--   (1) ADR 0024 — backfill `record_derived_from.parent_type` for
--       rows still carrying the legacy `"unknown"` literal. Pre-Sn-94
--       writers stamped the value as `"unknown"` because the in-memory
--       `DerivedFrom` struct had no `record_type` field. New rows
--       written under ADR 0024's code path carry the real tag; this
--       migration rewrites the existing rows by joining against each
--       per-record-type table.
--
--   (2) Sn-78 poison cleanup — delete Assertion rows whose `content`
--       JSON carries the pre-Sn-78 serde discriminator (`kind`)
--       instead of the post-Sn-78 one (`asserted_kind`). The read
--       path already tolerates these (`decode_assertion_row` skips
--       them with a warn-log) but they sit on disk and inflate raw
--       row counts. The session78-verify.sh runbook shipped manual
--       cleanup SQL; making it a migration ensures operators who
--       never ran it pick up the fix on next boot.
--
-- Bundled because both touch operator-visible row counts and both
-- are one-shot pure-SQL passes — running them in the same migration
-- keeps the schema_migrations bookkeeping linear and the operator
-- mental model "0020 cleans up legacy data" simple.
--
-- ## What gets deleted
--
-- The DELETE on `assertions` is irreversible. The matching rows are
-- structurally broken (their `content` cannot deserialize under the
-- current code), so deleting them is what frees the row count from
-- the silent skip path; the practical effect on the dashboard is
-- nothing — those rows already don't render.
--
-- The matching subjects + derivation join rows are also deleted in
-- the same transaction to keep the joins consistent with the parent
-- assertion table.
--
-- ## Closed-vocabulary discipline (ADR 0003 / 0017)
--
-- The six UPDATE statements below name the six closed-vocab record
-- types verbatim. Adding a seventh record type would require an ADR
-- amendment (per ADR 0003) AND a new UPDATE here. There is no
-- catch-all branch.
--
-- ## Why join order doesn't matter
--
-- Each UPDATE filters on `parent_type = 'unknown'`, so once a row is
-- backfilled it stops matching the predicate and subsequent UPDATEs
-- skip it. UUIDs are unique across the six per-type tables by
-- construction (the schema invariant), so the same parent_id never
-- appears in two tables — there's nothing to disambiguate.

------------------------------------------------------------------------
-- (1) ADR 0024 — backfill parent_type from 'unknown' to the real tag.
------------------------------------------------------------------------

UPDATE record_derived_from
   SET parent_type = 'observation'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM observations);

UPDATE record_derived_from
   SET parent_type = 'event'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM events);

UPDATE record_derived_from
   SET parent_type = 'entity'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM entities);

UPDATE record_derived_from
   SET parent_type = 'relation'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM relations);

UPDATE record_derived_from
   SET parent_type = 'document'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM documents);

UPDATE record_derived_from
   SET parent_type = 'assertion'
 WHERE parent_type = 'unknown'
   AND parent_id IN (SELECT id FROM assertions);

-- Rows whose parent_id resolves to NO per-table match are intentionally
-- left as 'unknown'. They signal a dangling derivation edge — the
-- parent record was deleted (cull pass, manual SQL) but the join row
-- was preserved on purpose by `delete_subjects_and_derivation` which
-- only cleans the *child* side. The read path's
-- `parse_parent_type_lenient` defaults dangling edges to Assertion +
-- a debug-log; surfacing them as a separate cleanup pass is a
-- Sn-95+ candidate.

------------------------------------------------------------------------
-- (2) Sn-78 poison cleanup — delete assertions with legacy
--     AssertedContent serde tag.
--
-- The serde discriminator was renamed `kind` → `asserted_kind` in
-- Sn-78. Rows written before that rename carry the old tag and fail
-- to deserialize under the current code. The clean-up has been a
-- manual SQL step since Sn-78; making it a migration removes the
-- "operator forgot to run cleanup" failure mode.
------------------------------------------------------------------------

-- Clean up the join rows first so the assertion delete leaves no
-- orphans in record_subjects_entities / _places / _topics.
DELETE FROM record_subjects_entities
 WHERE record_type = 'assertion'
   AND record_id IN (
     SELECT id FROM assertions
      WHERE json_extract_string(content, '$.asserted_kind') IS NULL
        AND json_extract_string(content, '$.kind') IS NOT NULL
   );

DELETE FROM record_subjects_places
 WHERE record_type = 'assertion'
   AND record_id IN (
     SELECT id FROM assertions
      WHERE json_extract_string(content, '$.asserted_kind') IS NULL
        AND json_extract_string(content, '$.kind') IS NOT NULL
   );

DELETE FROM record_subjects_topics
 WHERE record_type = 'assertion'
   AND record_id IN (
     SELECT id FROM assertions
      WHERE json_extract_string(content, '$.asserted_kind') IS NULL
        AND json_extract_string(content, '$.kind') IS NOT NULL
   );

-- The child-side derivation rows: deleted assertions can't be a
-- derivation child for anyone meaningfully.
DELETE FROM record_derived_from
 WHERE child_type = 'assertion'
   AND child_id IN (
     SELECT id FROM assertions
      WHERE json_extract_string(content, '$.asserted_kind') IS NULL
        AND json_extract_string(content, '$.kind') IS NOT NULL
   );

-- Parent-side derivation rows: a deleted poison assertion that some
-- promoted record references via parent_id becomes a dangling edge.
-- The (1) backfill above intentionally leaves those as 'unknown'
-- after the per-table lookup misses; the read path's
-- `parse_parent_type_lenient` treats them as Assertion + a
-- debug-log. We do NOT delete the parent-side rows here because
-- that would silently break the consensus-support / promotion
-- traceability of the surviving promoted records.

-- Finally delete the assertion rows themselves.
DELETE FROM assertions
 WHERE json_extract_string(content, '$.asserted_kind') IS NULL
   AND json_extract_string(content, '$.kind') IS NOT NULL;

------------------------------------------------------------------------
-- Record this migration.
------------------------------------------------------------------------

INSERT INTO schema_migrations (version, description)
    VALUES (20, 'ADR 0024: parent_type backfill + Sn-78 poison cleanup');
