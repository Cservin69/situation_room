#!/usr/bin/env bash
# Session 66 — hunt JsonPath + CsvCell strict Class B cases via the
# eval-harness. Each topic targets one of the still-empty modes from
# ADR 0012 Condition 3:
#
#   JsonPath   →  "FEMA disaster declarations 2025"
#                 (api.fema.gov publishes JSON; expectation: the LLM
#                 authors a JsonPath recipe against the API URL)
#   CsvCell    →  "US monthly unemployment by state 2025"
#                 (BLS / data.census.gov publish CSVs; expectation:
#                 LLM authors a CsvCell recipe against a CSV endpoint)
#
# These are speculative — the LLM may decline either topic at the URL
# proposer stage, or it may author a CssSelect recipe against an HTML
# index page even when a JSON / CSV endpoint exists. Five trials per
# topic give us a measurement, not a guarantee.
#
# `--keep-dbs` because we want the per-trial DuckDB files for the
# case-file fill-in pass per ADR 0012 §"Documenting observed Class B
# failures."
#
# JSONL lands at:    eval-runs/<slug>-<stamp>.jsonl
# Eval logs land at: logs/session66-<slug>-eval-<stamp>.log
#
# Operator runs this on Mac. Sandbox can't run cargo (proxy blocks
# crates.io / sh.rustup.rs per memory/workflow_cargo_mac.md).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mkdir -p logs

stamp="$(date -u +%Y%m%dT%H%M%SZ)"

run_eval() {
    local topic="$1"
    local slug="$2"
    echo
    echo "==> eval-harness: 5 trials × '${topic}'"
    cargo run --release -p situation_room-eval-harness -- \
        --topic "${topic}" \
        --trials 5 \
        --keep-dbs \
      2>&1 | tee "logs/session66-${slug}-eval-${stamp}.log"
}

# --- JsonPath target ----------------------------------------------------
run_eval "FEMA disaster declarations 2025" "fema-2025"

# --- CsvCell target -----------------------------------------------------
run_eval "US monthly unemployment by state 2025" "bls-unemployment-2025"

echo
echo "==> done."
echo "Next: examine the per-trial DuckDB files at /tmp/situation_room-eval-*/"
echo "for any failure_message matching the ADR 0012 predicate strings:"
echo "  - 'path matched no nodes'      (JsonPath)"
echo "  - 'no row matched filter'      (CsvCell)"
echo
echo "Per-trial inspection query:"
echo "  SELECT r.endpoint_url, rfa.failure_message"
echo "  FROM recipe_fetch_attempts rfa"
echo "  JOIN recipes r ON r.id = rfa.recipe_id"
echo "  WHERE rfa.succeeded = FALSE"
echo "  ORDER BY rfa.attempted_at DESC;"
