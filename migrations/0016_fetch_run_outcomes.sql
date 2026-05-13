-- situation_room schema, version 0016.
--
-- Per-(run, recipe-or-source) outcome rows — Session 46.
--
-- ## Why this exists
--
-- `fetch_runs` (migration 0006) carries per-run *summary* counters
-- (recipes_attempted, recipes_succeeded, records_produced,
-- error_summary). The per-recipe outcome detail rides synchronously
-- in `FetchReportDto` returned to the UI but **was never persisted**;
-- migration 0006's header comment says exactly that ("Per-recipe
-- failures live in the FetchReportDto returned synchronously and are
-- not persisted at this granularity yet").
--
-- `recipe_fetch_attempts` (migration 0013) writes one row only when a
-- recipe fails at apply stage, capturing the bytes the operator's
-- re-author dialog needs as ground truth. It is not — and was never
-- meant to be — a general per-run outcome log:
--   - It only writes apply-stage failures (`succeeded = false`).
--   - `Declined` outcomes have no `recipe_id` (no recipe was created)
--     and structurally cannot fit a recipe_id-keyed table.
--   - `RateLimited` and fetch-stage timeouts route through their own
--     code path and don't touch `recipe_fetch_attempts` at all.
--
-- The Session-46 recipe-success-heatmap surface needs the missing
-- per-(run, recipe-or-source, outcome_kind) join to render the
-- "is this source flaky or did it just fail once?" question across
-- runs. This table is that join, recorded once per outcome at run
-- completion.
--
-- ## Why a fresh table, not adding columns to `fetch_runs`
--
-- `fetch_runs` is one row per run. Outcomes are N rows per run. The
-- shapes don't match. Stuffing a JSON array into a column would
-- collapse the per-recipe lookup into an unindexed scan and would
-- mirror the same anti-pattern Session 6's handoff explicitly
-- avoided (mixing "what we want to learn" with "what happened").
--
-- ## Why not extend `recipe_fetch_attempts`
--
-- That table's contract is "bytes-and-failure tuple for the manual
-- re-author flow" (`recipe_id` is mandatory). Declined outcomes —
-- the most common failure shape in the Session-45 live run — have
-- no recipe and cannot fit. Reframing the table to make `recipe_id`
-- nullable would change the semantics of every existing read site
-- and ADR 0012 amendment 1 cited above.
--
-- ## Closed-vocabulary discipline
--
-- `outcome_kind` is a closed enum: 'succeeded' | 'skipped' | 'failed'
-- | 'rate_limited' | 'declined' | 'legacy_plan_cannot_author'. The
-- values are the same `RecipeOutcomeDto::kind` strings that already
-- cross the IPC boundary, so the UI's existing tone-mapping helper
-- (`apps/desktop/src/lib/outcomes.ts::outcomeTone`) renders rows from
-- this table identically to live `FetchReportDto` rows. Any seventh
-- outcome kind is an ADR-level decision, just like the six record
-- types or the five extraction modes.
--
-- ## Why two nullable id columns rather than one polymorphic
--
-- Five of the six outcome kinds carry both `recipe_id` and
-- `source_id`; `declined` and `legacy_plan_cannot_author` carry
-- `source_id` only. Modeling this as two columns where `recipe_id`
-- is NULL on the no-recipe variants is honest about the shape: a
-- query joining to `recipes` simply LEFT JOINs on `recipe_id`, and
-- the heatmap groups by `COALESCE(recipe_id, source_id)`. The
-- alternative — a synthetic "subject_id" column with a sibling
-- `subject_kind` discriminator — would obscure the join semantics
-- the natural shape already provides.
--
-- ## What's NOT in scope
--
-- - Backfill of historical outcomes. Pre-migration runs land in
--   `fetch_runs` with their summary counters but no per-outcome
--   rows; the heatmap renders columns sparsely for those runs (no
--   cells). The handoff before this session predicted this.
-- - Per-(recipe, run) bytes excerpts for *successful* runs. That is
--   `recipe_fetch_attempts`'s territory and is still gated on the
--   "what bytes produced this record" panel earning its weight.
--
-- Columns:
--   id                  UUIDv7 primary key.
--   run_id              Back-reference to the fetch run.
--   plan_id             Denormalized — the run's plan. Lets the
--                       per-plan history query stay a single SQL
--                       without joining `fetch_runs`.
--   recipe_id           NULL for declined / legacy outcomes (no
--                       recipe was authored); a UUID otherwise.
--   source_id           Always populated. The source the outcome
--                       attaches to. Same string the rest of the
--                       codebase uses (e.g. 'pubs.usgs.gov',
--                       'nom:<uuid>').
--   outcome_kind        Closed enum — see above.
--   records_produced    For 'succeeded': the record count. NULL for
--                       every other variant (no records were
--                       written).
--   retry_after_seconds For 'rate_limited' with a parsed
--                       Retry-After: the seconds value. NULL
--                       otherwise.
--   failure_stage       For 'failed': 'fetch' | 'apply' | 'insert'.
--                       NULL otherwise.
--   message             For 'failed': the failure message. For
--                       'skipped' / 'declined': the LLM/executor
--                       reason. NULL for 'succeeded' /
--                       'rate_limited' / 'legacy_plan_cannot_author'
--                       (the kind itself carries the meaning).
--   attempted_at        When the outcome was recorded. Mirrors
--                       `fetch_runs.finished_at` for the parent run
--                       in normal operation; carried per-row so the
--                       heatmap can sort cells by recording time
--                       without a join.

CREATE TABLE IF NOT EXISTS fetch_run_outcomes (
    id                  UUID        PRIMARY KEY,
    run_id              UUID        NOT NULL,
    plan_id             UUID        NOT NULL,
    recipe_id           UUID,
    source_id           TEXT        NOT NULL,
    outcome_kind        TEXT        NOT NULL,
    records_produced    INTEGER,
    retry_after_seconds BIGINT,
    failure_stage       TEXT,
    message             TEXT,
    attempted_at        TIMESTAMPTZ NOT NULL
);

-- The heatmap's primary read: every outcome for one plan, ordered
-- by recording time so the per-run columns line up.
CREATE INDEX IF NOT EXISTS idx_fetch_run_outcomes_plan_attempted_at
    ON fetch_run_outcomes(plan_id, attempted_at DESC);

-- The per-run lookup: every outcome inside one run, e.g. for
-- replaying the report panel from storage rather than from
-- in-memory state.
CREATE INDEX IF NOT EXISTS idx_fetch_run_outcomes_run_id
    ON fetch_run_outcomes(run_id);

-- Per-recipe lookup: every outcome a single recipe has produced
-- across runs. The heatmap doesn't need this directly (it groups
-- per row instead), but a future "trace a recipe across its life"
-- view would, and the index is cheap.
--
-- DuckDB doesn't support partial indexes (`WHERE recipe_id IS NOT
-- NULL`); the regular B-tree below is the right shape. NULLs collate
-- together at one end of the index so a NULL-excluding query reads
-- contiguously past them — same posture migration 0007 documents
-- for `research_plans.reclassified_from`.
CREATE INDEX IF NOT EXISTS idx_fetch_run_outcomes_recipe_id
    ON fetch_run_outcomes(recipe_id);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (16, 'fetch_run_outcomes table');
