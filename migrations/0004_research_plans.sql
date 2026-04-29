-- Stockpile schema, version 0004.
--
-- Research plans — the Level-1 output of the research function (ADR 0007).
-- A plan is the structured classification of a free-text topic into the
-- Stockpile vocabulary: which record types to populate, which sources to
-- nominate, what the geographic and temporal scope is, and the
-- interpretation paragraph the user reviews before fetching begins.
--
-- A plan is the single source of truth for a research session. Every
-- recipe authored by Level-2 carries a plan_id back-reference (see
-- `recipes.plan_id`); records produced by those recipes inherit the
-- plan's topic_tags via the standard subjects-topics path. So the plan
-- id is the spine of session-scoped traceability.
--
-- Plans are NOT part of the six record types — same reasoning as for
-- recipes (ADR 0003, ADR 0007). They live in their own table because
-- they describe how a session is shaped, not what records claim about
-- the world.
--
-- Identity: UUIDv7 per Stockpile convention. No dedup_key — re-running
-- classification for the same topic legitimately produces a new plan
-- (the LLM may classify the same topic differently across sessions, the
-- existing-topics injection might steer it differently, the prompt may
-- have advanced). Plans are immutable once written.
--
-- Columns:
--   id                     UUIDv7 primary key.
--   topic                  User's verbatim topic string (server-stamped).
--   interpretation         LLM's one-paragraph trust-moment restatement.
--   topic_tags             JSON array of canonical Topic strings.
--   geographic_scope       JSON array (ISO codes or region descriptors).
--   historical_window_days How far back ingestion should reach.
--   expectations           JSON-encoded RecordExpectations.
--   created_at             When classification produced this plan.
--   classified_by          Provider id (e.g. "xai") that classified.

CREATE TABLE IF NOT EXISTS research_plans (
    id                      UUID PRIMARY KEY,
    topic                   TEXT NOT NULL,
    interpretation          TEXT NOT NULL,
    topic_tags              JSON NOT NULL,
    geographic_scope        JSON NOT NULL,
    historical_window_days  INTEGER NOT NULL,
    expectations            JSON NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL,
    classified_by           TEXT NOT NULL
);

-- Fast lookup by created_at for session listings ("show me my recent plans").
CREATE INDEX IF NOT EXISTS idx_research_plans_created_at ON research_plans(created_at DESC);

-- Fast lookup by topic for "have I researched this before?" queries.
CREATE INDEX IF NOT EXISTS idx_research_plans_topic ON research_plans(topic);

-- Record this migration.
INSERT INTO schema_migrations (version, description) VALUES (4, 'research_plans table');
