-- Session 89 — DATABASE-ONLY failure analysis.
--
-- Read-only diagnostic queries the operator runs on Mac:
--   duckdb situation_room.duckdb < session89-analyze.sql > session89-results.txt
--
-- Every section heading and per-stanza comment matches a choke point in
-- the funnel diagram in SESSION_89_HANDOFF.md. The deliverable for
-- Session 90 is choosing which choke point to widen first.
--
-- No DML; no DDL; this script is safe to re-run.

.timer on
.headers on
.mode column
.maxwidth 80

------------------------------------------------------------------------
-- A. Sanity & scope.
------------------------------------------------------------------------

.print '== A1. schema_migrations (expect 19) =='
SELECT MAX(version) AS schema_version FROM schema_migrations;

.print '== A2. plans on disk =='
SELECT
    id,
    topic,
    classified_by,
    created_at,
    historical_window_days
FROM research_plans
ORDER BY created_at DESC
LIMIT 40;

.print '== A3. row-counts by record table (the "we land with three cards" question, system-wide) =='
SELECT 'observations' AS kind, COUNT(*) AS n FROM observations
UNION ALL SELECT 'events',     COUNT(*) FROM events
UNION ALL SELECT 'entities',   COUNT(*) FROM entities
UNION ALL SELECT 'relations',  COUNT(*) FROM relations
UNION ALL SELECT 'documents',  COUNT(*) FROM documents
UNION ALL SELECT 'assertions', COUNT(*) FROM assertions
ORDER BY n DESC;

.print '== A4. row-counts by infra/audit table =='
SELECT 'recipes'                AS tbl, COUNT(*) AS n FROM recipes
UNION ALL SELECT 'fetch_runs',          COUNT(*) FROM fetch_runs
UNION ALL SELECT 'fetch_run_outcomes',  COUNT(*) FROM fetch_run_outcomes
UNION ALL SELECT 'recipe_fetch_attempts', COUNT(*) FROM recipe_fetch_attempts
UNION ALL SELECT 'recipe_feedback',     COUNT(*) FROM recipe_feedback
ORDER BY n DESC;

------------------------------------------------------------------------
-- B. The federal-reserve plan: identify and characterise.
------------------------------------------------------------------------

.print '== B1. fed plan(s) by topic LIKE — confirm the id Session 89 should target =='
SELECT id, topic, created_at
FROM research_plans
WHERE LOWER(topic) LIKE '%federal reserve%'
   OR LOWER(topic) LIKE '%fed%rate%'
   OR LOWER(topic) LIKE '%fomc%'
ORDER BY created_at DESC;

.print '== B2. fed plan expectations (the classifier nominations per bucket) =='
-- expectations is JSON; pull the bucket counts so the funnel shape is legible.
SELECT
    id,
    topic,
    json_extract_string(expectations, '$.observations_kind') AS obs_kind,
    json_array_length(json_extract(expectations, '$.observations.metrics'))     AS obs_metric_count,
    json_array_length(json_extract(expectations, '$.events.kinds'))             AS event_kind_count,
    json_array_length(json_extract(expectations, '$.entities.kinds'))           AS entity_kind_count,
    json_array_length(json_extract(expectations, '$.relations.kinds'))          AS relation_kind_count,
    json_array_length(json_extract(expectations, '$.documents.kinds'))          AS document_kind_count,
    json_array_length(json_extract(expectations, '$.documents.source_nominations')) AS nomination_count
FROM research_plans
WHERE LOWER(topic) LIKE '%federal reserve%'
   OR LOWER(topic) LIKE '%fed%rate%'
   OR LOWER(topic) LIKE '%fomc%'
ORDER BY created_at DESC;

------------------------------------------------------------------------
-- C. Choke 1 — DECLINE AT AUTHORING (no recipe created).
--    Source: fetch_run_outcomes.outcome_kind = 'declined'
--    Per-bucket source_id shape:
--      nom:<uuid>                          ← nomination-level decline
--      nom:<uuid>:<bucket>:<index>         ← per-expectation decline
------------------------------------------------------------------------

.print '== C1. outcome_kind distribution — system-wide =='
SELECT outcome_kind, COUNT(*) AS n
FROM fetch_run_outcomes
GROUP BY outcome_kind
ORDER BY n DESC;

.print '== C2. fed-plan outcome_kind distribution (replace plan-id at the head if you want a specific one) =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT o.outcome_kind, COUNT(*) AS n
FROM fetch_run_outcomes o, fed
WHERE o.plan_id = fed.id
GROUP BY o.outcome_kind
ORDER BY n DESC;

.print '== C3. fed-plan DECLINE shape by bucket — nomination vs per-expectation =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    CASE
        -- nom:<uuid>:<bucket>:<index>  has 4 ':' parts → per-expectation
        WHEN o.source_id LIKE 'nom:%:%:%' THEN 'per_expectation'
        WHEN o.source_id LIKE 'nom:%'     THEN 'nomination_level'
        ELSE 'other'
    END                                         AS decline_shape,
    -- Slot the bucket index out of the source_id for per-expectation rows.
    -- DuckDB string_split returns LIST<TEXT>; element-3 is the bucket name.
    CASE
        WHEN o.source_id LIKE 'nom:%:%:%'
            THEN split_part(o.source_id, ':', 3)
        ELSE NULL
    END                                         AS bucket,
    COUNT(*)                                    AS n
FROM fetch_run_outcomes o, fed
WHERE o.plan_id = fed.id
  AND o.outcome_kind = 'declined'
GROUP BY decline_shape, bucket
ORDER BY n DESC;

.print '== C4. system-wide DECLINE reasons (top 30 by frequency, head only) =='
SELECT
    SUBSTR(message, 1, 100) AS message_head,
    COUNT(*) AS n
FROM fetch_run_outcomes
WHERE outcome_kind = 'declined'
GROUP BY message_head
ORDER BY n DESC
LIMIT 30;

.print '== C5. fed-plan DECLINE reasons — verbatim message head per row =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    o.source_id,
    SUBSTR(o.message, 1, 160) AS message_head,
    o.attempted_at
FROM fetch_run_outcomes o, fed
WHERE o.plan_id = fed.id
  AND o.outcome_kind = 'declined'
ORDER BY o.attempted_at DESC
LIMIT 80;

------------------------------------------------------------------------
-- D. Choke 2 — RECIPE RUNS BUT FAILS AT APPLY.
--    Source: fetch_run_outcomes where outcome_kind = 'failed' + stage.
------------------------------------------------------------------------

.print '== D1. fed-plan failure shape by stage =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    o.failure_stage,
    SUBSTR(o.message, 1, 80) AS message_head,
    COUNT(*) AS n
FROM fetch_run_outcomes o, fed
WHERE o.plan_id = fed.id
  AND o.outcome_kind = 'failed'
GROUP BY o.failure_stage, message_head
ORDER BY n DESC
LIMIT 40;

.print '== D2. system-wide apply-failure messages (Session-67 + ADR-0012 Class B territory) =='
SELECT
    SUBSTR(message, 1, 100) AS message_head,
    COUNT(*) AS n
FROM fetch_run_outcomes
WHERE outcome_kind = 'failed' AND failure_stage = 'apply'
GROUP BY message_head
ORDER BY n DESC
LIMIT 30;

.print '== D3. recipe_fetch_attempts (bytes captured for apply failures only — Session 13) =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    rfa.recipe_id,
    rfa.succeeded,
    SUBSTR(rfa.failure_message, 1, 100) AS failure_head,
    LENGTH(rfa.bytes_excerpt)            AS excerpt_bytes,
    rfa.attempted_at
FROM recipe_fetch_attempts rfa
JOIN recipes r   ON r.id = rfa.recipe_id
JOIN fed         ON r.plan_id = fed.id
ORDER BY rfa.attempted_at DESC
LIMIT 30;

------------------------------------------------------------------------
-- E. Choke 3 — RECIPE SUCCEEDS BUT EXTRACTS ZERO RECORDS.
--    Source: outcome_kind = 'succeeded' AND records_produced = 0.
------------------------------------------------------------------------

.print '== E1. fed-plan succeeded-but-empty recipes =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    o.recipe_id,
    o.source_id,
    o.records_produced,
    o.attempted_at
FROM fetch_run_outcomes o, fed
WHERE o.plan_id = fed.id
  AND o.outcome_kind = 'succeeded'
ORDER BY o.records_produced ASC, o.attempted_at DESC
LIMIT 40;

------------------------------------------------------------------------
-- F. Choke 4 — ASSERTIONS PERSIST BUT NEVER PROMOTE.
--    Source: assertions table + record_derived_from edges.
--    A promoted record has DerivationRole::ConsensusSupport edges from
--    promoted record → contributing Assertion (consensus pathway), or
--    DerivationRole::Promotion edges → contributing Assertion (auth path).
------------------------------------------------------------------------

.print '== F1. assertions by content_kind — system-wide =='
SELECT content_kind, COUNT(*) AS n
FROM assertions
GROUP BY content_kind
ORDER BY n DESC;

.print '== F2. fed-plan assertions by content_kind (via source_id LIKE recipe-id join) =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
),
fed_recipes AS (
    SELECT id FROM recipes WHERE plan_id IN (SELECT id FROM fed)
)
SELECT a.content_kind, COUNT(*) AS n
FROM assertions a
WHERE EXISTS (
    SELECT 1 FROM fed_recipes fr
    WHERE a.source_id LIKE '%#recipe:' || fr.id || '@v%'
)
   OR a.source_id LIKE 'plan:' || (SELECT id FROM fed) || '#%'
GROUP BY a.content_kind
ORDER BY n DESC;

.print '== F3. assertion claimant diversity per content_kind — quorum visibility =='
-- For promotion to fire at N≥3 we need ≥3 *distinct* claimants on a
-- content_hash-grouped subset. This proxy counts distinct claimants
-- per content_kind, which bounds the achievable consensus group size.
SELECT
    a.content_kind,
    COUNT(*) AS n_assertions,
    COUNT(DISTINCT a.claimant) AS distinct_claimants
FROM assertions a
GROUP BY a.content_kind
ORDER BY distinct_claimants DESC;

.print '== F4. unpromoted-assertion pile — assertions with no outgoing derived_from edge =='
-- A promoted record points back at its contributing Assertion via
-- record_derived_from. Assertions never appearing as a `parent_id`
-- with role ∈ {ConsensusSupport, Promotion} have not been promoted.
SELECT
    a.content_kind,
    COUNT(*) AS unpromoted_n
FROM assertions a
LEFT JOIN record_derived_from rdf
       ON rdf.parent_id = a.id
      AND rdf.parent_type = 'assertion'
      AND rdf.role IN ('consensus_support', 'promotion')
WHERE rdf.parent_id IS NULL
GROUP BY a.content_kind
ORDER BY unpromoted_n DESC;

------------------------------------------------------------------------
-- G. Choke 5 — DOCUMENT SYNTH ran but extraction never fired.
--    Source: documents table vs assertion-extract telemetry.
--    article-kind, non-empty body Documents are the extractor's gate.
------------------------------------------------------------------------

.print '== G1. fed-plan Documents by doc_kind + mime + body-length bucket =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
),
fed_recipes AS (
    SELECT id FROM recipes WHERE plan_id IN (SELECT id FROM fed)
)
SELECT
    d.doc_kind,
    d.mime,
    CASE
        WHEN LENGTH(d.body) = 0          THEN '0'
        WHEN LENGTH(d.body) < 256        THEN '1-255'
        WHEN LENGTH(d.body) < 4096       THEN '256-4k'
        WHEN LENGTH(d.body) < 32768      THEN '4k-32k'
        ELSE '32k+'
    END AS body_size_bucket,
    COUNT(*) AS n
FROM documents d
WHERE EXISTS (
    SELECT 1 FROM fed_recipes fr
    WHERE d.source_id LIKE '%#recipe:' || fr.id || '@v%'
)
   OR d.source_id LIKE 'plan:' || (SELECT id FROM fed) || '#%'
GROUP BY d.doc_kind, d.mime, body_size_bucket
ORDER BY n DESC;

------------------------------------------------------------------------
-- H. Promote-pass observability (ADR 0021 / Session 81-85).
--    Pulls the recent promote-history pointer for the fed plan.
------------------------------------------------------------------------

.print '== H1. promote_history (most recent 10 rows) =='
SELECT *
FROM promote_history
ORDER BY ran_at DESC
LIMIT 10;

------------------------------------------------------------------------
-- I. Recipes by reauthor_reason — the iteration loop (Session 65 / 68).
------------------------------------------------------------------------

.print '== I1. fed-plan recipes with their reauthor_reason head =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
)
SELECT
    r.id,
    r.source_id,
    r.version,
    r.prior_recipe_id IS NOT NULL AS is_reauthor,
    SUBSTR(r.reauthor_reason, 1, 80) AS reason_head,
    r.authored_at
FROM recipes r, fed
WHERE r.plan_id = fed.id
ORDER BY r.authored_at DESC
LIMIT 50;

------------------------------------------------------------------------
-- J. Cross-table per-bucket-index landing report — the headline.
--    For each expectation bucket the classifier produced, count how
--    many records of the matching kind have landed for the fed plan.
------------------------------------------------------------------------

.print '== J1. fed-plan landed-records-per-table (the dashboard numbers) =='
WITH fed AS (
    SELECT id FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
       OR LOWER(topic) LIKE '%fed%rate%'
       OR LOWER(topic) LIKE '%fomc%'
    ORDER BY created_at DESC LIMIT 1
),
fed_recipes AS (
    SELECT id FROM recipes WHERE plan_id IN (SELECT id FROM fed)
),
matches AS (
    SELECT 'observations' AS tbl, o.id, o.source_id FROM observations o
    UNION ALL SELECT 'events',     e.id, e.source_id FROM events e
    UNION ALL SELECT 'entities',   e2.id, e2.source_id FROM entities e2
    UNION ALL SELECT 'relations',  r2.id, r2.source_id FROM relations r2
    UNION ALL SELECT 'documents',  d.id, d.source_id  FROM documents d
    UNION ALL SELECT 'assertions', a.id, a.source_id  FROM assertions a
)
SELECT m.tbl, COUNT(*) AS landed_n
FROM matches m
WHERE EXISTS (
    SELECT 1 FROM fed_recipes fr
    WHERE m.source_id LIKE '%#recipe:' || fr.id || '@v%'
)
   OR m.source_id LIKE 'plan:' || (SELECT id FROM fed) || '#%'
GROUP BY m.tbl
ORDER BY landed_n DESC;

.print '== END of session89-analyze.sql =='
