-- situation_room schema, version 0023.
--
-- Persisted entity-refresh history — Session 100 candidate #3.
--
-- ## Why this exists
--
-- Sn-99 #4 added `EntityRefreshEvent` + a per-process `VecDeque` ring
-- buffer on `Store` so the dashboard's `EntityRefreshPanel` could show
-- when a tier elevation refreshed an entities row in place. The ring
-- is in-memory only: a Cmd-Q + relaunch wipes the strip, and the
-- "we just refreshed Tesla from `tsla` to `Tesla, Inc.`" signal the
-- panel was added to surface vanishes between sessions.
--
-- Sn-100 #3 follows the Sn-86 promote-history shape: write through to
-- a small history table on every refresh push, hydrate the in-memory
-- ring from disk at boot. The table is bounded by an insert-time prune
-- to `ENTITY_REFRESH_LOG_CAP` rows ordered by `at DESC`, mirroring
-- migration 0017's posture so a million-refresh operator session still
-- ends with `cap` rows on disk.
--
-- ## Why a row-per-event table, not a JSON blob
--
-- The `EntityRefreshEvent` struct is only six fields wide and three of
-- them are closed-vocab (`EntityProvenanceTier` strings). The shape is
-- expected to stay stable — unlike `PromoteReport` (migration 0017),
-- which carries a struct that grows over time. Row-per-event lets a
-- future read query slice by `entity_id` or `new_tier` without parsing
-- JSON; it also keeps the SQL self-documenting at the migration site.
--
-- Closed-vocabulary discipline: `previous_tier` and `new_tier` accept
-- any TEXT but the load path returns a hard error on a string outside
-- `EntityProvenanceTier`'s closed set — same posture migration 0016
-- takes for `outcome_kind` and migration 0017 takes for `trigger`.
--
-- ## What's NOT in scope
--
-- - Cross-binary-session analytics views over refresh activity
--   ("which entities elevated most this week"). The strip is an at-a-
--   glance widget; deeper queries are future work, and the operator
--   can ad-hoc SQL via `duckdb` against the file if needed.
-- - Refresh-event compaction (e.g. fold multiple back-to-back events
--   for the same entity into one). Today's emitter only pushes on
--   strictly-elevating tier changes, so a single entity can fire at
--   most three times across its lifetime (Unknown → RecipeIterator →
--   SlugHumanised → DocumentExtracted). The cap handles operator-
--   session shapes already.
-- - Pre-Sn-100 in-memory entries from a running binary — those are
--   lost on the upgrade boot. Acceptable because the strip is recovery-
--   only (no record-correctness depends on it).
--
-- Columns:
--   id                       UUIDv7 primary key.
--   recorded_at              When the refresh write committed. The
--                            strip orders by this DESC. (Column name
--                            `recorded_at` not `at` — DuckDB's parser
--                            reserves `AT` for `AT TIME ZONE` etc.,
--                            following the precedent migration 0017 set.)
--   entity_id                The business id of the refreshed entity
--                            (the UNIQUE `entities.entity_id` column).
--   previous_canonical_name  `entities.canonical_name` before the refresh.
--   new_canonical_name       `entities.canonical_name` after the refresh.
--   previous_tier            Closed-vocab `EntityProvenanceTier` string
--                            for the pre-refresh row.
--   new_tier                 Closed-vocab `EntityProvenanceTier` string
--                            for the post-refresh row. Always strictly
--                            greater than `previous_tier`.

CREATE TABLE IF NOT EXISTS entity_refresh_history (
    id                      UUID        PRIMARY KEY,
    recorded_at             TIMESTAMPTZ NOT NULL,
    entity_id               TEXT        NOT NULL,
    previous_canonical_name TEXT        NOT NULL,
    new_canonical_name      TEXT        NOT NULL,
    previous_tier           TEXT        NOT NULL,
    new_tier                TEXT        NOT NULL
);

-- The strip's primary read: newest-first within
-- ENTITY_REFRESH_LOG_CAP. The composite index keeps the load-on-boot
-- query at constant cost regardless of historical-row count.
CREATE INDEX IF NOT EXISTS idx_entity_refresh_history_recorded_at
    ON entity_refresh_history(recorded_at DESC);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (23, 'Sn-100 #3: entity_refresh_history table for persisted refresh log');
