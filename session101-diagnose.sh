#!/usr/bin/env bash
# Session 101 — single-paste diagnostic.
#
# Reads the live `situation_room.duckdb` and emits ONE log
# (`session101-diagnose.log`) covering Stages A → D from
# `SESSION_101_KICKOFF.md`. Operator runs once, rsyncs, agent
# reads once. No 5-round-trip paste-back.
#
# All reads are `duckdb -readonly`. LLM-FREE. No mutations. No
# cargo. No frontend.
#
# Column-name drift relative to the kickoff (verified against
# migrations/0001..0023):
#   - `recipes.created_at`        → `recipes.authored_at`
#   - `recipes.produces_json`     → `recipes.produces`            (JSON)
#   - `recipes.extraction_spec`   → `recipes.extraction`          (JSON)
#   - `recipe_fetch_attempts.url_attempted` does NOT exist; the URL
#     lives on `recipes.source_url`. Joined here.
#   - `recipe_fetch_attempts.response_status` does NOT exist; the
#     status proxy is `succeeded BOOLEAN`. `failure_message` carries
#     the apply-stage prose.
#   - `recipe_feedback` is keyed by `(plan_id, source_id)`, not by
#     `recipe_id`. Joined here on (plan_id, source_id).
#   - `observations.metric` does NOT exist as a column; the metric
#     lives inside `observations.content` JSON
#     (`json_extract_string($.metric)`).
#   - `observations.source_id` is formatted by `recipe_apply.rs` as
#     `{recipe.source_id}#recipe:{recipe.id}@v{version}` — joinable
#     via `LIKE '%' || recipe_id::TEXT || '%'`.
#
# Plan IDs the kickoff named:
#   TESLA (today, 2/8 coverage)      019e3a75-62c9-72f1-bb5c-48e6cb54f3c7
#   META  (reference, market_cap OK) 019e30c4-617f-7fc0-8a71-d2824a6b10e5

set -u  # NOT -e: a single stage failure should not blank the log.

REPO="/Users/aben/RustroverProjects/situation_room"
WORKSPACE="/Users/aben/Documents/Claude/Projects/SituationRoom"
DB="${REPO}/situation_room.duckdb"
# Log goes straight to the Cowork workspace folder — both paths are on
# the same Mac, so no rsync round-trip is needed to ship the log back
# to the agent. The script itself can be run from either location; only
# the DB is read from prod.
LOG="${WORKSPACE}/session101-diagnose.log"

TESLA="019e3a75-62c9-72f1-bb5c-48e6cb54f3c7"
META="019e30c4-617f-7fc0-8a71-d2824a6b10e5"

if ! command -v duckdb >/dev/null 2>&1; then
    echo "duckdb CLI not on PATH; brew install duckdb" >&2
    exit 1
fi
if [[ ! -f "${DB}" ]]; then
    echo "DB not found: ${DB}" >&2
    exit 1
fi

# Truncate the log so re-runs are idempotent.
: > "${LOG}"

stage() {
    local label="$1"
    {
        printf '\n\n===== %s =====\n' "${label}"
    } >> "${LOG}"
}

run_sql() {
    local sql="$1"
    duckdb -readonly "${DB}" -box -c "${sql}" >> "${LOG}" 2>&1 || true
}

stage "Stage 0 — schema_migrations head (sanity)"
run_sql "SELECT MAX(version) AS max_version, COUNT(*) AS rows
           FROM schema_migrations;"

stage "Stage 0b — both plan IDs exist?"
run_sql "SELECT id, topic, status,
                created_at,
                historical_window_days AS window_days,
                classified_by
           FROM research_plans
          WHERE id IN ('${TESLA}', '${META}')
          ORDER BY created_at ASC;"

stage "Stage A — TESLA plan: every recipe + Track-A reauthor_reason + Track-B feedback note"
# LEFT JOIN recipe_feedback on (plan_id, source_id) — recipe_feedback is
# keyed by source_id, not recipe_id. One recipe → at most one feedback
# row, but multiple recipe versions on the same source_id share that row.
run_sql "
SELECT r.id            AS recipe_id,
       r.source_id,
       r.authored_at,
       r.authored_from,
       r.prior_recipe_id,
       r.version,
       SUBSTR(r.reauthor_reason, 1, 240) AS reauthor_reason_head,
       SUBSTR(rf.note, 1, 240)            AS feedback_note_head,
       SUBSTR(r.produces::TEXT, 1, 240)   AS produces_head
  FROM recipes r
  LEFT JOIN recipe_feedback rf
         ON rf.plan_id   = r.plan_id
        AND rf.source_id = r.source_id
 WHERE r.plan_id = '${TESLA}'
 ORDER BY r.authored_at ASC;"

stage "Stage A-summary — count by authored_from (nominated / re-authored / declined)"
run_sql "
SELECT COALESCE(authored_from, '(null)') AS authored_from,
       COUNT(*) AS n
  FROM recipes
 WHERE plan_id = '${TESLA}'
 GROUP BY 1
 ORDER BY n DESC;"

stage "Stage B — TESLA plan expectations (what dispatch was supposed to cover)"
run_sql "
SELECT json_extract_string(expectations, '\$.observation_metrics') AS observation_metrics,
       json_extract_string(expectations, '\$.event_types')         AS event_types,
       json_extract_string(expectations, '\$.entity_kinds')        AS entity_kinds,
       json_extract_string(expectations, '\$.relation_kinds')      AS relation_kinds,
       json_extract_string(expectations, '\$.document_sources')    AS document_sources_head
  FROM research_plans
 WHERE id = '${TESLA}';"

stage "Stage B-bucket — per-expectation-slot nominations on TESLA (bucket-fair dispatch sanity)"
# Pulls each recipe's binding intent out of the produces JSON. If
# bucket-fair dispatch (ADR 0018) worked, we should see ≥1 row per
# expectation slot.
run_sql "
SELECT COALESCE(json_extract_string(r.produces, '\$.observation.metric'),
                json_extract_string(r.produces, '\$.event_kind'),
                json_extract_string(r.produces, '\$.entity_kind'),
                json_extract_string(r.produces, '\$.relation_kind'),
                '(unparsed)')               AS binds_to,
       COUNT(*)                              AS n_recipes,
       COUNT(r.reauthor_reason)              AS n_with_reauthor_reason
  FROM recipes r
 WHERE r.plan_id = '${TESLA}'
 GROUP BY 1
 ORDER BY n_recipes DESC;"

stage "Stage C-1 — META reference plan: recipes touching market_cap"
run_sql "
SELECT r.id            AS recipe_id,
       r.source_id,
       r.authored_at,
       r.authored_from,
       r.prior_recipe_id,
       SUBSTR(r.produces::TEXT, 1, 400)   AS produces_head,
       SUBSTR(r.extraction::TEXT, 1, 400) AS extraction_head
  FROM recipes r
 WHERE r.plan_id = '${META}'
   AND r.produces::TEXT LIKE '%market_cap%'
 ORDER BY r.authored_at ASC;"

stage "Stage C-2 — META reference plan: observations actually written from those recipes"
# observations.source_id is formatted '{slug}#recipe:{uuid}@v{n}' by
# recipe_apply.rs::source_id. Joining on substring of recipe UUID.
run_sql "
WITH mkt_recipes AS (
    SELECT id, source_id
      FROM recipes
     WHERE plan_id = '${META}'
       AND produces::TEXT LIKE '%market_cap%'
)
SELECT o.source_url,
       json_extract_string(o.content, '\$.metric') AS metric,
       COUNT(*)                                     AS n
  FROM observations o
  JOIN mkt_recipes mr
    ON o.source_id LIKE '%' || mr.id::TEXT || '%'
 GROUP BY 1, 2
 ORDER BY n DESC;"

stage "Stage C-3 — TESLA plan: ALL distinct (source_id, authored_from) — closed-vocab class-blind"
# Replaces the kickoff's host-substring LIKE. Operator + agent eyeball
# the set; closed-vocab discipline holds (we don't assume CNBC/NASDAQ
# are even in the set).
run_sql "
SELECT r.source_id,
       COALESCE(r.authored_from, '(null)') AS authored_from,
       COUNT(*)                             AS n_versions,
       MIN(r.authored_at)                   AS first_seen,
       MAX(r.authored_at)                   AS last_seen
  FROM recipes r
 WHERE r.plan_id = '${TESLA}'
 GROUP BY r.source_id, authored_from
 ORDER BY first_seen ASC;"

stage "Stage C-3b — META plan: same shape, for diff against TESLA"
run_sql "
SELECT r.source_id,
       COALESCE(r.authored_from, '(null)') AS authored_from,
       COUNT(*)                             AS n_versions,
       MIN(r.authored_at)                   AS first_seen,
       MAX(r.authored_at)                   AS last_seen
  FROM recipes r
 WHERE r.plan_id = '${META}'
 GROUP BY r.source_id, authored_from
 ORDER BY first_seen ASC;"

stage "Stage D — TESLA recipe_fetch_attempts (URL via recipes join; status via succeeded BOOLEAN)"
run_sql "
SELECT fa.recipe_id,
       r.source_id,
       r.source_url,
       fa.attempted_at,
       fa.succeeded,
       fa.response_content_type,
       SUBSTR(fa.failure_message, 1, 240) AS failure_message_head,
       LENGTH(fa.bytes_excerpt)            AS bytes_excerpt_len
  FROM recipe_fetch_attempts fa
  JOIN recipes r ON r.id = fa.recipe_id
 WHERE r.plan_id = '${TESLA}'
 ORDER BY fa.attempted_at ASC;"

stage "Stage D-summary — fetch_attempts presence per TESLA recipe (declined-without-fetch vs apply-failed)"
run_sql "
SELECT r.id            AS recipe_id,
       r.source_id,
       COUNT(fa.id)    AS n_fetch_attempts,
       SUM(CASE WHEN fa.succeeded THEN 1 ELSE 0 END) AS n_succeeded
  FROM recipes r
  LEFT JOIN recipe_fetch_attempts fa ON fa.recipe_id = r.id
 WHERE r.plan_id = '${TESLA}'
 GROUP BY r.id, r.source_id
 ORDER BY n_fetch_attempts DESC, r.source_id ASC;"

stage "Stage E — TESLA observations actually persisted (what landed = 2/8)"
run_sql "
WITH tesla_recipes AS (
    SELECT id, source_id FROM recipes WHERE plan_id = '${TESLA}'
)
SELECT json_extract_string(o.content, '\$.metric') AS metric,
       tr.source_id                                  AS recipe_source_id,
       COUNT(*)                                      AS n
  FROM observations o
  JOIN tesla_recipes tr
    ON o.source_id LIKE '%' || tr.id::TEXT || '%'
 GROUP BY 1, 2
 ORDER BY n DESC;"

stage "Stage E-other — TESLA events / entities / assertions counts"
run_sql "
WITH tesla_recipes AS (SELECT id FROM recipes WHERE plan_id = '${TESLA}')
SELECT 'events'     AS rec_kind, COUNT(*) AS n FROM events     e
   JOIN tesla_recipes tr ON e.source_id LIKE '%' || tr.id::TEXT || '%'
UNION ALL
SELECT 'entities',   COUNT(*) FROM entities    en
   JOIN tesla_recipes tr ON en.source_id LIKE '%' || tr.id::TEXT || '%'
UNION ALL
SELECT 'assertions', COUNT(*) FROM assertions  a
   JOIN tesla_recipes tr ON a.source_id LIKE '%' || tr.id::TEXT || '%';"

printf '\n\n===== END =====\n' >> "${LOG}"

echo
echo "Done. Log: ${LOG}"
echo "Size: $(wc -c < "${LOG}") bytes"
echo
echo "Log is already in the Cowork workspace folder — agent reads it next turn."
echo "(No reverse rsync needed; workspace and prod live on the same Mac.)"
