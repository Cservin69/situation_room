#!/usr/bin/env bash
# Session 101 — verify runbook for the recipe_author.md v1.25 → v1.26
# bump (per-target authoring discipline).
#
# Per feedback_eval_cost_discipline: NO LLM-paid trials in this
# runbook. The verify gate is "compiles + prompt-version header
# correct + post-build size still under the LLM_PROMPT_BODY budget
# headroom"; the live coverage check (TESLA plan 2/8 → ?/8) is a
# separate operator-paid run gated on this Stage passing.

set -u

REPO="/Users/aben/RustroverProjects/situation_room"
WORKSPACE="/Users/aben/Documents/Claude/Projects/SituationRoom"
PROMPT="${REPO}/config/prompts/recipe_author.md"

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
note()   { printf '\n--- %s ---\n' "$*"; }
fail()   { red   "FAIL: $*"; exit 1; }
pass()   { green "PASS: $*"; }
warn()   { yellow "WARN: $*"; }

cd "${REPO}" || fail "cd ${REPO}"

# ----- Stage 1 — prompt-version header ----------------------------------
note "Stage 1 — prompt-version header (sanity)"

HEAD=$(head -n 1 "${PROMPT}")
if [[ "${HEAD}" != "# Recipe Author Prompt — v1.26" ]]; then
    fail "prompt header is '${HEAD}', expected '# Recipe Author Prompt — v1.26'"
fi
pass "prompt header reads v1.26"

if ! grep -q "^## Per-target authoring " "${PROMPT}"; then
    fail "expected '## Per-target authoring …' section in prompt; absent"
fi
pass "new section 'Per-target authoring' present"

if ! grep -q "^- \*\*v1\.26\*\*" "${PROMPT}"; then
    fail "expected '- **v1.26**' changelog entry in prompt; absent"
fi
pass "v1.26 changelog entry present"

# Size sanity — LLM_PROMPT_BODY = 256 KiB; pre-edit was ~150 KiB, the
# new section adds ~6 KiB. Anything above 192 KiB risks squeezing the
# excerpt budget (PREFETCH_EXCERPT_BUDGET = 64 KiB) plus plan JSON.
SIZE=$(wc -c < "${PROMPT}")
if [[ "${SIZE}" -gt 196608 ]]; then
    fail "prompt size ${SIZE} bytes > 192 KiB threshold; squeezes excerpt budget"
fi
pass "prompt size ${SIZE} bytes (under 192 KiB threshold)"

# ----- Stage 2 — recipe_author crate tests ------------------------------
note "Stage 2 — cargo test -p situation_room-pipeline recipe_author"

if cargo test -p situation_room-pipeline --lib recipe_author 2>&1 | tee "${WORKSPACE}/session101-cargo-test.log"; then
    pass "recipe_author tests green"
else
    fail "recipe_author tests failed — see session101-cargo-test.log"
fi

# ----- Stage 3 — workspace builds (desktop + eval_harness include_str!) -
note "Stage 3 — cargo build --workspace (picks up include_str! changes)"

if cargo build --workspace 2>&1 | tee "${WORKSPACE}/session101-cargo-build.log"; then
    pass "workspace build green"
else
    fail "workspace build failed — see session101-cargo-build.log"
fi

# ----- Stage 4 — Svelte check (no frontend changes, regression guard) ---
note "Stage 4 — npm run check (no-frontend-change regression guard)"

if (cd apps/desktop && npm run check) 2>&1 | tee "${WORKSPACE}/session101-svelte-check.log"; then
    pass "svelte-check green"
else
    fail "svelte-check failed — see session101-svelte-check.log"
fi

# ----- Stage 5 — coverage-readiness snapshot (LLM-FREE, read-only) ------
note "Stage 5 — TESLA pre-fix coverage snapshot for diff against post-fix run"

DB="${REPO}/situation_room.duckdb"
SNAP="${WORKSPACE}/session101-pre-fix-coverage.txt"

if ! command -v duckdb >/dev/null 2>&1; then
    warn "duckdb CLI not on PATH; skipping pre-fix coverage snapshot"
else
    {
        printf '===== TESLA pre-fix recipes (should be 2 on yahoo) =====\n'
        duckdb -readonly "${DB}" -box -c "
            SELECT source_id, authored_at
              FROM recipes
             WHERE plan_id = '019e3a75-62c9-72f1-bb5c-48e6cb54f3c7'
             ORDER BY authored_at ASC;"
        printf '\n===== TESLA pre-fix obs metrics covered (should be 2 of 3) =====\n'
        duckdb -readonly "${DB}" -box -c "
            WITH tesla AS (SELECT id FROM recipes
                            WHERE plan_id = '019e3a75-62c9-72f1-bb5c-48e6cb54f3c7')
            SELECT json_extract_string(o.content, '\$.metric') AS metric, COUNT(*) AS n
              FROM observations o JOIN tesla t ON o.source_id LIKE '%' || t.id::TEXT || '%'
             GROUP BY 1 ORDER BY n DESC;"
        printf '\n===== TESLA pre-fix events/entities counts (should be 0/1) =====\n'
        duckdb -readonly "${DB}" -box -c "
            WITH tesla AS (SELECT id FROM recipes
                            WHERE plan_id = '019e3a75-62c9-72f1-bb5c-48e6cb54f3c7')
            SELECT 'events'   AS k, COUNT(*) FROM events     e
               JOIN tesla t ON e.source_id LIKE '%' || t.id::TEXT || '%'
            UNION ALL
            SELECT 'entities',     COUNT(*) FROM entities en
               JOIN tesla t ON en.source_id LIKE '%' || t.id::TEXT || '%';"
    } > "${SNAP}" 2>&1
    pass "pre-fix snapshot written to ${SNAP}"
fi

echo
green "All non-LLM stages green. Prompt v1.26 is ready to ship."
echo
yellow "NEXT — operator-paid live verification (NOT in this script):"
echo "  1. Re-fetch the TESLA plan from the desktop UI."
echo "  2. Diff post-fix coverage against ${SNAP}."
echo "  3. PASS criterion: observation_metrics coverage moves from 2/3"
echo "     toward 3/3 (market_cap lands), OR ≥1 event_type / entity_kind"
echo "     binding lands where none did before."
echo "  4. ${TASK_COST_NOTE:-LLM-paid: budget ~\$0.10–\$1.00 per re-fetch (per feedback_eval_cost_discipline}."
