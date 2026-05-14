#!/usr/bin/env bash
# Session 73 — verify the Document-drawer table viewer:
#   - apps/desktop/src/lib/dashboard/document_table.ts   (new)
#   - apps/desktop/src/lib/dashboard/document_table.test.mjs  (new)
#   - apps/desktop/src/components/DocumentTable.svelte   (new)
#   - apps/desktop/src/components/DocumentDrawer.svelte  (wired)
#
# What this runs:
#   [1/3] tsc --noEmit on the detector module (catches type drift
#         without depending on svelte-check's plugin chain).
#   [2/3] node-driven regression test against the compiled detector.
#         Covers: detectTableShape (bare-array, OData, FEMA, mixed,
#         largest-wins, freq-ordered cols, malformed/empty/non-JSON),
#         renderCell (primitives, null, object cap, string passthrough),
#         isNumericColumn (numeric/mixed/empty/null-skip),
#         nextSortDirection (3-state cycle), compareCells (numeric/
#         string/null-last). Exits non-zero on any failure.
#   [3/3] cd apps/desktop && npm run check
#         — full svelte-check pass covering DocumentTable.svelte +
#           DocumentDrawer.svelte wiring + the rest of the frontend.
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/3] tsc compile of document_table.ts → /tmp/dt-build"
# Build to /tmp/dt-build (matching the test's hardcoded import path).
# We emit JS rather than --noEmit because stage 2 imports the
# compiled .js from the same dir — the act of compiling *is* the
# type-check stage here (tsc errors fail the build and propagate
# via EXIT=$?).
rm -rf /tmp/dt-build
(cd apps/desktop && \
  ./node_modules/.bin/tsc \
    --target es2022 --moduleResolution bundler --module esnext \
    --strict --skipLibCheck \
    --outDir /tmp/dt-build \
    src/lib/dashboard/document_table.ts 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session73-tsc-${STAMP}.log"

echo
echo "[2/3] node detector regression test"
(cd apps/desktop && \
  node src/lib/dashboard/document_table.test.mjs 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session73-detector-test-${STAMP}.log"

echo
echo "[3/3] svelte-check"
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") \
  | tee "logs/session73-svelte-check-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session73-tsc-${STAMP}.log"
echo "  logs/session73-detector-test-${STAMP}.log"
echo "  logs/session73-svelte-check-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Live exercise — confirm the table renders:"
echo "  cargo run -p situation_room-desktop"
echo "  open or classify a plan whose recipe fetches an array-of-"
echo "  objects payload (FEMA disaster declarations is the canonical"
echo "  case; OData-shaped feeds work too: {\"value\":[{...},{...}]})."
echo "  Run a fetch. On the records dashboard, find the data_feed or"
echo "  api Document KindCard and click it."
echo
echo "  Pre-Session-73: drawer renders pretty-printed JSON only."
echo "  Post-Session-73: drawer renders a sortable table at the top"
echo "    with row + column counts in the caption (e.g."
echo "    \"DisasterDeclarationsSummaries  500 rows · 23 cols\"),"
echo "    then a \"show raw JSON\" toggle, with the raw <pre> tucked"
echo "    behind it."
echo
echo "  Click any column header → sorts asc; click again → desc;"
echo "  click again → no sort (original order). Numeric-only columns"
echo "  sort numerically and right-align; mixed/string columns sort"
echo "  lexicographically and left-align."
echo
echo "  Time-series payloads (Tesla / FRED / etc.) keep the chart"
echo "  preview behaviour from Session 69 — table detection skips"
echo "  when chartSeries is non-null. Verify by opening a Tesla"
echo "  data_feed Document; you should see the sparkline + raw JSON"
echo "  block, no table."
