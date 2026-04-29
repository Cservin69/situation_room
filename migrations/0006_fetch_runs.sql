-- Stockpile schema, version 0006.
--
-- Fetch runs — one row per invocation of the Phase-6 fetch executor
-- against an accepted plan. See SESSION 8.
--
-- Each row is opened (started_at populated, finished_at NULL) when
-- the executor begins work for a plan, and updated on completion
-- with the per-run counters and an optional top-level error summary.
-- The row is the audit trail: the user can look at any plan and see
-- "we ran the executor against this on $started_at; out of $attempted
-- recipes, $succeeded produced $produced records." Per-recipe outcome
-- detail is carried in the FetchReportDto returned to the UI in the
-- same call; the row is the persisted summary, not the per-recipe log.
--
-- Why a fresh table, not adding columns to an existing one: ADR 0007
-- separates plans from runs deliberately. A plan describes intent and
-- is immutable except for status; a run is an event in time and
-- writes summary numbers. Mixing them would conflate "what we want
-- to learn" with "what happened the last time we tried", and the
-- accept/reject flow already relies on the plan row being write-rare.
-- Sidesteps the DuckDB ALTER trap entirely (see migration 0005's
-- comment block — `ADD COLUMN ... NOT NULL DEFAULT ...` is rejected,
-- and the split form fails when indexes exist on the table).
--
-- Why the counter columns are NOT NULL with DEFAULT 0 and the
-- nullable columns are not: the counters are always meaningful (a
-- run that crashes before processing anything still has zeros for
-- each of these), and DuckDB accepts NOT NULL on freshly-created
-- columns inside a CREATE TABLE statement — the trap from migration
-- 0005 is specifically about ALTER TABLE ADD COLUMN, not about
-- table creation. `started_at` is non-nullable because we set it
-- when we open the row; `finished_at` and `error_summary` are
-- nullable because they're populated on completion.
--
-- Columns:
--   id                     UUIDv7 primary key.
--   plan_id                Back-reference to the ResearchPlan being run.
--   started_at             When the executor began work on this run.
--   finished_at            When work completed (NULL while in flight).
--   recipes_attempted      How many recipes the executor tried to apply.
--   recipes_succeeded      How many of those produced ≥1 record.
--   records_produced       Total records produced across all recipes.
--   error_summary          Top-level error if the run failed before
--                          processing any recipe (e.g. plan not found,
--                          plan not accepted, recipe authoring failed
--                          wholesale). Per-recipe failures live in the
--                          FetchReportDto returned synchronously and
--                          are not persisted at this granularity yet.

CREATE TABLE IF NOT EXISTS fetch_runs (
    id                  UUID PRIMARY KEY,
    plan_id             UUID NOT NULL,
    started_at          TIMESTAMPTZ NOT NULL,
    finished_at         TIMESTAMPTZ,
    recipes_attempted   INTEGER NOT NULL DEFAULT 0,
    recipes_succeeded   INTEGER NOT NULL DEFAULT 0,
    records_produced    INTEGER NOT NULL DEFAULT 0,
    error_summary       TEXT
);

-- The primary read path is "show me the recent runs for this plan,
-- newest first." A composite index on (plan_id, started_at DESC)
-- covers it without scanning the whole table.
CREATE INDEX IF NOT EXISTS idx_fetch_runs_plan_started_at
    ON fetch_runs(plan_id, started_at DESC);

-- Record this migration.
INSERT INTO schema_migrations (version, description) VALUES (6, 'fetch_runs table');
