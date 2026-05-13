#!/usr/bin/env bash
# Session 67 — re-hunt for JsonPath strict Class B cases after the
# Session 67 validator-gap fix.
#
# Background. Session 66's session66-hunt-classB.sh produced zero
# strict Class B JsonPath cases across 5 FEMA + 5 BLS trials. The
# diagnosis (recorded in ADR 0019's "Session 67 verification"
# subsection) was that every json_path × json_path recipe the LLM
# authored was intercepted at authoring with `extraction mode not
# implemented: iterator` — the structural validator's match-arm in
# `validate_recipe_against_bytes` was missing the (JsonPath, JsonPath)
# branch, even though the runtime's `apply_json_iterator` had supported
# the pair since Session 61. The Session 67 patch closes that
# coherence gap.
#
# Expectation under the new validator:
#   - FEMA trial 2's near-miss shape ($.DisasterDeclarationsSummaries[*]
#     outer + $.femaDeclarationString inner) — or a sibling shape the
#     LLM authors on the same source — should now persist as a recipe
#     and surface at apply with one of:
#       (a) "iterator path … matched no nodes"  — strict Class B JsonPath
#       (b) "inner path … matched no nodes within scope" — strict Class B JsonPath
#       (c) success: records produced (≠ Class B; that is the records
#           outcome the system should be optimising for first)
#     Any of (a/b) is a new mode for ADR 0012 Condition 2 (mode
#     diversity climbs 2 → 3).
#
# **Cost-cut from Session 66's 5×2 = 10-trial hunt:** this runs
# 2 trials × FEMA only. BLS is dropped — Session 66 established
# every BLS CSV path returns 403 (host_blocked_by_waf), so the LLM
# never reaches CsvCell on it; running BLS again would re-confirm a
# null result we already have. FEMA is enough to verify the
# json_path × json_path path now persists. If 2 trials don't show
# the new behaviour, the cost-cheap escalation is to bump to 5
# trials on FEMA only — still cheaper than Session 66's matrix.
#
# Operator runs this on Mac. Sandbox can't run cargo per
# memory/workflow_cargo_mac.md.
#
# Prerequisites:
#   cargo test --workspace     # the Session 67 validator tests must pass
#                              # before the eval-harness re-run carries
#                              # any signal — a broken validator would
#                              # poison the result.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mkdir -p logs

stamp="$(date -u +%Y%m%dT%H%M%SZ)"

echo "==> [1/2] cargo test --workspace (picks up Session 67 validator tests)"
cargo test --workspace 2>&1 \
  | tee "logs/session67-cargo-test-${stamp}.log"

echo
echo "==> [2/2] eval-harness: 2 trials × 'FEMA disaster declarations 2025' (post-validator-fix)"
cargo run --release -p situation_room-eval-harness -- \
    --topic "FEMA disaster declarations 2025" \
    --trials 2 \
    --keep-dbs \
  2>&1 | tee "logs/session67-fema-2025-eval-${stamp}.log"

echo
echo "==> done."
echo
echo "Compare against Session 66 baseline:"
echo "  eval-runs/fema-disaster-declarations-2025-20260513T113806Z.jsonl"
echo
echo "First diagnostic: did ANY recipe persist as json_path × json_path?"
echo "  Read the new JSONL; look for outcomes[].kind=\"failed\"/stage=\"apply\""
echo "  rows with json_path predicate text, OR outcomes[].kind=\"succeeded\""
echo "  with FEMA source_id. Either confirms the gate opened."
echo
echo "Strict Class B JsonPath predicate match (apply-time, persisted recipe):"
echo "  for db in /tmp/situation_room-eval-*/trial-*.duckdb \\"
echo "            \"\${TMPDIR}situation_room-eval-*\"/trial-*.duckdb; do"
echo "    echo \"\$db:\""
echo "    duckdb \"\$db\" -c \"SELECT r.endpoint_url, rfa.failure_message"
echo "                       FROM recipe_fetch_attempts rfa"
echo "                       JOIN recipes r ON r.id = rfa.recipe_id"
echo "                       WHERE rfa.succeeded = FALSE"
echo "                         AND (rfa.failure_message LIKE '%iterator path%matched no nodes%'"
echo "                           OR rfa.failure_message LIKE '%inner path%matched no nodes within%')"
echo "                       ORDER BY rfa.attempted_at DESC;\""
echo "  done"
echo
echo "Each match grounds a new docs/failure_cases/class_b/<stamp>_<host>_jsonpath_*.md file."
