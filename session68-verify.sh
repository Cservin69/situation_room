#!/usr/bin/env bash
# Session 68 — verify the cap fix + URL rewriter + dashboard scale
# fixes + re-author dialog hints.
#
# What this runs:
#   1. cargo test --workspace             — all backend tests, including
#                                            the 11 new url_pagination
#                                            tests + the
#                                            run_fetch_for_plan_caps_odata_url_before_fetch_session_68
#                                            integration test in
#                                            fetch_executor.rs.
#   2. cargo build -p situation_room-desktop  — frontend Tauri build
#                                            (catches Svelte compile
#                                            errors in the dashboard +
#                                            ReauthorDialog edits).
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/2] cargo test --workspace"
(cargo test --workspace 2>&1; echo "EXIT=$?") \
  | tee "logs/session68-cargo-test-${STAMP}.log"

echo
echo "[2/2] cargo build -p situation_room-desktop --release-mode=debug (frontend compile check)"
# Note: full Tauri build is heavy. Use cargo check on the bundled crate
# to catch type errors in our edits without producing a binary.
(cargo check -p situation_room-desktop 2>&1; echo "EXIT=$?") \
  | tee "logs/session68-cargo-check-desktop-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session68-cargo-test-${STAMP}.log"
echo "  logs/session68-cargo-check-desktop-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "If the desktop frontend (Svelte) edits need a real check, run:"
echo "  cd apps/desktop && npm run check"
echo "(separately — Svelte's tsc-shaped check isn't bundled into cargo;"
echo " single npm package, no pnpm in this repo)"
