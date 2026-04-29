-- Indexes for the hot query paths.
--
-- UUIDv7 primary keys already give chronological ordering, so time-range
-- scans over a record table do not need a separate observed_at index
-- when `id DESC` suffices. We still add observed_at indexes for the
-- panel query pattern "newest records first in this subject window",
-- which filters before ordering.
--
-- DuckDB creates indexes as min/max zone maps (like ART indexes in newer
-- versions) — they accelerate equality and range scans; they aren't
-- B-tree indexes in the Postgres sense.

------------------------------------------------------------------------
-- Dedup keys
------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_observations_dedup_key
    ON observations (dedup_key);
CREATE INDEX IF NOT EXISTS idx_events_dedup_key
    ON events (dedup_key);
CREATE INDEX IF NOT EXISTS idx_relations_dedup_key
    ON relations (dedup_key);
CREATE INDEX IF NOT EXISTS idx_documents_dedup_key
    ON documents (dedup_key);
CREATE INDEX IF NOT EXISTS idx_assertions_dedup_key
    ON assertions (dedup_key);

------------------------------------------------------------------------
-- Observation/event time windowing per source
------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_observations_source_observed
    ON observations (source_id, observed_at);
CREATE INDEX IF NOT EXISTS idx_events_source_observed
    ON events (source_id, observed_at);

------------------------------------------------------------------------
-- Entity business-key lookup
--
-- entity_id is the stable identifier other records reference. Unique
-- because duplicate entities with the same entity_id would break those
-- references.
------------------------------------------------------------------------

CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_entity_id
    ON entities (entity_id);
CREATE INDEX IF NOT EXISTS idx_entities_kind
    ON entities (kind);

------------------------------------------------------------------------
-- Junction-table joins
------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_subjects_entities_entity
    ON record_subjects_entities (entity_id, record_type);
CREATE INDEX IF NOT EXISTS idx_subjects_topics_topic
    ON record_subjects_topics (topic, record_type);
CREATE INDEX IF NOT EXISTS idx_subjects_places_kind
    ON record_subjects_places (place_kind, record_type);

------------------------------------------------------------------------
-- Derivation chain traversal (both directions)
------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_derived_child
    ON record_derived_from (child_id);
CREATE INDEX IF NOT EXISTS idx_derived_parent
    ON record_derived_from (parent_id);

------------------------------------------------------------------------
-- Document retention
------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_document_retention_tier
    ON document_retention (tier);

------------------------------------------------------------------------
-- Mark this migration as applied.
------------------------------------------------------------------------

INSERT INTO schema_migrations (version, description)
VALUES (2, 'indexes for dedup, observed_at, entity business keys, junction joins, derivation');
