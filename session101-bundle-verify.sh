#!/usr/bin/env bash
# Session 101 bundle — Lever 1 (lift lock-on-first-success) + Lever 2a
# (wire UA policy into prefetch_excerpt) verify runbook.
#
# Per feedback_eval_cost_discipline: NO LLM-paid trials in this
# runbook. The verify gate is "tests + build + svelte-check + pre-
# bundle coverage snapshot." Live coverage check (TESLA + tesla-stock
# re-fetch) is operator-paid and gated on this stage passing.

set -u
# Sn-101 fix: pipefail makes `cargo … | tee` return cargo's exit status
# rather than tee's. Without this, the verify script's first run
# silently flagged Stage 2 + Stage 3 as PASS even though cargo failed
# to compile — tee's 0 exit was hiding the real failure.
set -o pipefail

REPO="/Users/aben/RustroverProjects/situation_room"
WORKSPACE="/Users/aben/Documents/Claude/Projects/SituationRoom"
EXEC="${REPO}/crates/pipeline/src/fetch_executor.rs"
PROPOSE_RS="${REPO}/crates/pipeline/src/propose_source_url.rs"
PROPOSE_MD="${REPO}/config/prompts/propose_source_url.md"

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
note()   { printf '\n--- %s ---\n' "$*"; }
fail()   { red   "FAIL: $*"; exit 1; }
pass()   { green "PASS: $*"; }
warn()   { yellow "WARN: $*"; }

cd "${REPO}" || fail "cd ${REPO}"

# ----- Stage 1 — code markers (sanity that the bundle is present) -------
note "Stage 1 — Sn-101 code markers in fetch_executor.rs"

if ! grep -q "Session 101 Lever 1 — lift lock-on-first-success" "${EXEC}"; then
    fail "Lever 1 marker absent in fetch_executor.rs — bundle not applied"
fi
pass "Lever 1 marker present"

if ! grep -q "Session 101 / Lever 2a — UA-policy parity" "${EXEC}"; then
    fail "Lever 2a marker absent in fetch_executor.rs — bundle not applied"
fi
pass "Lever 2a marker present"

if ! grep -q "filled_targets: Vec<ExpectationRef>" "${EXEC}"; then
    fail "Lever 1 filled_targets state absent — partial edit"
fi
pass "Lever 1 filled_targets state present"

if ! grep -q "fetch_with_backoff_ua(" "${EXEC}"; then
    fail "fetch_with_backoff_ua not called in fetch_executor.rs — Lever 2a not wired"
fi
# Lever 2a's prefetch_excerpt now calls fetch_with_backoff_ua; count
# uses: pre-Sn-101 had 1 (in fetch_recipe_bytes); post-Sn-101 has 2.
UA_CALLS=$(grep -c "fetch_with_backoff_ua(" "${EXEC}")
if [[ "${UA_CALLS}" -lt 2 ]]; then
    fail "expected ≥2 fetch_with_backoff_ua call sites (prefetch + recipe), found ${UA_CALLS}"
fi
pass "fetch_with_backoff_ua called from ${UA_CALLS} sites (≥2 required)"

# Lever 3 — target-aware proposer.
if ! grep -q "target_kinds_needed: &\[&str\]" "${PROPOSE_RS}"; then
    fail "Lever 3 target_kinds_needed param absent in propose_source_url.rs"
fi
pass "Lever 3 target_kinds_needed param present"
if ! grep -q "render_target_kinds_needed" "${PROPOSE_RS}"; then
    fail "Lever 3 render_target_kinds_needed helper absent"
fi
pass "Lever 3 render_target_kinds_needed helper present"
if ! head -n 1 "${PROPOSE_MD}" | grep -q "v1\.6"; then
    fail "propose_source_url.md not bumped to v1.6"
fi
pass "propose_source_url.md header reads v1.6"
if ! grep -q "{{TARGET_KINDS_NEEDED}}" "${PROPOSE_MD}"; then
    fail "Lever 3 placeholder TARGET_KINDS_NEEDED absent in prompt template"
fi
pass "Lever 3 placeholder TARGET_KINDS_NEEDED present in prompt template"
if ! grep -q "remaining_kind_strings" "${EXEC}"; then
    fail "Lever 3 caller wiring (remaining_kind_strings) absent in fetch_executor.rs"
fi
pass "Lever 3 caller wiring (remaining_kind_strings) present"

# ----- Stage 2 — cargo test -p situation_room-pipeline ------------------
note "Stage 2 — cargo test -p situation_room-pipeline (full)"

if cargo test -p situation_room-pipeline 2>&1 | tee "${WORKSPACE}/session101-bundle-cargo-test.log"; then
    pass "pipeline tests green"
else
    fail "pipeline tests failed — see session101-bundle-cargo-test.log"
fi

# ----- Stage 3 — workspace build -----------------------------------------
note "Stage 3 — cargo build --workspace"

if cargo build --workspace 2>&1 | tee "${WORKSPACE}/session101-bundle-cargo-build.log"; then
    pass "workspace build green"
else
    fail "workspace build failed — see session101-bundle-cargo-build.log"
fi

# ----- Stage 4 — svelte-check (no frontend changes, regression guard) ---
note "Stage 4 — npm run check (regression guard)"

if (cd apps/desktop && npm run check) 2>&1 | tee "${WORKSPACE}/session101-bundle-svelte-check.log"; then
    pass "svelte-check green"
else
    fail "svelte-check failed — see session101-bundle-svelte-check.log"
fi

# ----- Stage 5 — coverage snapshot for both TESLA plans -----------------
note "Stage 5 — pre-bundle coverage snapshot (both TESLA plans, LLM-free, read-only)"

DB="${REPO}/situation_room.duckdb"
SNAP="${WORKSPACE}/session101-bundle-pre-coverage.txt"
TESLA_UPPER="019e3a75-62c9-72f1-bb5c-48e6cb54f3c7"
TESLA_LOWER="019e3202-6ade-7270-9ca0-154c5e839e80"

if ! command -v duckdb >/dev/null 2>&1; then
    warn "duckdb CLI not on PATH; skipping pre-bundle snapshot"
else
    {
        for PLAN in "${TESLA_UPPER}" "${TESLA_LOWER}"; do
            printf '\n===== PLAN %s =====\n' "${PLAN}"
            duckdb -readonly "${DB}" -box -c "
                SELECT 'recipes' AS k, COUNT(*) AS n FROM recipes WHERE plan_id = '${PLAN}'
                UNION ALL SELECT 'distinct_sources', COUNT(DISTINCT source_id) FROM recipes WHERE plan_id = '${PLAN}'
                UNION ALL SELECT 'fetch_outcomes_total', COUNT(*) FROM fetch_run_outcomes WHERE plan_id = '${PLAN}'
                UNION ALL SELECT 'fetch_outcomes_declined', COUNT(*) FROM fetch_run_outcomes WHERE plan_id = '${PLAN}' AND outcome_kind = 'declined'
                UNION ALL SELECT 'fetch_outcomes_succeeded', COUNT(*) FROM fetch_run_outcomes WHERE plan_id = '${PLAN}' AND outcome_kind = 'succeeded';"
        done
    } > "${SNAP}" 2>&1
    pass "pre-bundle snapshot written to ${SNAP}"
fi

echo
green "All LLM-free stages green. Sn-101 bundle (Lever 1 + Lever 2a) is ready to ship."
echo
yellow "NEXT — operator-paid live verification (NOT in this script):"
echo "  1. Cmd-Q the desktop if running."
echo "  2. Launch the desktop fresh (it now loads the post-bundle binary)."
echo "  3. Create a NEW plan (Lever 1 only affects accept-time authoring;"
echo "     existing plans still hold their recipes — see Sn-101 finding"
echo "     that Run fetch on accepted plans does not re-author)."
echo "  4. Accept the plan and watch the recipes pane fill."
echo "  5. Diff coverage against the post-Sn-101 baseline:"
echo "     - Pre: TESLA-upper 2/8, TESLA-lower 4/8 (yahoo only)."
echo "     - Expected post-Sn-101 on a fresh plan: ≥3/8 coverage,"
echo "       ≥2 distinct source classes (Lever 1 pivots URL per still-"
echo "       unfilled target; e.g. yahoo chart → yahoo quoteSummary"
echo "       for market_cap). Lever 2a only fires when HOST_CLASS_OVERRIDES"
echo "       is populated — run \`cargo run --bin host_probe\` for"
echo "       additional unblocking on SEC EDGAR / Reuters / etc."
echo "  6. Budget: one accept-time run is ~\$0.10–\$2.00 worst-case"
echo "     (4 nominations × up to 3 propose × up to 6 author calls)."
