-- Session 65 — diagnostic: confirm the DB we just opened actually
-- has the data we expect. If these all return zero, the desktop is
-- still running and holding the WAL; quit it first, then re-run.

-- D1: how many rows total?
SELECT 'recipes'                AS table_name, COUNT(*) AS n FROM recipes
UNION ALL
SELECT 'recipe_fetch_attempts', COUNT(*)               FROM recipe_fetch_attempts
UNION ALL
SELECT 'plans',                 COUNT(*)               FROM plans;

-- D2: any rows with reauthor_reason set? (Condition 5 evidence
-- regardless of which specific UUID matched.)
SELECT CAST(id AS VARCHAR)              AS id,
       source_id,
       CAST(prior_recipe_id AS VARCHAR) AS prior_recipe_id,
       reauthor_reason,
       authored_at
FROM recipes
WHERE prior_recipe_id IS NOT NULL
ORDER BY authored_at DESC
LIMIT 10;

-- D3: most recent fetch attempts — does our 019e1ffc apply
-- failure exist by any prefix?
SELECT CAST(recipe_id AS VARCHAR) AS recipe_id,
       succeeded,
       substr(failure_message, 1, 120) AS failure_head,
       attempted_at
FROM recipe_fetch_attempts
ORDER BY attempted_at DESC
LIMIT 10;
