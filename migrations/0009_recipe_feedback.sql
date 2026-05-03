-- situation_room schema, version 0009.
--
-- ADR 0013: per-(plan_id, source_id) operator feedback channel.
--
-- The operator flags a recipe in the inspection panel with a free-text
-- note explaining what is wrong. The note persists per (plan, source)
-- pair, survives recipe re-authoring (recipes rotate by version on the
-- same dedup_key; feedback is about the source's behaviour for this
-- plan, not about a specific recipe row), and is fed back to the LLM
-- the next time recipe-authoring runs for that pair via the v1.8
-- recipe-author prompt's `{{RECIPE_FEEDBACK}}` placeholder.
--
-- See `docs/adr/0013-recipe-feedback-channel.md` for the design
-- rationale (key-by-(plan, source); overwrite-on-set; separation from
-- failure cases; reuse of the classifier's fenced-render security
-- pattern with a per-call UUID nonce).
--
-- ## Why a fresh table, not an ALTER on `recipes`
--
-- Per migration 0005's comment block (since echoed in 0007 and 0008),
-- DuckDB rejects `ADD COLUMN ... NOT NULL DEFAULT ...` outright and
-- rejects the split `ADD COLUMN ... DEFAULT ...` + `ALTER COLUMN ...
-- SET NOT NULL` when the table has indexes on it. The `recipes` table
-- has the `(plan_id, source_id)` index from migration 0003, so an
-- ALTER would hit the trap.
--
-- A fresh CREATE TABLE sidesteps the issue entirely. It is also the
-- correct shape on the merits — feedback is keyed differently from
-- recipes (no recipe_id; no version; one row per (plan, source) where
-- recipes have many rows per (plan, source) over time), and modeling
-- it as a sibling table avoids smearing two unrelated lifecycles into
-- one row.
--
-- ## Why no `id` column
--
-- The natural key `(plan_id, source_id)` is the primary key. A surrogate
-- `id` (UUIDv7) would only serve to give us a stable handle for child
-- tables that don't exist. If a `recipe_feedback_history` table ever
-- earns its weight (per ADR 0013 §"When to amend or supersede"), it
-- can carry its own surrogate id and FK to (plan_id, source_id) — at
-- which point the keying choice still survives. There is no current
-- need.
--
-- ## Why no FK to `research_plans` or to the source-id catalog
--
-- DuckDB has FK enforcement but the rest of the schema does not use
-- it: `recipes.plan_id` does not declare a foreign key against
-- `research_plans.id`, and the storage crate has stayed deliberately
-- agnostic of cross-table referential integrity, relying on the Rust
-- layer to keep relationships consistent. We follow that posture here.
-- A flagged plan that is later soft-deleted (status=rejected) keeps
-- its feedback rows; the listing path filters by selected plan, so
-- orphans are invisible until a future cleanup pass earns its keep.

CREATE TABLE IF NOT EXISTS recipe_feedback (
    plan_id    UUID         NOT NULL,
    source_id  TEXT         NOT NULL,
    note       TEXT         NOT NULL,
    created_at TIMESTAMPTZ  NOT NULL,
    PRIMARY KEY (plan_id, source_id)
);

-- Index on plan_id alone for the listing path
-- (`recipe_feedback_for_plan(plan_id)`). The PK already covers
-- point lookups by (plan_id, source_id); the standalone plan_id index
-- handles the "all feedback for this plan" query that the inspection
-- panel issues on plan selection.
CREATE INDEX IF NOT EXISTS idx_recipe_feedback_plan
    ON recipe_feedback(plan_id);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (9, 'recipe_feedback table');
