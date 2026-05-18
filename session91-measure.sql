-- Session 91 — measurement: relation-Assertion claimant fragment shape.
--
-- Read-only. Run BEFORE landing the Sn-91 code changes; the output
-- frames ADR 0023's path choice (extraction-time diversity vs lower
-- N vs operator-curated registry).
--
-- Question: per (relation kind, from, to) triple, how many distinct
-- claimants exist today? The answer tells us how often Path A1
-- (multi-claimant extraction) actually unlocks consensus.
--
-- Run:
--   cd /Users/aben/RustroverProjects/situation_room && \
--     duckdb -readonly situation_room.duckdb < \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session91-measure.sql > \
--       /Users/aben/Documents/Claude/Projects/SituationRoom/session91-results.txt 2>&1
--
-- Notes on JSON paths:
--   `assertions.content` is the serde-tagged JSON of `AssertedContent`.
--   For Relation: `{"asserted_kind":"relation","kind":"<predicate>",
--                   "from":"<entity-id>","to":"<entity-id>",...}`.
--   EntityId serializes as a plain string, NOT `{entity_id:"..."}`.
--   (Session 89's M4 used `$.subject.entity_id` — that path is wrong
--   for relation content; this file corrects it.)

.timer off
.headers on
.mode column
.maxwidth 80

------------------------------------------------------------------------
-- A. Sanity: total relation Assertions + content_kind sanity.
------------------------------------------------------------------------

.print '== A1. assertion counts by content_kind =='
SELECT content_kind, COUNT(*) AS n
FROM assertions
GROUP BY content_kind
ORDER BY n DESC;

.print '== A2. one relation row — wire-shape spot-check =='
SELECT content::TEXT AS content_json,
       claimant
FROM assertions
WHERE content_kind = 'relation'
LIMIT 3;

------------------------------------------------------------------------
-- B. Per-triple distinct-claimant distribution (the core question).
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

.print '== B2. triples at N=1 (singletons) — the Path A target =='
WITH per_triple AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        COUNT(DISTINCT claimant)               AS distinct_claimants,
        COUNT(*)                               AS n_assertions
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
)
SELECT
    rel_kind, rel_from, rel_to,
    distinct_claimants,
    n_assertions
FROM per_triple
WHERE distinct_claimants = 1
ORDER BY n_assertions DESC
LIMIT 40;

.print '== B3. triples at N=2 (one more claimant = consensus) =='
WITH per_triple AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        COUNT(DISTINCT claimant)               AS distinct_claimants,
        COUNT(*)                               AS n_assertions
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
)
SELECT
    rel_kind, rel_from, rel_to,
    distinct_claimants,
    n_assertions
FROM per_triple
WHERE distinct_claimants = 2
ORDER BY n_assertions DESC
LIMIT 40;

.print '== B4. triples already at N>=3 (would already promote) =='
WITH per_triple AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        COUNT(DISTINCT claimant)               AS distinct_claimants,
        COUNT(*)                               AS n_assertions
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
)
SELECT
    rel_kind, rel_from, rel_to,
    distinct_claimants,
    n_assertions
FROM per_triple
WHERE distinct_claimants >= 3
ORDER BY distinct_claimants DESC, n_assertions DESC
LIMIT 40;

------------------------------------------------------------------------
-- C. Per-relation-kind summary (matches Sn-90 handoff's table).
------------------------------------------------------------------------

.print '== C1. relation kind summary: assertions, triples, distinct claimants =='
SELECT
    json_extract_string(content, '$.kind') AS rel_kind,
    COUNT(*)                               AS n_assertions,
    COUNT(DISTINCT (
        json_extract_string(content, '$.from') || '|' ||
        json_extract_string(content, '$.to')
    ))                                     AS n_distinct_triples,
    COUNT(DISTINCT claimant)               AS n_distinct_claimants_system_wide,
    MAX(claimants_per_triple)              AS max_claimants_any_triple
FROM (
    SELECT
        content,
        claimant,
        COUNT(DISTINCT claimant) OVER (PARTITION BY
            json_extract_string(content, '$.kind'),
            json_extract_string(content, '$.from'),
            json_extract_string(content, '$.to')
        ) AS claimants_per_triple
    FROM assertions
    WHERE content_kind = 'relation'
)
GROUP BY rel_kind
ORDER BY n_assertions DESC;

------------------------------------------------------------------------
-- D. Per-plan breakdown — does claimant fragmentation track to plans?
------------------------------------------------------------------------

.print '== D1. relations per plan + per-triple claimant ceiling =='
WITH rel AS (
    SELECT
        rp.id    AS plan_id,
        rp.topic AS topic,
        rp.created_at::DATE AS created,
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
        plan_id, topic, created,
        rel_kind, rel_from, rel_to,
        COUNT(DISTINCT claimant) AS distinct_claimants,
        COUNT(*)                 AS n_assertions
    FROM rel
    GROUP BY plan_id, topic, created, rel_kind, rel_from, rel_to
)
SELECT
    plan_id, topic, created,
    COUNT(*)                                AS triples,
    SUM(CASE WHEN distinct_claimants = 1 THEN 1 ELSE 0 END) AS singleton_triples,
    SUM(CASE WHEN distinct_claimants = 2 THEN 1 ELSE 0 END) AS pair_triples,
    SUM(CASE WHEN distinct_claimants >= 3 THEN 1 ELSE 0 END) AS quorum_triples,
    SUM(n_assertions)                       AS total_assertions
FROM per_triple
GROUP BY plan_id, topic, created
ORDER BY total_assertions DESC;

------------------------------------------------------------------------
-- E. Source-document overlap — would Path A1 actually find multiple
--    claimants? For each singleton triple, look at the source document
--    and check: does the document body reference >=1 other agency /
--    company / publisher by canonical prefix shape ("agency:..." or
--    "company:...")? This is a rough upper bound on Path A1's yield
--    without actually re-running the LLM extractor.
--
--    NOTE: Documents are matched via the assertion's source_id (which
--    points to the recipe / source). The body_preview lives on
--    `documents.body` (Session 69). We sample, not exhaustively scan.
------------------------------------------------------------------------

.print '== E1. singleton-triple → document body preview spot-check (10 rows) =='
WITH singletons AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        MIN(a.id)                              AS sample_assertion_id,
        MIN(a.source_id)                       AS sample_source_id,
        COUNT(DISTINCT claimant)               AS distinct_claimants
    FROM assertions a
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
    HAVING distinct_claimants = 1
),
docs AS (
    -- Join singletons to documents that share the same recipe-routed
    -- source_id. Documents are stored 1:1 per fetch (Session 69), and
    -- Assertions carry the same `{src}#recipe:{id}@v{ver}` shape, so
    -- this LIKE join is exact on the recipe identifier.
    SELECT
        s.rel_kind, s.rel_from, s.rel_to,
        s.sample_assertion_id,
        d.source_url,
        SUBSTR(d.body, 1, 240) AS body_head
    FROM singletons s
    LEFT JOIN documents d
      ON d.source_id = s.sample_source_id
)
SELECT * FROM docs
ORDER BY rel_kind, rel_from, rel_to
LIMIT 10;

.print '== E2. cited-claimant prefix counts in document bodies (rough proxy) =='
-- Count how many distinct `agency:` or `company:` prefix-shaped tokens
-- appear in *any* document body that produced a relation Assertion.
-- This is a *very* rough upper bound on Path A1's yield: an LLM with
-- the v1.2 prompt could in principle pull each of these as a cited
-- claimant. Path A1's actual yield will be lower because not every
-- prefix-shaped token names an entity that already exists in the
-- topic's vocabulary.
SELECT
    COUNT(DISTINCT d.id)                                              AS docs_with_relation_assertions,
    AVG(LENGTH(d.body))                                               AS avg_body_chars,
    AVG((LENGTH(d.body) - LENGTH(REPLACE(d.body, 'agency:', ''))) / 7.0)  AS avg_agency_mentions,
    AVG((LENGTH(d.body) - LENGTH(REPLACE(d.body, 'company:', ''))) / 8.0) AS avg_company_mentions
FROM documents d
WHERE EXISTS (
    SELECT 1 FROM assertions a
    WHERE a.content_kind = 'relation'
      AND a.source_id = d.source_id
);

------------------------------------------------------------------------
-- F. Path B counter-check — if we instead curated registry entries,
--    how many singleton claimants would we need to add to unlock all
--    the singleton triples?
------------------------------------------------------------------------

.print '== F1. distinct singleton-claimants (Path B curation cost) =='
WITH singletons AS (
    SELECT
        json_extract_string(content, '$.kind') AS rel_kind,
        json_extract_string(content, '$.from') AS rel_from,
        json_extract_string(content, '$.to')   AS rel_to,
        MIN(claimant)            AS sole_claimant,
        COUNT(DISTINCT claimant) AS distinct_claimants
    FROM assertions
    WHERE content_kind = 'relation'
    GROUP BY rel_kind, rel_from, rel_to
    HAVING distinct_claimants = 1
)
SELECT
    sole_claimant,
    COUNT(*) AS triples_held_back
FROM singletons
GROUP BY sole_claimant
ORDER BY triples_held_back DESC;

.print '== END of session91-measure.sql =='
