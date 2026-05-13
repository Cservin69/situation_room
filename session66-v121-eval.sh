#!/usr/bin/env bash
# Session 66 — variance-bounded comparison of recipe_author v1.21
# (selector_trace prompt experiment) against v1.20 baseline.
#
# v1.20 baseline (Session 64): 5 trials × "2025 Atlantic hurricane
# season" produced records [0, 30, 0, 0, 1], with
# recipes_with_extracted_inner [1, 0, 0, 0, 1].
# Reference JSONL:
#   eval-runs/2025-atlantic-hurricane-season-20260512T153257Z.jsonl
#
# v1.21 hypothesis: the explicit selector_trace forces the LLM to
# walk the iterator-scope→inner-selector descendant check before
# committing. Class B inner-no-elements failures should drop;
# records may rise (less likely to fall, because the trace is
# mechanism-neutral for the apply path).
#
# Session 56 lesson stands: per-trial variance > prompt-version
# effect at N=5. 5 trials each is the floor; if results are
# inconclusive, the operator should follow up with 10 trials or
# pool across topics.
#
# Operator runs this on Mac. Sandbox can't run cargo.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mkdir -p logs

stamp="$(date -u +%Y%m%dT%H%M%SZ)"

echo "==> [1/2] cargo test --workspace (picks up v1.21 prompt + selector_trace + Store::checkpoint tests)"
cargo test --workspace 2>&1 \
  | tee "logs/session66-cargo-test-${stamp}.log"

echo
echo "==> [2/2] eval-harness: 5 trials × '2025 Atlantic hurricane season' (v1.21)"
echo "    (~3-5 min per trial; --keep-dbs to inspect failure rates post-hoc)"
cargo run --release -p situation_room-eval-harness -- \
    --topic "2025 Atlantic hurricane season" \
    --trials 5 \
    --keep-dbs \
  2>&1 | tee "logs/session66-v121-hurricane-eval-${stamp}.log"

echo
echo "==> done. Compare against v1.20 baseline:"
echo "    eval-runs/2025-atlantic-hurricane-season-20260512T153257Z.jsonl"
echo
echo "Quick comparison (counts records per trial):"
echo "  jq '.records_produced' eval-runs/2025-atlantic-hurricane-season-*.jsonl | sort"
echo
echo "Class B inner-no-elements per trial:"
echo "  for db in /tmp/situation_room-eval-*/trial-*.duckdb; do"
echo "    echo \"\$db:\""
echo "    duckdb \"\$db\" -c \"SELECT COUNT(*) FROM recipe_fetch_attempts WHERE failure_message LIKE '%selector matched no elements%';\""
echo "  done"
