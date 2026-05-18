-- Session 92 — measurement: post re-extraction + Option 1 live-verify
-- inspection queries.
--
-- Read-only. Designed to be run BEFORE Option 1 (snapshot), then
-- AFTER the operator clicks `re-extract relations` on a Minecraft-
-- shape plan (the Sn-91 verify target). The before/after diff is the
-- ADR 0023 live-verification signal.
--
-- Run:
--   cd /Users/aben/RustroverProjects/situation_room && \
--     duckdb -readonly situation_room.duckdb < \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session92-measure.sql > \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session92-results.txt 2>&1
--
-- Cross-reference with `session91-results.txt` for the pre-Sn-92
-- baseline (B1 histogram + B2/B3 sample lists).

.timer off
.headers on
.mode column
.maxwidth 80

------------------------------------------------------------------------
-- A. Sanity: total relation Assertions + content_kind sanity.
--    Same baseline shape as session91-measure.sql so the operator
--    can compare row counts directly.
------------------------------------------------------------------------

.print '== A1. assertion counts by content_kind =='
SELECT content_kind, COUNT(*) AS n
FROM assertions
GROUP BY content_kind
ORDER BY n DESC;

.print '== A2. relation assertion total + distinct claimant universe =='
SELECT
    COUNT(*)                                AS n_relation_assertions,
    COUNT(DISTINCT claimant)                AS n_distinct_claimants_total,
    MIN(observed_at)                        AS earliest_observed_at,
    MAX(observed_at)                        AS latest_observed_at
FROM assertions
WHERE content_kind = 'relation';

------------------------------------------------------------------------
-- B. The Sn-91 histogram, refreshed. ADR 0023's live-verification
--    signal is whether the N>=3 bucket grows (or N=2 grows toward
--    N=3) after re-extraction on a plan that has article-shape
--    bytes on disk (the Minecraft path, not the aluminium index-
--    page path Sn-91 verify exposed).
------------------------------------------------------------------------

.print '== B1. distinct-claimants-per-triple HISTOGRAM (all plans) =='
WITH per_triple AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        COUNT(DISTINCT claimant)               AS distinct_claimants
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
)
SELECT
    distinct_claimants,
    COUNT(*) AS n_triples
FROM per_triple
GROUP BY distinct_claimants
ORDER BY distinct_claimants;

.print '== B2. distinct-claimants-per-triple HISTOGRAM (per plan) =='
-- Per-plan view: live-verify is per-plan, so the global B1 above can
-- be dominated by other plans. This view shows the Minecraft-plan
-- shift in isolation. Joins via the assertion source_id LIKE recipe
-- pattern + the plan-keyed shape; matches records_for_plan's join.
WITH plan_rel AS (
    SELECT
        rp.id    AS plan_id,
        rp.topic AS topic,
        a.id     AS assertion_id,
        a.claimant,
        json_extract_string(a.content, '$.kind') AS rel_kind,
        json_extract_string(a.content, '$.from') AS rel_from,
        json_extract_string(a.content, '$.to')   AS rel_to
    FROM assertions a
    JOIN research_plans rp
      ON  a.source_id LIKE 'plan:' || rp.id || '#%'
       OR EXISTS (
              SELECT 1 FROM recipes r
              WHERE r.plan_id = rp.id
                AND a.source_id LIKE '%#recipe:' || r.id || '@v%'
          )
    WHERE a.content_kind = 'relation'
),
per_triple AS (
    SELECT
        plan_id, topic,
        rel_kind, rel_from, rel_to,
        COUNT(DISTINCT claimant) AS distinct_claimants
    FROM plan_rel
    GROUP BY plan_id, topic, rel_kind, rel_from, rel_to
)
SELECT
    topic,
    COUNT(*)                                            AS triples,
    SUM(CASE WHEN distinct_claimants = 1 THEN 1 ELSE 0 END) AS singleton_triples,
    SUM(CASE WHEN distinct_claimants = 2 THEN 1 ELSE 0 END) AS pair_triples,
    SUM(CASE WHEN distinct_claimants >= 3 THEN 1 ELSE 0 END) AS quorum_triples
FROM per_triple
GROUP BY topic
ORDER BY triples DESC;

.print '== B3. multi-claimant SAMPLE (the ADR 0023 success shape) =='
-- Triples that picked up >=2 distinct claimants. If session91-results
-- showed N=1 dominated, this list should be non-empty after a Sn-92
-- re-extract pass on a plan with attribution-bearing article bytes.
WITH per_triple AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        COUNT(DISTINCT claimant)               AS distinct_claimants,
        COUNT(*)                               AS n_assertions,
        LIST(DISTINCT claimant)                AS claimants_list
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
)
SELECT
    rel_kind, rel_from, rel_to,
    distinct_claimants,
    n_assertions,
    claimants_list
FROM per_triple
WHERE distinct_claimants >= 2
ORDER BY distinct_claimants DESC, n_assertions DESC
LIMIT 40;

------------------------------------------------------------------------
-- C. Re-extraction freshness signal: relation Assertions written
--    within the last hour. If the operator just clicked the button,
--    this counter should jump. Distinguishes "the re-extract pass
--    actually wrote rows" from "nothing happened".
------------------------------------------------------------------------

.print '== C1. relation Assertions written in last 1 hour (re-extract freshness) =='
SELECT
    COUNT(*)                                          AS n_recent,
    COUNT(DISTINCT claimant)                          AS n_distinct_claimants_recent,
    COUNT(DISTINCT json_extract_string(content, '$.from') || '|' ||
                   json_extract_string(content, '$.to')) AS n_distinct_triples_recent,
    MIN(observed_at)                                  AS earliest_recent,
    MAX(observed_at)                                  AS latest_recent
FROM assertions
WHERE content_kind = 'relation'
  AND observed_at >= (CURRENT_TIMESTAMP - INTERVAL 1 HOUR);

.print '== C2. last-hour relations grouped by (kind, from, to) =='
-- If the re-extract pass produced multi-claimant rows on the same
-- triple, this query surfaces them: rows with distinct_claimants_recent
-- > 1 are the ADR 0023 success shape on a single Document re-extracted
-- by Sn-92's button.
WITH recent AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        claimant
    FROM assertions
    WHERE content_kind = 'relation'
      AND observed_at >= (CURRENT_TIMESTAMP - INTERVAL 1 HOUR)
)
SELECT
    rel_kind, rel_from, rel_to,
    COUNT(DISTINCT claimant)  AS distinct_claimants_recent,
    COUNT(*)                  AS n_assertions_recent,
    LIST(DISTINCT claimant)   AS claimants_list
FROM recent
GROUP BY rel_kind, rel_from, rel_to
ORDER BY distinct_claimants_recent DESC, n_assertions_recent DESC
LIMIT 40;

------------------------------------------------------------------------
-- D. Promote-pathway visibility — after re-extract, the operator
--    should run promote and see the consensus N=3 quorum (or
--    authoritative N=1) take effect on the new multi-claimant rows.
------------------------------------------------------------------------

.print '== D1. relation rows promoted in last 1 hour (post-promote signal) =='
SELECT
    COUNT(*)                AS n_promoted_relations_recent,
    MIN(observed_at)        AS earliest_recent,
    MAX(observed_at)        AS latest_recent
FROM relations
WHERE observed_at >= (CURRENT_TIMESTAMP - INTERVAL 1 HOUR);

.print '== D2. relations promoted in last 1 hour — claimant fingerprint =='
-- The promote stage stamps claimant = `agency:consensus` for
-- consensus-pathway promotions and `agency:authoritative` for
-- ADR 0004 / authoritative-registry hits. This query surfaces which
-- path the new rows came through.
SELECT
    -- relations table doesn't carry claimant directly; the source_id
    -- carries the synthesised provenance shape. Look for `:consensus`
    -- or `:authoritative` substring as the proxy.
    CASE
        WHEN source_id LIKE '%consensus%'      THEN 'consensus'
        WHEN source_id LIKE '%authoritative%'  THEN 'authoritative'
        ELSE 'other'
    END                AS pathway,
    COUNT(*)           AS n
FROM relations
WHERE observed_at >= (CURRENT_TIMESTAMP - INTERVAL 1 HOUR)
GROUP BY pathway
ORDER BY n DESC;

------------------------------------------------------------------------
-- E. Document corpus snapshot — counts the surface the re-extract
--    button actually iterates. Tells the operator the cost-bound of
--    the next re-extract pass per plan.
------------------------------------------------------------------------

.print '== E1. article-kind document count per plan (re-extract cost preview) =='
WITH plan_docs AS (
    SELECT
        rp.id    AS plan_id,
        rp.topic AS topic,
        d.id     AS doc_id,
        d.mime,
        d.doc_kind,
        LENGTH(d.body) AS body_len
    FROM documents d
    JOIN research_plans rp
      ON  d.source_id LIKE 'plan:' || rp.id || '#%'
       OR EXISTS (
              SELECT 1 FROM recipes r
              WHERE r.plan_id = rp.id
                AND d.source_id LIKE '%#recipe:' || r.id || '@v%'
          )
    -- Match `should_extract_from`: HTML MIME + non-empty body.
    WHERE d.mime LIKE 'text/html%'
      AND LENGTH(d.body) > 0
)
SELECT
    topic,
    COUNT(*)             AS article_docs,
    AVG(body_len)::INT   AS avg_body_chars,
    MAX(body_len)        AS max_body_chars,
    plan_id
FROM plan_docs
GROUP BY topic, plan_id
ORDER BY article_docs DESC;

.print '== END of session92-measure.sql =='
