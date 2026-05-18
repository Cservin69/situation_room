-- situation_room schema, version 0018.
--
-- Per-record `selector_path` + `raw_bytes_excerpt` provenance columns
-- — Session 87 (the kickoff "613.99 across three metric_kinds" surface).
--
-- ## What this exists to fix
--
-- Session 86 shipped MetricDetailDrawer, which surfaces every Observation
-- in a metric group with `value | when | source | recipe@version |
-- source_url`. That helps the operator see *which recipes contributed
-- to the volatility*, but it doesn't answer the next question: **what
-- did the recipe actually pull from the page?** When three different
-- metric_kinds all render 613.99 the operator wants to know whether
-- all three recipes matched the same DOM scalar (a logo, a footer, the
-- live quote rendered after JavaScript) — and that requires a per-
-- Observation record of the selector that resolved and a short excerpt
-- of the raw bytes the leaf came from.
--
-- Without these columns the only available signal is the recipe id;
-- with them the operator can compare selectors and excerpts across
-- the per-fetch history strip.
--
-- ## Schema shape
--
-- Two nullable TEXT columns added to every record table:
--
--   - `selector_path` — closed-vocabulary tag + selector form.
--     Examples: `"css:#price"`, `"json:$.close"`, `"csv:close@row=3"`,
--     `"pdf:p1/t0/r2/c3"`, `"regex:group=1"`. Iterator-mode recipes
--     stamp `"<iter> >> <inner>"`. Format owned by
--     `crates/pipeline/src/recipe_apply.rs::render_selector_path`.
--   - `raw_bytes_excerpt` — short UTF-8 excerpt of the leaf bytes,
--     capped at `RAW_BYTES_EXCERPT_CAP` codepoints (today 256). The
--     stamper applies the cap with a trailing `"…"` marker; this
--     column stores whatever the stamper produced, no further trim.
--
-- Both columns are NULL for promoted / derived / LLM-synthesized
-- records — recipe_apply is the only writer.
--
-- ## Why columns, not JSON blob
--
-- Existing envelope fields (`source_id`, `source_url`, `license`, …)
-- are flat columns on every record table — same shape, same writer,
-- same reader. Adding the two new fields as columns keeps the storage
-- contract symmetric and lets the MetricDetailDrawer's "distinct
-- selectors / distinct excerpts" counts be derivable with a simple
-- COUNT(DISTINCT …) query if the future warrants it. Storing them in
-- a JSON blob would force `json_extract` lookups for the same reads.
--
-- ## Why every record table, not just observations
--
-- The kickoff surface is Observation-driven, but `recipe_apply` writes
-- the same `Provenance` on every record type it produces (Event,
-- Relation, Assertion via the closed-vocab record-type enum). Adding
-- the columns everywhere keeps `EnvelopeColumns` symmetric and avoids
-- a "this table is special" branch in the storage layer. Entities are
-- materialised at plan-accept time from the classifier's exemplars
-- (not by recipe_apply), so the columns will stay NULL there; that's
-- fine — the column exists for shape uniformity, not because Entity
-- has a recipe selector to stamp.
--
-- ## What's NOT in scope
--
-- - Backfilling existing records' selector_path / raw_bytes_excerpt.
--   The columns are NULL for every pre-Session-87 row; the
--   MetricDetailDrawer treats NULL as "no selector trace available"
--   and falls back to the source_id-only display Session 86 shipped.
-- - Indexing the new columns. The drawer reads at most a few dozen
--   rows per metric group; the existing per-table indexes cover the
--   filter paths.

ALTER TABLE observations ADD COLUMN selector_path        TEXT;
ALTER TABLE observations ADD COLUMN raw_bytes_excerpt    TEXT;

ALTER TABLE events       ADD COLUMN selector_path        TEXT;
ALTER TABLE events       ADD COLUMN raw_bytes_excerpt    TEXT;

ALTER TABLE entities     ADD COLUMN selector_path        TEXT;
ALTER TABLE entities     ADD COLUMN raw_bytes_excerpt    TEXT;

ALTER TABLE relations    ADD COLUMN selector_path        TEXT;
ALTER TABLE relations    ADD COLUMN raw_bytes_excerpt    TEXT;

ALTER TABLE documents    ADD COLUMN selector_path        TEXT;
ALTER TABLE documents    ADD COLUMN raw_bytes_excerpt    TEXT;

ALTER TABLE assertions   ADD COLUMN selector_path        TEXT;
ALTER TABLE assertions   ADD COLUMN raw_bytes_excerpt    TEXT;

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (18, 'provenance selector_path + raw_bytes_excerpt columns');
