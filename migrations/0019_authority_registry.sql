-- Migration 0019: authority_registry table
--
-- Session 88 (Sn-87 candidate 2) — scaffold for the DB-backed
-- authoritative-source registry. ADR 0022 documents the migration
-- plan from `config/vocab/authoritative_sources.toml` to this table.
--
-- ## What this lands NOW
--
-- The table + indexes. The boot path still loads the TOML and the
-- runtime registry (`LiveAuthorityRegistry` / `PromoteConfig.
-- authoritative`) is unchanged in Session 88: this migration creates
-- the durable home, the next session populates it from the TOML on
-- first boot and switches the runtime read to the DB. Two-stage
-- migration so the scaffolding lands without changing operator-
-- visible behaviour.
--
-- ## Why a table not JSON-on-disk
--
-- ADR 0022 records the rationale; the short version is that the
-- promote stage runs a fan-out of `(source_id, metric, topic)` lookups
-- per pass and a relational filter is cheaper + more legible than
-- O(N) scans of an in-memory Vec for each lookup. The DB is also the
-- single source of truth the rest of the schema lives in; the TOML
-- becomes a bootstrap/seeding artefact rather than a runtime config
-- surface.

CREATE TABLE IF NOT EXISTS authority_registry (
    id                  UUID PRIMARY KEY,
    source_id           TEXT NOT NULL,
    metric              TEXT,
    topic               TEXT,
    consensus_quorum    INTEGER,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Closed-vocab provenance tag: how this row got into the table.
    -- 'toml_seed' = imported from authoritative_sources.toml at boot.
    -- 'operator' = added by an operator (future TUI / CLI surface).
    -- Mirrors the closed-vocab discipline in the rest of the schema
    -- (see ADR 0017 / project_sr_no_source_routing).
    provenance          TEXT NOT NULL DEFAULT 'toml_seed'
);

-- Looking up by (source_id, metric, topic) is the promote stage's
-- fan-out. The composite index sized for the most-selective leading
-- column matches the existing in-memory lookup order.
CREATE INDEX IF NOT EXISTS idx_authority_registry_source
    ON authority_registry (source_id);
CREATE INDEX IF NOT EXISTS idx_authority_registry_source_metric
    ON authority_registry (source_id, metric);
CREATE INDEX IF NOT EXISTS idx_authority_registry_source_topic
    ON authority_registry (source_id, topic);
