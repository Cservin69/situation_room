-- situation_room schema, version 0017.
--
-- Promote-pass history — Session 86.
--
-- ## Why this exists
--
-- Session 85 turned `AppState::last_promote_summary` into a ring buffer
-- (`VecDeque<LastPromoteSummary>`, cap PROMOTE_HISTORY_CAP=20) so the
-- dashboard's PromoteStatusPanel could render a "last N passes" strip.
-- That ring is in-memory only — after a Cmd-Q + relaunch the strip
-- shows empty until a new promote pass writes the first entry.
--
-- Persisting the strip across binary restarts solves two operator-
-- visible product gaps:
--
--   1. Reviewing yesterday's promote activity. The session-handoff
--      pattern is "operator quits app at the end of work, picks up the
--      next day." Without persistence the strip is always "starting
--      fresh" — losing the signal the ring-buffer dashboard was added
--      to surface.
--
--   2. Cross-plan trigger counter (Session 85's "Y runs in the last
--      60s") needs >1 entry within the rolling window to fire. On a
--      fresh boot this counter resets even if multiple plans ran in
--      the last minute on the previous binary.
--
-- ## Why a small JSON-blob table, not row-per-counter
--
-- The PromoteReport struct (9× u32 counters today) already derives
-- Serialize/Deserialize. Stuffing the full struct into a JSON column
-- preserves the schema as a single source of truth (`crates/pipeline/
-- src/promote.rs`); changing the counter set in a future session is a
-- Rust-side edit only — no migration to rename a column. Reads in this
-- module are bounded by PROMOTE_HISTORY_CAP rows, so an indexed JSON
-- column doesn't pay rent that a row-per-counter shape would.
--
-- The alternative (one column per counter) would force a new migration
-- on every PromoteReport field addition, which is exactly the pattern
-- ADR 0019's "extracted_inner" arc was trying to avoid for record
-- content. Same logic applies here: shapes that the Rust crate owns
-- shouldn't bleed into migration churn.
--
-- ## Why a `cap_marker` row isn't here
--
-- The cap is enforced at write time: every insert is followed by a
-- DELETE for rows beyond PROMOTE_HISTORY_CAP ordered by `at DESC`. The
-- alternative (a `latest_n` view, or a TRIGGER) ties the cap to the
-- database schema, which means changing PROMOTE_HISTORY_CAP would need
-- a migration. Today it's an `usize` constant on the api crate; the
-- write-then-prune posture lets it stay there.
--
-- ## Closed-vocabulary discipline
--
-- `trigger` is `'auto_after_fetch' | 'manual'`, mirroring the
-- `LastPromoteTrigger::as_str()` strings the Tauri command surface
-- already uses. Rust-side parsing is strict (no Unknown fallback);
-- the column accepts anything but the load path returns a hard error
-- on a string outside the closed set — same posture migration 0016
-- takes for `outcome_kind`.
--
-- ## What's NOT in scope
--
-- - Persistence of `LlmCostLedger` (Session 81 ring buffer, same
--   pattern). That'd be a parallel migration; not load-bearing for
--   the promote-strip product gap.
-- - Cross-plan analytics views over promote history (e.g. "which
--   plans triggered most promote passes this week"). The strip is
--   an at-a-glance dashboard widget; deeper queries are future work.
-- - Pre-Session-86 in-memory entries from a running binary — those
--   are lost on the upgrade boot. This is acceptable because the
--   strip is recovery-only (no record-correctness depends on it).
--
-- Columns:
--   id           UUIDv7 primary key.
--   plan_id      The plan whose promote pass this entry describes.
--   recorded_at  When the pass completed. The strip orders by this DESC.
--                Note: column name is `recorded_at` (not `at`) because
--                DuckDB's parser reserves `AT` for `AT TIME ZONE` etc.
--                The in-memory `LastPromoteSummary.at` field name stays;
--                only the SQL column is renamed.
--   trigger      Closed enum — see above.
--   report       Full PromoteReport struct serialized as JSON text.
--                Decoded via serde_json::from_str on load.

CREATE TABLE IF NOT EXISTS promote_history (
    id          UUID        PRIMARY KEY,
    plan_id     UUID        NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    trigger     TEXT        NOT NULL,
    report      TEXT        NOT NULL
);

-- The strip's primary read: newest-first within
-- PROMOTE_HISTORY_CAP. The composite index keeps the load-on-boot
-- query at constant cost regardless of historical-row count.
CREATE INDEX IF NOT EXISTS idx_promote_history_recorded_at
    ON promote_history(recorded_at DESC);

-- Record this migration.
INSERT INTO schema_migrations (version, description)
    VALUES (17, 'promote_history table');
