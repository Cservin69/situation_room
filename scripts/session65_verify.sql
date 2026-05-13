-- Session 65 — Condition 5 verification + Class B evidence
-- Usage: duckdb situation_room.duckdb < scripts/session65_verify.sql

-- Q1: prior_recipe_id chain (ADR 0012 Condition 5)
-- Expectation: two rows; the 019e1fff row's prior_recipe_id equals
-- the 019e1ffc row's id, and its reauthor_reason is non-NULL.
SELECT CAST(id AS VARCHAR)              AS id,
       source_id,
       CAST(prior_recipe_id AS VARCHAR) AS prior_recipe_id,
       reauthor_reason,
       authored_at
FROM recipes
WHERE CAST(id AS VARCHAR) LIKE '019e1ffc%'
   OR CAST(id AS VARCHAR) LIKE '019e1fff%'
ORDER BY authored_at;

-- Q2: the failed apply's verbatim failure_message (case-file evidence)
SELECT CAST(recipe_id AS VARCHAR) AS recipe_id,
       succeeded,
       failure_message,
       attempted_at
FROM recipe_fetch_attempts
WHERE CAST(recipe_id AS VARCHAR) LIKE '019e1ffc%'
ORDER BY attempted_at DESC
LIMIT 5;

-- Q3: captured bytes for the failed apply (case-file evidence)
SELECT CAST(recipe_id AS VARCHAR) AS recipe_id,
       length(bytes_excerpt)      AS n_bytes,
       substr(bytes_excerpt, 1, 800) AS head
FROM recipe_fetch_attempts
WHERE CAST(recipe_id AS VARCHAR) LIKE '019e1ffc%'
  AND succeeded = FALSE
ORDER BY attempted_at DESC
LIMIT 1;
