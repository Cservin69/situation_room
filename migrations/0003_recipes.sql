-- Stockpile schema, version 0003.
--
-- Recipes — the Level-2 output of the research function (ADR 0007).
-- A recipe is an instruction, not a fact: it tells the runtime what
-- URL to fetch, how to extract values, and how to map them into
-- records. Every record produced by the runtime carries a provenance
-- string that includes the recipe id and version, so audits can trace
-- a value all the way back to "this URL, this recipe version, this
-- extraction coordinate".
--
-- Recipes are NOT part of the six record types. They live in their
-- own table because they describe how records were learned, not what
-- the records claim about the world. See ADR 0003 on why the six are
-- closed and ADR 0007 on why recipes sit outside them.
--
-- Identity: UUIDv7 per Stockpile convention. A (plan_id, source_id,
-- binding_tag) dedup_key supports idempotent re-authoring — running
-- Level-2 twice for the same plan × source × binding collides on the
-- key and bumps version rather than creating a parallel recipe.
--
-- Columns:
--   id              UUIDv7 primary key.
--   dedup_key       Optional natural key for idempotent upserts.
--   plan_id         Back-reference to the ResearchPlan.
--   source_id       Registered source id (e.g. "world_bank_indicators").
--   source_url      The URL the runtime fetches.
--   extraction      JSON-encoded ExtractionSpec (closed enum).
--   produces        JSON-encoded Vec<ProductionBinding>.
--   authored_at     When Level-2 produced this recipe.
--   authored_by     Provider id or key fingerprint that authored.
--   version         Monotonic; bumps on semantic re-authoring.

CREATE TABLE IF NOT EXISTS recipes (
    id              UUID PRIMARY KEY,
    dedup_key       TEXT,
    plan_id         UUID NOT NULL,
    source_id       TEXT NOT NULL,
    source_url      TEXT NOT NULL,
    extraction      JSON NOT NULL,
    produces        JSON NOT NULL,
    authored_at     TIMESTAMPTZ NOT NULL,
    authored_by     TEXT NOT NULL,
    version         INTEGER NOT NULL
);

-- Fast lookup by dedup key (NULL-safe: the recipe author ensures
-- non-null dedup_key at the Rust layer when authoring intentionally).
CREATE INDEX IF NOT EXISTS idx_recipes_dedup_key ON recipes(dedup_key);

-- Fast lookup by (plan, source) — the primary query path when the
-- runtime is deciding which recipes apply to a given session.
CREATE INDEX IF NOT EXISTS idx_recipes_plan_source ON recipes(plan_id, source_id);

-- Record this migration.
INSERT INTO schema_migrations (version, description) VALUES (3, 'recipes table');
