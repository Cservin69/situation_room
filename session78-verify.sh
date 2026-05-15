#!/usr/bin/env bash
# Session 78 verification — three-stage gate + one-time DB cleanup.
#
#   Stage 0: SQL cleanup for v1 poison Assertion rows (Session 77 wrote
#            `AssertedContent::Relation` JSON with duplicate `"kind"`
#            keys; the Session-78 schema fix in content.rs renames the
#            discriminator to `asserted_kind`, the decode-tolerance fix
#            in queries.rs keeps the dashboard alive, and this stage
#            flushes the historical rows so the table stays tidy).
#            Operator-driven; opt out by deleting the block below if
#            the table is empty.
#   Stage 1: cargo check on the workspace + all-targets. Catches the
#            `ExecutorContext::document_events_prompt` rollout, the
#            `AppState::new` signature change, the new
#            `crates/pipeline/src/extract.rs::extract_and_persist_events`
#            entry point, and the `AssertedContent` tag rename in
#            content.rs.
#   Stage 2: cargo test on the four crates that carry new logic
#            (pipeline + storage + api + llm + core). The
#            `relation_assertion_roundtrips_without_duplicate_keys`
#            test is the regression guard for the duplicate-`kind`
#            bug.
#   Stage 3: svelte-check on apps/desktop. Catches the AssertionDto
#            doc-comment + recordSummary doc-comment updates and any
#            knock-on TS-generated drift.
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

stage0="logs/session78-db-cleanup-${TS}.log"
stage1="logs/session78-cargo-check-${TS}.log"
stage2="logs/session78-cargo-test-${TS}.log"
stage3="logs/session78-svelte-check-${TS}.log"

# ---------------------------------------------------------------------------
# Stage 0 — optional: flush v1 duplicate-`kind` poison Assertion rows.
# ---------------------------------------------------------------------------
# Session 77 wrote `AssertedContent::Relation` to disk as
#   {"kind":"relation","kind":"supplies_to","from":"...","to":"..."}
# That JSON re-parses as duplicate-field error. The Session-78 schema
# fix prevents new rows from carrying the bug; existing poison rows are
# tolerated at read time (decode_assertion_row logs+skips them) so the
# dashboard doesn't go red. This stage hard-deletes them so the
# assertions table doesn't accumulate dead rows.
#
# Heuristic: poison rows have `content` starting with `{"kind":"relation"`
# (the v1 tag prefix). Post-Session-78 rows start with
# `{"asserted_kind":"relation"` (the new tag). The LIKE pattern
# isolates exactly the v1 set without manual UUID hunting.

DB_PATH="${SITUATION_ROOM_DB:-$REPO_ROOT/situation_room.duckdb}"

echo "== Stage 0: DB cleanup of v1 duplicate-\`kind\` Assertion rows =="
{
  if [ ! -f "$DB_PATH" ]; then
    echo "DB not found at $DB_PATH; skipping stage 0 (nothing to clean)."
    echo "EXIT=0"
  elif ! command -v duckdb >/dev/null 2>&1; then
    echo "duckdb CLI not on PATH; skipping stage 0."
    echo "If you want the cleanup, install duckdb and re-run, OR run"
    echo "this SQL manually against $DB_PATH:"
    echo "  SELECT count(*) FROM assertions WHERE content LIKE '{\"kind\":\"relation\"%';"
    echo "  DELETE FROM assertions WHERE content LIKE '{\"kind\":\"relation\"%';"
    echo "EXIT=0"
  else
    echo "Before:"
    duckdb "$DB_PATH" \
      "SELECT count(*) AS poison_rows FROM assertions WHERE content LIKE '{\"kind\":\"relation\"%';"
    echo "Deleting…"
    duckdb "$DB_PATH" \
      "DELETE FROM assertions WHERE content LIKE '{\"kind\":\"relation\"%';"
    echo "After:"
    duckdb "$DB_PATH" \
      "SELECT count(*) AS remaining_poison FROM assertions WHERE content LIKE '{\"kind\":\"relation\"%';"
    echo "EXIT=$?"
  fi
} 2>&1 | tee "$stage0"

# ---------------------------------------------------------------------------
# Stage 1 — cargo check (workspace).
# ---------------------------------------------------------------------------
echo "== Stage 1: cargo check (workspace) =="
{
  cargo check --workspace --all-targets 2>&1
  echo "EXIT=$?"
} | tee "$stage1"

# ---------------------------------------------------------------------------
# Stage 2 — cargo test on the crates that carry new logic.
# ---------------------------------------------------------------------------
echo "== Stage 2: cargo test (pipeline + storage + api + llm + core) =="
{
  cargo test \
    --package situation_room-pipeline \
    --package situation_room-storage \
    --package situation_room-api \
    --package situation_room-llm \
    --package situation_room-core \
    --lib 2>&1
  echo "EXIT=$?"
} | tee "$stage2"

# ---------------------------------------------------------------------------
# Stage 3 — svelte-check.
# ---------------------------------------------------------------------------
echo "== Stage 3: svelte-check =="
{
  ( cd apps/desktop && npm run check 2>&1 )
  echo "EXIT=$?"
} | tee "$stage3"

echo ""
echo "Logs:"
echo "  Stage 0 (DB cleanup):   $stage0"
echo "  Stage 1 (cargo check):  $stage1"
echo "  Stage 2 (cargo test):   $stage2"
echo "  Stage 3 (svelte-check): $stage3"
echo ""
echo "Pass criteria: last line of each Stage 1/2/3 log is EXIT=0."
echo "Stage 0 is best-effort cleanup; non-zero there is informational only."
