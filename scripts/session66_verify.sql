-- Session 66 — verify Session 65's persistence bug is fixed, then close
-- ADR 0012 Condition 5 by walking the prior_recipe_id chain.
--
-- Usage: duckdb situation_room.duckdb < scripts/session66_verify.sql
--
-- Runbook ordering (operator does, on Mac):
--   1. cargo test --workspace            # picks up Store::checkpoint() tests
--   2. ./scripts/run_desktop.sh          # boot desktop (with the new fix)
--   3. classify a plan in the UI (any topic — Fed rate policy is the
--      Session 64/65 target if you want to combine with Condition 5)
--   4. accept the plan, click "run fetch"
--   5. when run completes, **Ctrl-C the run_desktop.sh terminal** (the
--      path that lost writes pre-fix). The new SIGTERM handler should
--      log "received shutdown signal — checkpointing DuckDB then asking
--      Tauri to exit" before exit.
--   6. duckdb situation_room.duckdb < scripts/session66_verify.sql

------------------------------------------------------------------------
-- Q0: schema_migrations sanity. If this is empty something is very
-- wrong; if max(version) is less than what main expects, the fix didn't
-- land on this binary. Expectation: max(version) >= 16 (Session 46's
-- fetch_run_outcomes migration is the floor for current main).
------------------------------------------------------------------------
SELECT MAX(version) AS max_migration_version,
       COUNT(*)     AS migrations_applied
FROM schema_migrations;

------------------------------------------------------------------------
-- Q1: research_plans roster post-Ctrl-C.
--
-- BEFORE FIX: today's plan would be missing — only May 11 plans
-- (ebola, lithium) plus Cmd-Q-path survivors (atlantic_hurricanes).
--
-- AFTER FIX: today's plan persists. Verify by checking
-- created_at::date for today.
------------------------------------------------------------------------
SELECT CAST(id AS VARCHAR) AS id,
       topic,
       status,
       created_at
FROM research_plans
ORDER BY created_at DESC
LIMIT 10;

------------------------------------------------------------------------
-- Q2: ADR 0012 Condition 5 — prior_recipe_id chain populated in real
-- data. If you classified a Fed plan in step 3 above and clicked
-- re-author on the apply failure, the new recipe's prior_recipe_id
-- should point back to the failed one. Even one non-NULL row closes
-- "verified in a real run" for Condition 5.
------------------------------------------------------------------------
SELECT CAST(id AS VARCHAR)              AS id,
       source_id,
       CAST(prior_recipe_id AS VARCHAR) AS prior_recipe_id,
       SUBSTR(reauthor_reason, 1, 120)  AS reauthor_reason_head,
       authored_at
FROM recipes
WHERE prior_recipe_id IS NOT NULL
ORDER BY authored_at DESC
LIMIT 20;

------------------------------------------------------------------------
-- Q3: Fed-Class-B evidence. If today's run produced an apply failure
-- against `www.federalreserve.gov`, the failure_message + bytes_excerpt
-- here ground the case file. Filter by host substring via
-- recipes.source_url.
------------------------------------------------------------------------
SELECT CAST(rfa.recipe_id AS VARCHAR)      AS recipe_id,
       r.source_url,
       rfa.succeeded,
       SUBSTR(rfa.failure_message, 1, 200) AS failure_message_head,
       length(rfa.bytes_excerpt)           AS n_bytes,
       rfa.attempted_at
FROM recipe_fetch_attempts rfa
JOIN recipes r ON r.id = rfa.recipe_id
WHERE r.source_url LIKE '%federalreserve.gov%'
  AND rfa.succeeded = FALSE
ORDER BY rfa.attempted_at DESC
LIMIT 5;

------------------------------------------------------------------------
-- Q4: Fed-Class-B bytes — the captured response excerpt to paste into
-- the case file's bytes block. Same predicate as Q3, just larger
-- substr.
------------------------------------------------------------------------
SELECT CAST(rfa.recipe_id AS VARCHAR) AS recipe_id,
       SUBSTR(rfa.bytes_excerpt, 1, 1600) AS bytes_head
FROM recipe_fetch_attempts rfa
JOIN recipes r ON r.id = rfa.recipe_id
WHERE r.source_url LIKE '%federalreserve.gov%'
  AND rfa.succeeded = FALSE
ORDER BY rfa.attempted_at DESC
LIMIT 1;
