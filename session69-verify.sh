#!/usr/bin/env bash
# Session 69 — verify the per-fetch Document synthesis + the classify
# screen-reset fix.
#
# What this runs:
#   1. cargo test --workspace
#      — all backend tests, including the 16 new unit tests in
#        `crates/pipeline/src/document_synth.rs` (MIME→kind mapping,
#        body preview UTF-8 safety, build_fetch_document shape) and the
#        new wiring test
#        `run_fetch_synthesises_one_document_per_successful_fetch_session_69`
#        in `crates/pipeline/src/fetch_executor.rs`.
#
#   2. cargo check -p situation_room-desktop
#      — type-checks the Tauri crate (the Session 69 frontend edits
#        live entirely in `apps/desktop/src/stores/plans.svelte.ts`; the
#        backend exposes nothing new at the IPC boundary, so this is
#        just the standard "did anything break" smoke check).
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/2] cargo test --workspace"
(cargo test --workspace 2>&1; echo "EXIT=$?") \
  | tee "logs/session69-cargo-test-${STAMP}.log"

echo
echo "[2/2] cargo check -p situation_room-desktop"
(cargo check -p situation_room-desktop 2>&1; echo "EXIT=$?") \
  | tee "logs/session69-cargo-check-desktop-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session69-cargo-test-${STAMP}.log"
echo "  logs/session69-cargo-check-desktop-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Svelte frontend check (Session 69 store edits):"
echo "  cd apps/desktop && npm run check"
echo
echo "Once the test log is green, exercise the live path:"
echo "  cargo run -p situation_room-desktop"
echo "Then:"
echo "  - classify a topic (existing flow)"
echo "  - run fetch on the accepted plan"
echo "  - confirm DOCUMENTS bucket shows N rows (one per successful recipe)"
echo "  - classify a second topic from the topic input"
echo "    → right pane should drop to the home empty state during the wait"
echo "    → new plan lands with empty buckets"
echo "  - run fetch on the new plan"
echo "    → DOCUMENTS bucket fills with the new plan's URLs"
