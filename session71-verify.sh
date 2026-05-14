#!/usr/bin/env bash
# Session 71 — verify the chart-regression fix:
#   1. Backend: raise structured-body cap from 32 KiB to 128 KiB so
#      JSON/CSV/XML feeds (chart parser inputs) don't truncate
#      mid-array on realistic 2y daily-bars time series.
#      (crates/pipeline/src/document_synth.rs)
#   2. Frontend: defensive truncation recovery + DevTools diagnostic
#      in `RecordsDashboard.detectTimeSeriesShape`.
#      (apps/desktop/src/components/RecordsDashboard.svelte)
#
# What this runs:
#   [1/2] cargo test -p situation_room-pipeline
#         — exercises the new tests in document_synth.rs:
#           * is_structured_text_mime_matches_json_csv_xml
#           * is_structured_text_mime_rejects_html_plain_binary
#           * body_preview_json_uses_structured_cap_not_text_cap
#           * body_preview_json_still_caps_at_structured_cap
#           * body_preview_text_plain_keeps_small_cap
#           * body_preview_csv_uses_structured_cap
#           * body_preview_yahoo_chart_shape_preserves_timestamps_and_close
#             (the regression guard — fails if a 2y daily-bars-shaped
#              payload can't round-trip parse after the cap)
#
#   [2/2] cargo check -p situation_room-desktop
#         — type-checks the desktop crate so the
#           `recoverTruncatedJson` + diagnostic changes compile.
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/2] cargo test -p situation_room-pipeline document_synth"
(cargo test -p situation_room-pipeline document_synth 2>&1; echo "EXIT=$?") \
  | tee "logs/session71-cargo-test-${STAMP}.log"

echo
echo "[2/2] cargo check -p situation_room-desktop"
(cargo check -p situation_room-desktop 2>&1; echo "EXIT=$?") \
  | tee "logs/session71-cargo-check-desktop-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session71-cargo-test-${STAMP}.log"
echo "  logs/session71-cargo-check-desktop-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Frontend type-check (Session 71 dashboard edits):"
echo "  cd apps/desktop && npm run check"
echo
echo "Live exercise — confirm the chart returns:"
echo "  cargo run -p situation_room-desktop"
echo "  classify a 'tesla stock price' plan (or use an existing one)"
echo "  run fetch; on the records dashboard, the data_feed Document"
echo "  tile should render an inline sparkline + 'close · TSLA · N pts'"
echo "  caption again. Pre-Session-71 it showed the raw JSON sample."
echo
echo "If the chart is still missing on a fresh fetch, open DevTools"
echo "and look for the situation_room: chart preview ... warning —"
echo "the diagnostic surfaces parse-failure modes the cap-bump may"
echo "not have caught."
echo
echo "Optional — verify the cap fix against your live DB:"
echo "  duckdb situation_room.duckdb \\"
echo "    \"SELECT length(body) AS body_len, source_url FROM documents \\"
echo "     WHERE source_url LIKE '%query1.finance%' \\"
echo "     ORDER BY observed_at DESC LIMIT 5;\""
echo "Pre-Session-71 these rows were near 32768 bytes (cap hit)."
echo "Post-Session-71 they should be 35-50 KiB (Yahoo's real size)."
