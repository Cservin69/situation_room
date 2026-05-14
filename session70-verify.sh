#!/usr/bin/env bash
# Session 70 — verify the four ships:
#   1. HTML body strip for article-kind Documents (Rust unit tests)
#   2. Document-detail drawer modal (Svelte type-check)
#   3. ADR 0009 amendment 2: per-request UA + UaPolicy enum
#      (Rust unit tests in pipeline + secure)
#   4. (separate runbook: session70-iter-verify.sh, operator-driven)
#
# What this runs:
#   [1/2] cargo test --workspace
#         — all backend tests, including:
#           * `crates/pipeline/src/document_synth.rs` new HTML-strip
#             tests (is_html_mime, strip_html_drops_*, decode_html_*,
#             body_preview_strips_a_realistic_homepage, …)
#           * `crates/pipeline/src/ua_policies.rs` (new module,
#             8 unit tests for the closed UaPolicy enum + mapping)
#           * `crates/secure/src/http.rs` new test
#             `get_with_headers_ua_routes_through_url_guard_session_70`
#
#   [2/2] cargo check -p situation_room-desktop
#         — type-checks the desktop crate. The Session 70 frontend
#           edits add `DocumentDrawer.svelte`, extend `KindCard.svelte`
#           with an optional onOpen prop, and wire the drawer into
#           `RecordsDashboard.svelte`. No new IPC commands.
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/2] cargo test --workspace"
(cargo test --workspace 2>&1; echo "EXIT=$?") \
  | tee "logs/session70-cargo-test-${STAMP}.log"

echo
echo "[2/2] cargo check -p situation_room-desktop"
(cargo check -p situation_room-desktop 2>&1; echo "EXIT=$?") \
  | tee "logs/session70-cargo-check-desktop-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session70-cargo-test-${STAMP}.log"
echo "  logs/session70-cargo-check-desktop-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Svelte frontend check (Session 70 component + dashboard edits):"
echo "  cd apps/desktop && npm run check"
echo
echo "Once the test log is green, exercise the live path:"
echo "  cargo run -p situation_room-desktop"
echo "Then:"
echo "  - Open an existing plan with Documents (e.g. the TESLA plan)"
echo "  - article-kind tiles now show plain article text, not raw HTML"
echo "  - click a Document KindCard → drawer opens with full body"
echo "  - data_feed tiles with JSON payloads pretty-print on open"
echo "  - Escape closes the drawer; backdrop click closes the drawer"
echo
echo "Iterator verification on non-TESLA topic (separate, opt-in,"
echo "consumes ~1 call/topic — budget-safe under \"calls only\"):"
echo "  bash session70-iter-verify.sh"
