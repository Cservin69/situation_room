-- Session 89 — analyze v2.
--
-- Fixes from v1:
--   1. Topic LIKE picks the *newest* fed plan (019e313e) which has 0
--      outcomes — the operator's screenshot is from an older fed plan.
--      Stop using "ORDER BY created_at DESC LIMIT 1"; loop the three.
--   2. H1: promote_history column is not `ran_at`. Inspect schema.
--   3. B2: expectations JSON keys are different than guessed. Inspect.
--   4. Add: assertion claimant breakdown — F3 said entity_attribute has
--      **1 distinct claimant for 107 rows**; we need to see who.
--
-- Three fed plans:
--   019e313e-55b4-7e91-a147-6cc93b8dcfe9  16:43:49 (newest; 0 outcomes)
--   019e313d-eccb-7202-b7b5-f5d86ac8ad6f  16:43:22 (27s before)
--   019e20b3-7c87-7191-ad0a-8b53cb42e632  May 13   (older)

.timer off
.headers on
.mode column
.maxwidth 80

------------------------------------------------------------------------
-- K. Schema introspection — what columns actually exist.
------------------------------------------------------------------------

.print '== K1. promote_history schema =='
DESCRIBE promote_history;

.print '== K2. one promote_history row (most recent by rowid) =='
SELECT * FROM promote_history LIMIT 3;

.print '== K3. expectations JSON shape — actual top-level keys =='
SELECT
    id,
    topic,
    json_keys(expectations) AS top_level_keys
FROM research_plans
WHERE LOWER(topic) LIKE '%federal reserve%'
LIMIT 3;

.print '== K4. expectations JSON full (newest fed plan) =='
SELECT expectations::TEXT AS expectations_text
FROM research_plans
WHERE id = '019e313e-55b4-7e91-a147-6cc93b8dcfe9';

------------------------------------------------------------------------
-- L. Outcomes per fed plan id (correct join, no LIMIT 1 truncation).
------------------------------------------------------------------------

.print '== L1. outcomes per fed plan id =='
SELECT
    rp.id,
    rp.topic,
    rp.created_at,
    SUM(CASE WHEN o.outcome_kind='succeeded'    THEN 1 ELSE 0 END) AS succeeded,
    SUM(CASE WHEN o.outcome_kind='declined'     THEN 1 ELSE 0 END) AS declined,
    SUM(CASE WHEN o.outcome_kind='failed'       THEN 1 ELSE 0 END) AS failed,
    SUM(CASE WHEN o.outcome_kind='rate_limited' THEN 1 ELSE 0 END) AS rate_limited,
    SUM(CASE WHEN o.outcome_kind='skipped'      THEN 1 ELSE 0 END) AS skipped,
    COUNT(o.id)                                                    AS total
FROM research_plans rp
LEFT JOIN fetch_run_outcomes o ON o.plan_id = rp.id
WHERE LOWER(rp.topic) LIKE '%federal reserve%'
GROUP BY rp.id, rp.topic, rp.created_at
ORDER BY rp.created_at DESC;

.print '== L2. recipes per fed plan id =='
SELECT
    rp.id,
    rp.topic,
    COUNT(r.id) AS recipe_n
FROM research_plans rp
LEFT JOIN recipes r ON r.plan_id = rp.id
WHERE LOWER(rp.topic) LIKE '%federal reserve%'
GROUP BY rp.id, rp.topic
ORDER BY rp.id;

.print '== L3. fed plan landed-records-per-table — per fed plan id =='
WITH fed AS (
    SELECT id, topic, created_at FROM research_plans
    WHERE LOWER(topic) LIKE '%federal reserve%'
),
fed_recipes AS (
    SELECT plan_id, id AS recipe_id FROM recipes
    WHERE plan_id IN (SELECT id FROM fed)
),
matches AS (
    SELECT 'observations' AS tbl, source_id FROM observations
    UNION ALL SELECT 'events',     source_id FROM events
    UNION ALL SELECT 'entities',   source_id FROM entities
    UNION ALL SELECT 'relations',  source_id FROM relations
    UNION ALL SELECT 'documents',  source_id FROM documents
    UNION ALL SELECT 'assertions', source_id FROM assertions
)
SELECT
    fed.id      AS plan_id,
    fed.created_at::DATE AS created,
    m.tbl,
    COUNT(*)    AS landed
FROM fed
JOIN matches m ON
    m.source_id LIKE 'plan:' || fed.id || '#%'
    OR EXISTS (
        SELECT 1 FROM fed_recipes fr
        WHERE fr.plan_id = fed.id
          AND m.source_id LIKE '%#recipe:' || fr.recipe_id || '@v%'
    )
GROUP BY fed.id, fed.created_at, m.tbl
ORDER BY fed.created_at DESC, landed DESC;

------------------------------------------------------------------------
-- M. The unpromoted-pile — claimants, content, dedup_keys.
------------------------------------------------------------------------

.print '== M1. entity_attribute assertions — who is the single claimant? =='
SELECT
    claimant,
    COUNT(*) AS n,
    COUNT(DISTINCT dedup_key) AS distinct_dedup_keys,
    MIN(observed_at)::DATE AS first_seen,
    MAX(observed_at)::DATE AS last_seen
FROM assertions
WHERE content_kind = 'entity_attribute'
GROUP BY claimant
ORDER BY n DESC;

.print '== M2. entity_attribute content shape — what attributes are claimed? =='
SELECT
    json_extract_string(content, '$.entity_id')           AS entity_id,
    json_extract_string(content, '$.attribute')           AS attribute,
    COUNT(*)                                              AS n
FROM assertions
WHERE content_kind = 'entity_attribute'
GROUP BY entity_id, attribute
ORDER BY n DESC
LIMIT 30;

.print '== M3. relation assertions — claimant diversity per relation kind =='
SELECT
    json_extract_string(content, '$.kind')                 AS rel_kind,
    COUNT(*)                                               AS n_assertions,
    COUNT(DISTINCT claimant)                               AS distinct_claimants,
    COUNT(DISTINCT dedup_key)                              AS distinct_dedup_keys
FROM assertions
WHERE content_kind = 'relation'
GROUP BY rel_kind
ORDER BY n_assertions DESC;

.print '== M4. relation assertions — subject-object-predicate spread =='
SELECT
    json_extract_string(content, '$.subject.entity_id')    AS subj,
    json_extract_string(content, '$.kind')                 AS predicate,
    json_extract_string(content, '$.object.entity_id')     AS obj,
    COUNT(*)                                               AS n_claims,
    COUNT(DISTINCT claimant)                               AS distinct_claimants
FROM assertions
WHERE content_kind = 'relation'
GROUP BY subj, predicate, obj
ORDER BY distinct_claimants DESC, n_claims DESC
LIMIT 30;

------------------------------------------------------------------------
-- N. Fed plan deep dive — for the *right* plan id (operator's screenshot
--    is probably 019e313d; we'll see which has the 1 obs / 3 ev shape).
------------------------------------------------------------------------

.print '== N1. all three fed plans — declines by decline_shape =='
WITH fed AS (
    SELECT id, created_at::DATE AS created
    FROM research_plans WHERE LOWER(topic) LIKE '%federal reserve%'
)
SELECT
    fed.id,
    fed.created,
    CASE
        WHEN o.source_id LIKE 'nom:%:%:%' THEN 'per_expectation'
        WHEN o.source_id LIKE 'nom:%'     THEN 'nomination_level'
        WHEN o.source_id LIKE '%#recipe:%' THEN 'recipe_keyed'
        ELSE 'other'
    END                                AS decline_shape,
    CASE
        WHEN o.source_id LIKE 'nom:%:%:%' THEN split_part(o.source_id, ':', 3)
        ELSE NULL
    END                                AS bucket,
    COUNT(*)                           AS n
FROM fed
JOIN fetch_run_outcomes o ON o.plan_id = fed.id
WHERE o.outcome_kind = 'declined'
GROUP BY fed.id, fed.created, decline_shape, bucket
ORDER BY fed.id, n DESC;

.print '== N2. all three fed plans — decline message heads =='
WITH fed AS (
    SELECT id, created_at::DATE AS created
    FROM research_plans WHERE LOWER(topic) LIKE '%federal reserve%'
)
SELECT
    fed.id,
    SUBSTR(o.message, 1, 110) AS message_head,
    COUNT(*) AS n
FROM fed
JOIN fetch_run_outcomes o ON o.plan_id = fed.id
WHERE o.outcome_kind = 'declined'
GROUP BY fed.id, message_head
ORDER BY fed.id, n DESC
LIMIT 100;

.print '== N3. all three fed plans — failed-stage shape =='
WITH fed AS (
    SELECT id FROM research_plans WHERE LOWER(topic) LIKE '%federal reserve%'
)
SELECT
    fed.id,
    o.failure_stage,
    SUBSTR(o.message, 1, 80) AS message_head,
    COUNT(*) AS n
FROM fed
JOIN fetch_run_outcomes o ON o.plan_id = fed.id
WHERE o.outcome_kind = 'failed'
GROUP BY fed.id, o.failure_stage, message_head
ORDER BY fed.id, n DESC;

------------------------------------------------------------------------
-- O. Promote history (fixed).
------------------------------------------------------------------------

.print '== O1. promote_history full rows (capped) =='
SELECT * FROM promote_history LIMIT 20;

------------------------------------------------------------------------
-- P. Outcome shape by source — who declines the most.
------------------------------------------------------------------------

.print '== P1. system-wide decline source_id prefixes =='
SELECT
    CASE
        WHEN source_id LIKE 'nom:%:%:%' THEN 'nom:_:_:_ (per-expectation)'
        WHEN source_id LIKE 'nom:%'     THEN 'nom:_ (nomination-level)'
        WHEN source_id LIKE '%#recipe:%' THEN 'recipe-keyed'
        ELSE 'other'
    END AS shape,
    COUNT(*) AS n
FROM fetch_run_outcomes
WHERE outcome_kind = 'declined'
GROUP BY shape
ORDER BY n DESC;

.print '== P2. per-expectation decline by bucket — system-wide =='
SELECT
    split_part(source_id, ':', 3) AS bucket,
    COUNT(*) AS n
FROM fetch_run_outcomes
WHERE outcome_kind = 'declined'
  AND source_id LIKE 'nom:%:%:%'
GROUP BY bucket
ORDER BY n DESC;

.print '== END of session89-analyze-v2.sql =='
