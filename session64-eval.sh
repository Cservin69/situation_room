#!/usr/bin/env bash
# Session 64 — validate the type-blind-coercion fix and run a 5-trial
# v1.20 hurricane eval against the instrumented harness. Tees stderr
# into logs/ so the agent can read the run summary alongside the
# JSONL.
#
# JSONL lands at: eval-runs/2025-atlantic-hurricane-season-<stamp>.jsonl
# Test log lands at: logs/session64-cargo-test-<stamp>.log
# Eval log lands at: logs/session64-hurricane-eval-<stamp>.log

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mkdir -p logs

stamp="$(date -u +%Y%m%dT%H%M%SZ)"

echo "==> [1/2] cargo test --workspace (validates Session 64 fix + new tests)"
cargo test --workspace 2>&1 \
  | tee "logs/session64-cargo-test-${stamp}.log"

echo
echo "==> [2/2] eval-harness: 5 trials × '2025 Atlantic hurricane season'"
echo "    (~3-5 min per trial, --keep-dbs for post-hoc inspection)"
cargo run --release -p situation_room-eval-harness -- \
    --topic "2025 Atlantic hurricane season" \
    --trials 5 \
    --keep-dbs \
  2>&1 | tee "logs/session64-hurricane-eval-${stamp}.log"

echo
echo "==> done. JSONL at eval-runs/, summary log at logs/session64-hurricane-eval-${stamp}.log"
