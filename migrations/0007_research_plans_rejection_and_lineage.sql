-- situation_room schema, version 0007.
--
-- Two columns on `research_plans` for the Session 15 rejection-feedback
-- loop:
--
--   rejection_reason   — free-text note the user attached when
--                        rejecting a plan. NULL on plans that were
--                        rejected before this column existed, on
--                        accepted/pending plans, and on rejections
--                        where the user supplied no note.
--
--   reclassified_from  — UUID linking a re-classified plan back to the
--                        rejected plan that prompted it. NULL on
--                        plans that were not produced by the
--                        re-classify-with-feedback flow.
--
-- These satisfy SESSION 15 P-rejection-loop's storage requirements per
-- the consensus design discussed at the top of the session. The
-- minority-report design (separate `rejection_events` table) was
-- rejected: a `reclassified_from` link reconstructs the chain
-- structurally without introducing a parallel audit table, and the
-- rejection-reason scalar fits cleanly on the row it describes.
--
-- ## DuckDB ALTER TABLE — same lessons as migration 0005
--
-- Per the comment block in `0005_research_plan_status.sql`, DuckDB
-- rejects two paths to a `NOT NULL DEFAULT` ALTER:
--
--   1. `ADD COLUMN ... NOT NULL DEFAULT ...` — "Adding columns with
--      constraints not yet supported".
--   2. `ADD COLUMN ... DEFAULT ...; ALTER COLUMN ... SET NOT NULL;` —
--      "Cannot alter entry because there are entries that depend on
--      it" when indexes exist on the table.
--
-- Both new columns here are nullable by design (NULL = absence of
-- feedback / absence of lineage), which sidesteps the issue entirely.
-- No DEFAULT needed; new rows that omit the column write NULL, which
-- is the correct semantics.
--
-- ## Why TEXT for `reclassified_from` rather than UUID
--
-- DuckDB has a UUID type and the storage crate uses it elsewhere via
-- `params![uuid]` and `row.get::<_, Uuid>(...)`. A UUID column is
-- preferable on read/write ergonomics. **However**, the Session 7
-- experience with column-add-then-alter motivated keeping the new
-- columns as un-typed-on-disk where reasonable. We use UUID here
-- because the storage layer's `set_plan_lineage` and `row_to_stored`
-- code paths consistently round-trip Uuid through duckdb's typed
-- parameter binding; introducing a TEXT column just to avoid one
-- UUID type would force a parse-on-read that the rest of the schema
-- doesn't pay.
--
-- The trade-off vs. SQLite-style flexibility: a future migration
-- that needs to alter this column will face the same DuckDB
-- limitations as 0005 did. We accept that cost for the read-side
-- ergonomic win.

ALTER TABLE research_plans ADD COLUMN rejection_reason TEXT;
ALTER TABLE research_plans ADD COLUMN reclassified_from UUID;

-- Index on `reclassified_from` so "find the descendants of plan X"
-- — used by the lineage-walk on the listing — is one indexed lookup
-- rather than a table scan. Keep partial-index discipline in mind:
-- DuckDB doesn't have partial indexes per se, but a NULL-filtered
-- predicate on a non-NULL column will use a B-tree index efficiently
-- because NULLs collate together.
CREATE INDEX IF NOT EXISTS idx_research_plans_reclassified_from
    ON research_plans(reclassified_from);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (7, 'research_plans: rejection_reason and reclassified_from columns');
