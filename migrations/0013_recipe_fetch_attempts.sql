-- situation_room schema, version 0013.
--
-- ADR 0012 amendment 1 (Track A, Session 25/26): capture the bytes
-- that triggered a recipe's last apply-stage failure, so the manual
-- re-author command can author the next recipe against ground truth
-- rather than against a fresh pre-fetch (which the source may have
-- changed shape on by then).
--
-- ## Why a sibling table, not a column on `fetch_runs` or `recipes`
--
-- Three distinct lifecycles:
--
--   - `fetch_runs` is per-run: one row per "user clicked Run fetch".
--     A run produces N recipe outcomes; storing per-recipe bytes
--     there would force a fan-out the schema doesn't otherwise allow.
--   - `recipes` is per-version: one row per authoring event, with
--     `prior_recipe_id` for the lineage. Stuffing bytes there would
--     tie storage to authoring, when the bytes belong to execution.
--   - `recipe_fetch_attempts` is per-(recipe, run) — the natural
--     join of "which recipe ran, when, and what bytes did it see."
--     It is the only place an attempt's bytes-and-failure tuple
--     lives coherently.
--
-- ## Truncation discipline
--
-- The `bytes_excerpt` column holds the first MAX_EXCERPT_BYTES of
-- whatever the runtime fetched. The Rust write path
-- (`Store::insert_recipe_fetch_attempt`) is responsible for truncating
-- to bytes (UTF-8 char-boundary aware), and the Rust load path
-- treats the column as "head of the bytes, may be partial." The cap
-- is 64 KiB — documented in
-- `crates/storage/src/recipe_fetch_attempts.rs` as
-- `MAX_EXCERPT_BYTES`. The number balances: large enough that a
-- typical regex/CSS author reviewing the excerpt sees the response
-- shape (a 30-line RSS feed fits; a small JSON API response fits);
-- small enough that storage doesn't bloat from runs against API
-- responses that vary in shape.
--
-- The excerpt is stored as TEXT, not BLOB — the runtime's primary
-- supported sources are text (HTML, JSON, CSV, RSS). PDF is handled
-- separately by `static_payload` (ADR 0007 Amendment 3). A future
-- binary-bytes case can add a sibling BLOB column without migrating
-- this one. UTF-8 is enforced by the write path (lossy decode), so
-- the column is always valid UTF-8.
--
-- ## Why not capture every attempt
--
-- Today only failed-apply bytes are captured (the operator's
-- audit-the-failure use case). Capturing every successful fetch's
-- bytes would 10x the table size for a use case that doesn't yet
-- exist (a "what bytes produced this record" trace would be useful
-- for ADR 0007's traceability promise, but the existing record's
-- provenance string already names the source; the bytes themselves
-- are over-capture). When that case earns its weight, this same
-- table can absorb the additional rows by changing the write path —
-- the schema doesn't need to change.
--
-- ## ADR 0009 implications
--
-- Captured bytes are response payloads from sources the operator
-- chose to fetch. They never contain `Authorization` headers (that's
-- request-side; the SecureHttpClient guarantees nothing of the
-- request crosses into the response body). Storing them at rest in
-- the single-user desktop database is consistent with ADR 0009's
-- threat model (single-tenant, local).

CREATE TABLE IF NOT EXISTS recipe_fetch_attempts (
    id              UUID        PRIMARY KEY,
    recipe_id       UUID        NOT NULL,
    run_id          UUID        NOT NULL,
    attempted_at    TIMESTAMPTZ NOT NULL,
    succeeded       BOOLEAN     NOT NULL,
    failure_message TEXT,
    bytes_excerpt   TEXT
);

-- Compound index for the common read: latest attempt per recipe.
-- DESC on attempted_at means the index already orders the result the
-- way the query wants it.
CREATE INDEX IF NOT EXISTS recipe_fetch_attempts_recipe_id_attempted_at_idx
    ON recipe_fetch_attempts(recipe_id, attempted_at DESC);

-- Sibling index for "all attempts in this run" — useful in the
-- inspection panel and in any future "compare across runs" view.
CREATE INDEX IF NOT EXISTS recipe_fetch_attempts_run_id_idx
    ON recipe_fetch_attempts(run_id);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (13, 'recipe_fetch_attempts table');
