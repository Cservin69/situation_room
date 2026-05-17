#!/usr/bin/env bash
# Session 89 — DATABASE-ONLY failure-analysis runbook.
#
# Sandbox-side blocker: the Linux sandbox can't run DuckDB
# (PyPI/GitHub/duckdb.org blocked by proxy; the libduckdb.a in
# target/ is Mach-O arm64 — Mac-only). All queries land on Mac.
#
# Usage on Mac:
#
#   cd /Users/aben/Documents/Claude/Projects/SituationRoom
#   bash session89-runbook.sh
#
# Outputs:
#
#   session89-results.txt   — verbatim DuckDB output from analyze.sql
#   session89-results.log   — same with timestamps for replay
#
# Then back-sync. Session 90 reads session89-results.txt and picks the
# code-path tasks documented at the bottom of SESSION_89_HANDOFF.md.

set -euo pipefail

DB="${DB:-./situation_room.duckdb}"
SQL="./session89-analyze.sql"
OUT="./session89-results.txt"
LOG="./session89-results.log"

if [[ ! -f "$DB" ]]; then
    echo "ERROR: $DB not found. Edit the DB= env var or cd to the right folder." >&2
    exit 1
fi

if [[ ! -f "$SQL" ]]; then
    echo "ERROR: $SQL not found alongside this script." >&2
    exit 1
fi

if ! command -v duckdb >/dev/null 2>&1; then
    echo "ERROR: duckdb CLI not in PATH. Install: brew install duckdb" >&2
    exit 1
fi

echo "session89-runbook: writing $OUT and $LOG"
echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] session89-runbook start" | tee "$LOG"
echo "DB:  $DB"  | tee -a "$LOG"
echo "SQL: $SQL" | tee -a "$LOG"

# Read-only attach so we don't accidentally take a write lock if the
# desktop app happens to be running. DuckDB's CLI supports `-readonly`.
duckdb -readonly "$DB" < "$SQL" > "$OUT" 2>&1 || {
    rc=$?
    echo "duckdb exited with status $rc" | tee -a "$LOG"
    echo "(partial output is still in $OUT; inspect manually)" | tee -a "$LOG"
    exit "$rc"
}

echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] session89-runbook done — $(wc -l <"$OUT") lines captured" | tee -a "$LOG"
echo
echo "Next: review $OUT, then in Session 90 the kickoff message can just"
echo "paste the headline numbers (A3 + C1 + F4 + J1 are the load-bearing"
echo "four). The handoff names the candidate-task ranking heuristic."
