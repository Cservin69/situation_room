#!/usr/bin/env bash
# Session 77 verification — three-stage gate.
#
#   Stage 1: cargo check on pipeline + storage + api + llm + apps_common
#            + the desktop binary (apps/desktop/src-tauri) + eval_harness.
#            The full check catches the `ExecutorContext` field rollout,
#            the `RelationKindExpectation::exemplar_triples` addition,
#            and the `AppState::new` signature change in one pass.
#   Stage 2: cargo test on the four crates that carry new logic
#            (pipeline + storage + api + llm). Each crate's tests
#            land alongside the new code.
#   Stage 3: svelte-check on apps/desktop. Catches stale-prompt banner
#            wiring, `classified_by` DTO surface, and the new
#            RelationTripleExemplarDto / ClassifierPromptVersionDto
#            generated types.
#
# Pass criteria: EXIT=0 on the last line of all three logs.
# Sandbox can't run cargo (proxy blocks crates.io / sh.rustup.rs);
# operator runs this on Mac. The script tees both stdout+stderr
# into logs/ so a fresh agent run can read the outcome from disk
# without re-running.

set -u

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

mkdir -p logs
TS=$(date -u +%Y%m%dT%H%M%SZ)

stage1="logs/session77-cargo-check-${TS}.log"
stage2="logs/session77-cargo-test-${TS}.log"
stage3="logs/session77-svelte-check-${TS}.log"

echo "== Stage 1: cargo check (workspace) =="
{
  cargo check --workspace --all-targets 2>&1
  echo "EXIT=$?"
} | tee "$stage1"

echo "== Stage 2: cargo test (pipeline + storage + api + llm) =="
{
  cargo test \
    --package situation_room-pipeline \
    --package situation_room-storage \
    --package situation_room-api \
    --package situation_room-llm \
    --lib 2>&1
  echo "EXIT=$?"
} | tee "$stage2"

echo "== Stage 3: svelte-check =="
{
  ( cd apps/desktop && npm run check 2>&1 )
  echo "EXIT=$?"
} | tee "$stage3"

echo ""
echo "Logs:"
echo "  Stage 1 (cargo check):  $stage1"
echo "  Stage 2 (cargo test):   $stage2"
echo "  Stage 3 (svelte-check): $stage3"
echo ""
echo "Pass criteria: last line of each log is EXIT=0."
