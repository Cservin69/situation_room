-- session93-measure.sql — read-only measurement queries for Sn-93's
-- index-page detector + cull pass + per-Document re-extract + relation-
-- vocab gate. Mirrors session92-measure.sql's shape; sections re-used
-- from Sn-92 stay numerically comparable so the operator can diff
-- pre / post across sessions.
--
-- USAGE:
--   cd /Users/aben/RustroverProjects/situation_room && \
--     duckdb -readonly situation_room.duckdb < \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session93-measure.sql > \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session93-results.txt 2>&1
--
-- Or via the verify runbook which captures pre/post for Stage 4.

.headers on
.mode column

-- A. Assertion sanity — total counts unchanged unless Sn-93's cull
-- pass ran. Matches Sn-92 A1+A2 numerically so the operator can
-- correlate.
SELECT '== A1. assertion counts by content_kind ==' AS section;
SELECT content_kind, COUNT(*) AS n
FROM assertions
GROUP BY content_kind
ORDER BY 1;

SELECT '== A2. relation assertion total + distinct claimant universe ==' AS section;
SELECT COUNT(*)                        AS n_relation_assertions,
       COUNT(DISTINCT claimant)        AS n_distinct_claimants_total,
       MIN(observed_at)                AS earliest_observed_at,
       MAX(observed_at)                AS latest_observed_at
FROM assertions
WHERE content_kind = 'relation';

-- B. Distinct-claimants-per-triple histogram. Sn-91 baseline was
-- 7 singletons (aluminium) + 2 pair triples (minecraft). After
-- Sn-93's cull (if operator ran it), the aluminium 7 should be
-- gone; the minecraft 2 should stay. Per-plan breakout matches
-- Sn-92 B2's shape so trends are comparable.
SELECT '== B1. distinct-claimants-per-triple HISTOGRAM (all plans) ==' AS section;
WITH triple_universe AS (
  SELECT json_extract_string(content, '$.kind')   AS rel_kind,
         json_extract_string(content, '$.from')   AS rel_from,
         json_extract_string(content, '$.to')     AS rel_to,
         claimant
  FROM assertions
  WHERE content_kind = 'relation'
)
SELECT n_distinct_claimants, COUNT(*) AS n_triples
FROM (
  SELECT rel_kind, rel_from, rel_to,
         COUNT(DISTINCT claimant) AS n_distinct_claimants
  FROM triple_universe
  GROUP BY rel_kind, rel_from, rel_to
) t
GROUP BY n_distinct_claimants
ORDER BY 1;

SELECT '== B2. distinct-claimants-per-triple HISTOGRAM (per plan) ==' AS section;
-- Per-plan breakout: join via topic_tags (the records_for_plan
-- discriminator). Reads Provenance off the assertion row and the
-- plan's topic_tags off research_plans.plan_json.
WITH triple_universe AS (
  SELECT json_extract_string(content, '$.kind')   AS rel_kind,
         json_extract_string(content, '$.from')   AS rel_from,
         json_extract_string(content, '$.to')     AS rel_to,
         claimant,
         source_id
  FROM assertions
  WHERE content_kind = 'relation'
),
per_plan AS (
  SELECT rp.id AS plan_id,
         json_extract_string(rp.plan_json, '$.topic') AS topic,
         rel_kind, rel_from, rel_to,
         COUNT(DISTINCT claimant) AS n_distinct_claimants
  FROM triple_universe t
  CROSS JOIN research_plans rp
  -- Match via the canonical source_id `…#recipe:{uuid}@v{ver}` shape
  -- to plans whose recipes own the recipe uuid in the middle. This
  -- mirrors records_for_plan's LIKE-join.
  WHERE EXISTS (
    SELECT 1 FROM recipes r
    WHERE r.plan_id = rp.id
      AND t.source_id LIKE '%#recipe:' || r.id || '@%'
  )
  GROUP BY rp.id, topic, rel_kind, rel_from, rel_to
)
SELECT topic,
       COUNT(*)                                                    AS triples,
       SUM(CASE WHEN n_distinct_claimants = 1 THEN 1 ELSE 0 END)   AS singleton_triples,
       SUM(CASE WHEN n_distinct_claimants = 2 THEN 1 ELSE 0 END)   AS pair_triples,
       SUM(CASE WHEN n_distinct_claimants >= 3 THEN 1 ELSE 0 END)  AS quorum_triples
FROM per_plan
GROUP BY topic
ORDER BY triples DESC;

SELECT '== B3. multi-claimant SAMPLE (the ADR 0023 success shape) ==' AS section;
WITH triple_universe AS (
  SELECT json_extract_string(content, '$.kind')   AS rel_kind,
         json_extract_string(content, '$.from')   AS rel_from,
         json_extract_string(content, '$.to')     AS rel_to,
         claimant
  FROM assertions
  WHERE content_kind = 'relation'
)
SELECT rel_kind, rel_from, rel_to,
       COUNT(DISTINCT claimant) AS distinct_claimants,
       COUNT(*) AS n_assertions,
       list(DISTINCT claimant) AS claimants_list
FROM triple_universe
GROUP BY rel_kind, rel_from, rel_to
HAVING COUNT(DISTINCT claimant) >= 2
ORDER BY distinct_claimants DESC, n_assertions DESC
LIMIT 10;

-- C. Last-hour relation Assertion freshness (the re-extract write
-- signal). After Stage 4 of the verify runbook, this should show
-- a non-zero count whose recent-distinct-claimants per triple ≥2.
SELECT '== C1. relation Assertions written in last 1 hour (re-extract freshness) ==' AS section;
SELECT COUNT(*)                                AS n_recent,
       COUNT(DISTINCT claimant)                AS n_distinct_claimants_recent,
       COUNT(DISTINCT json_extract_string(content, '$.kind') ||
                      json_extract_string(content, '$.from') ||
                      json_extract_string(content, '$.to'))
                                               AS n_distinct_triples_recent,
       MIN(observed_at)                        AS earliest_recent,
       MAX(observed_at)                        AS latest_recent
FROM assertions
WHERE content_kind = 'relation'
  AND observed_at >= (now() - INTERVAL 1 HOUR);

SELECT '== C2. last-hour relations grouped by (kind, from, to) ==' AS section;
WITH recent AS (
  SELECT json_extract_string(content, '$.kind')   AS rel_kind,
         json_extract_string(content, '$.from')   AS rel_from,
         json_extract_string(content, '$.to')     AS rel_to,
         claimant
  FROM assertions
  WHERE content_kind = 'relation'
    AND observed_at >= (now() - INTERVAL 1 HOUR)
)
SELECT rel_kind, rel_from, rel_to,
       COUNT(DISTINCT claimant) AS distinct_claimants_recent,
       COUNT(*) AS n_assertions_recent,
       list(DISTINCT claimant) AS claimants_list
FROM recent
GROUP BY rel_kind, rel_from, rel_to
ORDER BY distinct_claimants_recent DESC, n_assertions_recent DESC
LIMIT 20;

-- D. Promoted relations in last hour. After operator clicks promote,
-- this surfaces the (kind, from, to) triples that reached the
-- consensus quorum (or matched an authoritative source). If Stage 4
-- of the verify runbook produced multi-claimant rows AND the
-- operator clicked promote, this should be non-zero.
SELECT '== D1. relation rows promoted in last 1 hour (post-promote signal) ==' AS section;
SELECT COUNT(*) AS n_promoted_relations_recent,
       MIN(observed_at) AS earliest_recent,
       MAX(observed_at) AS latest_recent
FROM relations
WHERE observed_at >= (now() - INTERVAL 1 HOUR);

-- D2. Pathway fingerprint — which promote pathway produced each
-- recent promoted relation. `authoritative` ⇒ N=1 fast-track,
-- `consensus` ⇒ N=3 quorum hit, anything else ⇒ pre-Sn-82 promote.
SELECT '== D2. relations promoted in last 1 hour — claimant fingerprint ==' AS section;
SELECT CASE
         WHEN license = 'authoritative' THEN 'authoritative'
         WHEN license = 'consensus'     THEN 'consensus'
         ELSE 'other'
       END AS pathway,
       COUNT(*) AS n
FROM relations
WHERE observed_at >= (now() - INTERVAL 1 HOUR)
GROUP BY pathway
ORDER BY n DESC;

-- E. Article-kind Document count per plan — pre-trigger cost preview
-- for the re-extract pass. Matches Sn-92 E1 numerically.
SELECT '== E1. article-kind document count per plan (re-extract cost preview) ==' AS section;
SELECT json_extract_string(rp.plan_json, '$.topic')        AS topic,
       COUNT(*)                                            AS article_docs,
       CAST(AVG(LENGTH(d.body)) AS INTEGER)                AS avg_body_chars,
       MAX(LENGTH(d.body))                                 AS max_body_chars,
       rp.id                                               AS plan_id
FROM documents d
JOIN research_plans rp ON EXISTS (
  SELECT 1 FROM recipes r
  WHERE r.plan_id = rp.id
    AND d.source_id LIKE '%#recipe:' || r.id || '@%'
)
WHERE d.kind = 'article'
  AND LENGTH(d.body) > 0
GROUP BY topic, plan_id
ORDER BY article_docs DESC
LIMIT 20;

-- F. Session 93 specific — Assertion + Document co-occurrence by
-- source_id. The cull pass groups Assertions by source_id and joins
-- to the latest Document under that key. F1 surfaces the per-source
-- Assertion-vs-Document pile so the operator can see which sources
-- would feed the structural detector at cull time.
SELECT '== F1. per source_id: assertion count + most-recent document presence ==' AS section;
WITH src_docs AS (
  SELECT source_id, MAX(observed_at) AS latest_doc_at, COUNT(*) AS n_docs
  FROM documents
  WHERE kind = 'article'
    AND LENGTH(body) > 0
  GROUP BY source_id
),
src_assertions AS (
  SELECT source_id, content_kind, COUNT(*) AS n_assertions
  FROM assertions
  GROUP BY source_id, content_kind
)
SELECT sa.source_id,
       sa.content_kind,
       sa.n_assertions,
       sd.n_docs                                              AS docs_for_source_id,
       sd.latest_doc_at                                       AS latest_doc_at
FROM src_assertions sa
LEFT JOIN src_docs sd USING (source_id)
WHERE sa.n_assertions >= 1
ORDER BY sa.n_assertions DESC, sa.source_id
LIMIT 40;

-- G. Session 93 specific — relation-predicate vocabulary drift surface.
-- Counts relation Assertions whose `kind` is NOT in the owning plan's
-- declared relation_kinds[] list. After the Sn-93 pipeline gate
-- ships, this should stay flat (the gate drops drift before persist).
-- Spike here = the upstream gate isn't catching everything; debug at
-- the LLM-schema layer first.
SELECT '== G1. relation predicate vocab drift (kind NOT in plan.relation_kinds) ==' AS section;
WITH plan_predicates AS (
  SELECT rp.id AS plan_id,
         unnest(
           json_extract(rp.plan_json, '$.expectations.relation_kinds[*].kind')
         ) AS declared_kind
  FROM research_plans rp
  WHERE json_extract(rp.plan_json, '$.expectations.relation_kinds') IS NOT NULL
),
plan_recipe AS (
  SELECT r.plan_id, r.id AS recipe_id
  FROM recipes r
),
rel_with_plan AS (
  SELECT a.id                                          AS assertion_id,
         json_extract_string(a.content, '$.kind')      AS rel_kind,
         pr.plan_id
  FROM assertions a
  JOIN plan_recipe pr ON a.source_id LIKE '%#recipe:' || pr.recipe_id || '@%'
  WHERE a.content_kind = 'relation'
)
SELECT rwp.plan_id,
       json_extract_string(rp.plan_json, '$.topic')   AS topic,
       rwp.rel_kind                                   AS unexpected_kind,
       COUNT(*)                                       AS n_assertions
FROM rel_with_plan rwp
JOIN research_plans rp ON rp.id = rwp.plan_id
WHERE NOT EXISTS (
  SELECT 1 FROM plan_predicates pp
  WHERE pp.plan_id = rwp.plan_id
    AND TRIM(BOTH '"' FROM pp.declared_kind) = rwp.rel_kind
)
GROUP BY rwp.plan_id, topic, rwp.rel_kind
ORDER BY n_assertions DESC
LIMIT 20;

SELECT '== END of session93-measure.sql ==' AS section;
