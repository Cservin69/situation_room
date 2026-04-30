-- situation_room schema, version 0005.
--
-- Soft-delete-via-status for research plans. See SESSION 7 P1.
--
-- A plan moves through three states:
--
--   pending   — newly classified, awaiting user review (the default).
--   accepted  — user reviewed and approved. The future Phase-6 fetch
--               executor will consume only plans in this state.
--   rejected  — user discarded. The row stays for audit so we can
--               answer "did we ever try X" without a delete trail,
--               but it's hidden from default views.
--
-- Why a column rather than a lifecycle table: the state machine is
-- small (3 states, 2 transitions in either direction) and a single
-- column round-trips cleanly through the existing storage helpers.
-- A separate `plan_events` table buys us nothing until/unless we
-- want to record *when* and *by whom* the transition happened — and
-- this is a single-user desktop app (handoff §"explicitly NOT"), so
-- there's no actor to record. If multi-user lands later, that's
-- when the lifecycle table earns its weight.
--
-- Why TEXT rather than an enum/CHECK constraint: DuckDB's enum
-- support is per-table and changing the allowed set later is a
-- migration, not a code change. Validation happens in Rust at the
-- storage boundary via `PlanStatus::FromStr` — that's where the
-- error message can name the bad value usefully.
--
-- Backfill: `DEFAULT 'pending'` populates existing rows. Plans
-- classified before Session 7 land in pending and the user re-
-- triages. That's the right default — it forces a deliberate review
-- of every pre-existing plan rather than silently promoting them.
--
-- Why no NOT NULL constraint: DuckDB rejects two paths to it.
-- `ADD COLUMN ... NOT NULL DEFAULT ...` in one statement fails with
-- "Adding columns with constraints not yet supported". Splitting
-- into `ADD COLUMN ... DEFAULT ...` followed by `ALTER COLUMN ...
-- SET NOT NULL` fails with "Cannot alter entry because there are
-- entries that depend on it" — the existing v4 indexes on
-- `created_at` and `topic` block any column-level alter. Dropping
-- and recreating those indexes for the sake of a NOT NULL on a
-- column with a DEFAULT is heavier machinery than the constraint
-- buys us, because:
--
--   1. The `status` column is only ever written by Rust code that
--      takes a typed `PlanStatus` by value (`insert_research_plan`,
--      `set_plan_status`). There is no code path that can write
--      NULL.
--   2. The DEFAULT covers any future SQL path that omits the column
--      on INSERT.
--   3. A surprise NULL on read would fail `PlanStatus::from_str` and
--      surface as `StorageError::Other` with the column value in
--      the message — visible failure, not silent corruption. Same
--      as any other unknown status string would.
--
-- The Rust-side invariant is the load-bearing one. NOT NULL would
-- have been belt-and-suspenders, and the suspenders fight DuckDB.

ALTER TABLE research_plans ADD COLUMN status TEXT DEFAULT 'pending';

-- Belt-and-suspenders for any DuckDB version where `ADD COLUMN ...
-- DEFAULT` doesn't backfill existing rows. No-op when the DEFAULT
-- already did the work.
UPDATE research_plans SET status = 'pending' WHERE status IS NULL;

-- The listing's filtered query is "newest pending plans" by default;
-- this composite index covers it without scanning the whole table.
CREATE INDEX IF NOT EXISTS idx_research_plans_status_created_at
    ON research_plans(status, created_at DESC);

-- Record this migration.
INSERT INTO schema_migrations (version, description) VALUES (5, 'research_plans.status column');
