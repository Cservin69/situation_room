-- Situation_room schema, version 0001.
--
-- One table per record type. Envelope columns are flat (id, dedup_key,
-- provenance, timestamps, confidence) so the hot query paths are columnar.
-- Content shapes vary per record type and are stored as JSON.
--
-- Subject dimensions (entities, places, topics) live in junction tables
-- because they are many-to-many and drive filter queries. The optional
-- time-scope dimension is small (0 or 1 per record) and stays inline.
-- Tags are a flat JSON array — filter use is secondary, junction overkill.
--
-- All timestamps are UTC. UUIDs stored as UUID (DuckDB native).

------------------------------------------------------------------------
-- Migrations bookkeeping
------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    description TEXT NOT NULL
);

------------------------------------------------------------------------
-- Record tables
--
-- Column conventions (common envelope):
--   id              UUIDv7 primary key, chronologically orderable.
--   dedup_key       Optional natural key for idempotent upserts.
--   source_id       Registered source id (e.g. "usgs_mcs").
--   source_url      Optional URL the record was fetched from.
--   source_published_at  Optional source-reported publication time.
--   license         License string ("public_domain", "fair_use", etc.).
--   tags            JSON array of "key:value" strings.
--   subject_time    JSON-encoded Option<TimeScope>, null when absent.
--   observed_at     When situation_room learned about this record.
--   valid_at        When the claim was true in the world (optional).
--   confidence      Float in [0.0, 1.0].
------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS observations (
    -- Envelope
    id                      UUID PRIMARY KEY,
    dedup_key               TEXT,
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL,
    -- Content
    content                 JSON NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id                      UUID PRIMARY KEY,
    dedup_key               TEXT,
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL,
    content                 JSON NOT NULL
);

CREATE TABLE IF NOT EXISTS entities (
    id                      UUID PRIMARY KEY,
    -- Entity-specific (not an envelope field, but structural)
    entity_id               TEXT NOT NULL,
    kind                    TEXT NOT NULL,
    canonical_name          TEXT NOT NULL,
    geometry                JSON,
    -- Envelope
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL
    -- NOTE: entities do not carry a dedup_key at the envelope level;
    -- the business key is entity_id, enforced via a unique index below.
);

CREATE TABLE IF NOT EXISTS relations (
    id                      UUID PRIMARY KEY,
    dedup_key               TEXT,
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL,
    content                 JSON NOT NULL
);

CREATE TABLE IF NOT EXISTS documents (
    id                      UUID PRIMARY KEY,
    dedup_key               TEXT,
    -- Document-specific
    title                   TEXT,
    doc_kind                TEXT NOT NULL,
    mime                    TEXT NOT NULL,
    body                    TEXT NOT NULL,
    published_at            TIMESTAMPTZ,
    author                  TEXT,
    -- Envelope
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS assertions (
    id                      UUID PRIMARY KEY,
    dedup_key               TEXT,
    -- Assertion-specific
    claimant                TEXT NOT NULL,
    stance                  TEXT NOT NULL,
    content_kind            TEXT NOT NULL,      -- discriminator of AssertedContent
    content                 JSON NOT NULL,
    -- Envelope
    source_id               TEXT NOT NULL,
    source_url              TEXT,
    source_published_at     TIMESTAMPTZ,
    license                 TEXT NOT NULL,
    tags                    JSON NOT NULL DEFAULT '[]',
    subject_time            JSON,
    observed_at             TIMESTAMPTZ NOT NULL,
    valid_at                TIMESTAMPTZ,
    confidence              REAL NOT NULL
);

------------------------------------------------------------------------
-- Subjects junction tables
--
-- One row per (record, subject) pair. The record_type column lets a
-- single junction table span all six record-type tables without
-- duplication.
--
-- Values:
--   record_type ∈ {"observation","event","entity","relation","document","assertion"}
--
-- Kept as plain tables (not foreign-keyed to the record tables) because
-- DuckDB's cross-table FK enforcement is limited, and because the
-- pipeline inserts records and subjects in the same transaction.
-- Integrity is enforced by the storage layer, not by constraints.
------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS record_subjects_entities (
    record_id       UUID NOT NULL,
    record_type     TEXT NOT NULL,
    entity_id       TEXT NOT NULL,
    PRIMARY KEY (record_id, entity_id)
);

CREATE TABLE IF NOT EXISTS record_subjects_places (
    record_id       UUID NOT NULL,
    record_type     TEXT NOT NULL,
    -- PlaceRef is an enum (Country/Region/Point); we store it as JSON
    -- and also pull out a kind tag for cheap filtering.
    place_kind      TEXT NOT NULL,     -- "country" | "region" | "point"
    place_value     JSON NOT NULL,
    PRIMARY KEY (record_id, place_kind, place_value)
);

CREATE TABLE IF NOT EXISTS record_subjects_topics (
    record_id       UUID NOT NULL,
    record_type     TEXT NOT NULL,
    topic           TEXT NOT NULL,
    PRIMARY KEY (record_id, topic)
);

------------------------------------------------------------------------
-- Derived-from chain
--
-- An edge in the provenance DAG. A record can reference multiple
-- parents (a consensus-promoted Observation points at the N Assertions
-- that made consensus).
--
-- role ∈ {"extraction","promotion","aggregation","correction"}
-- matching DerivationRole in the schema.
------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS record_derived_from (
    child_id        UUID NOT NULL,
    child_type      TEXT NOT NULL,
    parent_id       UUID NOT NULL,
    parent_type     TEXT NOT NULL,
    role            TEXT NOT NULL,
    PRIMARY KEY (child_id, parent_id, role)
);

------------------------------------------------------------------------
-- Cache/archive classification (ADR 0008)
--
-- Documents default to `cache`; user engagement (open, pin, annotate)
-- promotes them to `archive` which is never auto-evicted. Implemented
-- as a sidecar table so the core document row stays immutable.
------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS document_retention (
    document_id     UUID PRIMARY KEY,
    tier            TEXT NOT NULL,     -- "cache" | "archive"
    classified_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

------------------------------------------------------------------------
-- Record this migration as applied.
------------------------------------------------------------------------

INSERT INTO schema_migrations (version, description)
VALUES (1, 'initial schema: six record tables + subjects + derived-from + retention');
