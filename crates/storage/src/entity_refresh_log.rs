//! Entity refresh-event ring buffer — Session 99 candidate #4.
//!
//! Sn-98 #5 added tier-aware refresh semantics to
//! [`crate::Store::upsert_entity`]: when an incoming Entity carries a
//! strictly-richer provenance tier than the existing row, the row's
//! `kind` / `canonical_name` / `license` are updated in place. The
//! refresh fires silently — the operator's dashboard tile flips from
//! one display name to another between two polls with no audit trail.
//!
//! Sn-99 #4 surfaces those refreshes. This module provides an
//! in-memory ring buffer of recent refresh events; the storage layer
//! pushes into it from the upsert refresh branch, and the api layer
//! exposes a Tauri command that returns the snapshot for the
//! `EntityRefreshPanel` dashboard tile.
//!
//! Sn-100 #3 persists the ring to disk (table `entity_refresh_history`,
//! migration 0023) and seeds the in-memory ring from disk at boot.
//! Mirrors the Sn-86 `promote_history` shape: write through on every
//! refresh, prune to [`ENTITY_REFRESH_LOG_CAP`] at the insert site,
//! hydrate via [`Store::hydrate_entity_refresh_log`] after migration.
//!
//! ## Design choices
//!
//! - **Persisted, not just in-memory** (Sn-100 #3). Sn-99's in-memory
//!   posture left the strip empty across Cmd-Q + relaunch — the exact
//!   shape the operator hits between sessions. Persistence is best-
//!   effort: a storage failure warn-logs and the in-memory push still
//!   proceeds.
//! - **Ring buffer with hard cap.** Same shape as
//!   [`crate::promote_history::PROMOTE_HISTORY_CAP`] +
//!   `cost_ledger::TIMELINE_CAP`. A few-dozen entries fit an
//!   interactive session's refresh activity without unbounded growth.
//! - **Lives on [`crate::Store`].** The refresh branch is inside
//!   `Store::upsert_entity_with_tier`; the natural seat for its
//!   companion log is the same struct. Reads via
//!   `Store::entity_refresh_log_snapshot()`.
//! - **Closed-vocabulary event shape.** The struct fields are the
//!   minimal set the operator needs to see what changed: timestamp,
//!   business id, before/after canonical name + tier. No free-text
//!   reason string; the tier transition encodes the why.

use chrono::{DateTime, Utc};
use duckdb::params;
use tracing::warn;
use uuid::Uuid;

use crate::connection::Store;
use crate::entities::EntityProvenanceTier;
use crate::{Result, StorageError};

/// Cap on the per-process refresh-event ring buffer. 50 entries
/// matches [`crate::cost_ledger::TIMELINE_CAP`] — comfortably above
/// the per-fetch-run upper bound on tier-elevating refreshes (Lever A
/// only fires once per article-shape Document per fetch) while small
/// enough to stay trivial in binary memory.
///
/// Pinned at the storage-crate edge so the api layer's wire-shape
/// caller (`entity_refresh_log` Tauri command) reads the same constant
/// the writer uses, avoiding cross-crate drift.
pub const ENTITY_REFRESH_LOG_CAP: usize = 50;

/// One in-memory record of an in-place refresh on the `entities`
/// table. Pushed onto the ring buffer by
/// [`crate::Store::upsert_entity_with_tier`] when an incoming row
/// strictly elevates the stored tier. `Inserted` and same-tier
/// `NoOp` upserts do NOT push — the log surfaces *changes* operators
/// can't otherwise see.
#[derive(Debug, Clone)]
pub struct EntityRefreshEvent {
    /// When the refresh write committed. Operator-visible as the
    /// strip's time column.
    pub at: DateTime<Utc>,
    /// Entity business id (the UNIQUE `entities.entity_id` column).
    /// Stable across refreshes so the operator can correlate strip
    /// rows with a card on the dashboard.
    pub entity_id: String,
    /// `canonical_name` before the refresh. Useful for "wait, why did
    /// this row's name change?" — the operator sees both sides
    /// without diffing two snapshots themselves.
    pub previous_canonical_name: String,
    /// `canonical_name` after the refresh. Always strictly richer in
    /// the ordering picked by [`EntityProvenanceTier`].
    pub new_canonical_name: String,
    /// Pre-refresh tier (the existing row's license-derived bucket).
    pub previous_tier: EntityProvenanceTier,
    /// Post-refresh tier (the incoming row's tier, strictly greater).
    pub new_tier: EntityProvenanceTier,
}

impl EntityRefreshEvent {
    /// Did the canonical_name actually change? Tier elevation
    /// without a name change is legal (e.g. two pipelines independently
    /// land on the same display string) but rare — surface it as a
    /// "tier-only" hint when the dashboard renders the row.
    pub fn name_changed(&self) -> bool {
        self.previous_canonical_name != self.new_canonical_name
    }
}

// ---------------------------------------------------------------------------
// Tier ↔ string mapping for the SQL boundary (Sn-100 #3)
// ---------------------------------------------------------------------------

/// Closed-vocabulary tier strings the storage layer accepts in the
/// `entity_refresh_history.previous_tier` / `new_tier` columns.
///
/// Kept in lockstep with [`EntityProvenanceTier`]; the round-trip
/// helpers below are the authoritative conversion. Storage rejects rows
/// with a tier outside this set on load — same posture migration 0017
/// takes for `trigger`.
pub const TIER_STRINGS: &[&str] = &[
    "unknown",
    "recipe_iterator",
    "slug_humanised",
    "document_extracted",
];

/// Map a closed-vocab [`EntityProvenanceTier`] to its SQL string. The
/// inverse is [`tier_from_str`]. Storage writes go through this so a
/// future variant addition surfaces as a compile error rather than a
/// silent string drift.
pub fn tier_as_str(tier: EntityProvenanceTier) -> &'static str {
    match tier {
        EntityProvenanceTier::Unknown => "unknown",
        EntityProvenanceTier::RecipeIterator => "recipe_iterator",
        EntityProvenanceTier::SlugHumanised => "slug_humanised",
        EntityProvenanceTier::DocumentExtracted => "document_extracted",
    }
}

/// Parse a SQL tier string back into [`EntityProvenanceTier`]. Returns
/// `None` for any string outside [`TIER_STRINGS`] so the load path can
/// surface the offending value rather than silently coercing.
pub fn tier_from_str(s: &str) -> Option<EntityProvenanceTier> {
    match s {
        "unknown" => Some(EntityProvenanceTier::Unknown),
        "recipe_iterator" => Some(EntityProvenanceTier::RecipeIterator),
        "slug_humanised" => Some(EntityProvenanceTier::SlugHumanised),
        "document_extracted" => Some(EntityProvenanceTier::DocumentExtracted),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Persistence — write + load against migration 0023's table (Sn-100 #3)
// ---------------------------------------------------------------------------

impl Store {
    /// Insert one refresh-history row, then prune the table to the
    /// most recent `cap` rows (ordered by `recorded_at DESC`). Mirrors
    /// [`Self::insert_promote_history_entry`]'s shape — see migration
    /// 0023 for the table rationale.
    ///
    /// `cap` should match [`ENTITY_REFRESH_LOG_CAP`]. `cap == 0` is the
    /// defensive misconfiguration case: the insert + prune still run,
    /// but the prune leaves an empty table.
    pub fn insert_entity_refresh_history_entry(
        &self,
        event: &EntityRefreshEvent,
        cap: usize,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO entity_refresh_history
              (id, recorded_at, entity_id, previous_canonical_name,
               new_canonical_name, previous_tier, new_tier)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                Uuid::now_v7(),
                event.at,
                event.entity_id,
                event.previous_canonical_name,
                event.new_canonical_name,
                tier_as_str(event.previous_tier),
                tier_as_str(event.new_tier),
            ],
        )
        .map_err(StorageError::DuckDb)?;

        // Prune. Same OFFSET arithmetic + cap==0 defensive branch the
        // promote_history insert uses — see that module's doc-comment
        // for the rationale.
        if cap == 0 {
            conn.execute("DELETE FROM entity_refresh_history", [])
                .map_err(StorageError::DuckDb)?;
        } else {
            conn.execute(
                "DELETE FROM entity_refresh_history
                 WHERE recorded_at < (
                     SELECT recorded_at FROM entity_refresh_history
                     ORDER BY recorded_at DESC
                     OFFSET ? LIMIT 1
                 )",
                params![(cap - 1) as i64],
            )
            .map_err(StorageError::DuckDb)?;
        }

        Ok(())
    }

    /// Load the most recent `limit` refresh-history rows, ordered
    /// newest-first. Used by [`Self::hydrate_entity_refresh_log`] at
    /// boot.
    ///
    /// Returns an empty vec when the table is empty (the common cold-
    /// boot case). Rows with tier strings outside [`TIER_STRINGS`] are
    /// surfaced as a hard error rather than coerced — same posture
    /// the `promote_history` load path takes for `trigger`.
    pub fn load_recent_entity_refresh_history(
        &self,
        limit: usize,
    ) -> Result<Vec<EntityRefreshEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT recorded_at, entity_id, previous_canonical_name,
                        new_canonical_name, previous_tier, new_tier
                 FROM entity_refresh_history
                 ORDER BY recorded_at DESC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let at: DateTime<Utc> = row.get(0)?;
                let entity_id: String = row.get(1)?;
                let previous_canonical_name: String = row.get(2)?;
                let new_canonical_name: String = row.get(3)?;
                let previous_tier_s: String = row.get(4)?;
                let new_tier_s: String = row.get(5)?;
                Ok((
                    at,
                    entity_id,
                    previous_canonical_name,
                    new_canonical_name,
                    previous_tier_s,
                    new_tier_s,
                ))
            })
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::with_capacity(limit);
        for row in rows {
            let (at, eid, prev_name, new_name, prev_tier_s, new_tier_s) =
                row.map_err(StorageError::DuckDb)?;
            let previous_tier = tier_from_str(&prev_tier_s).ok_or_else(|| {
                StorageError::Other(format!(
                    "entity_refresh_history: unknown previous_tier {prev_tier_s:?}"
                ))
            })?;
            let new_tier = tier_from_str(&new_tier_s).ok_or_else(|| {
                StorageError::Other(format!(
                    "entity_refresh_history: unknown new_tier {new_tier_s:?}"
                ))
            })?;
            out.push(EntityRefreshEvent {
                at,
                entity_id: eid,
                previous_canonical_name: prev_name,
                new_canonical_name: new_name,
                previous_tier,
                new_tier,
            });
        }
        Ok(out)
    }

    /// Sn-100 #3 — seed the in-memory ring from the persisted
    /// `entity_refresh_history` table. Idempotent: every call replaces
    /// the ring's contents with the disk view, capped at
    /// [`ENTITY_REFRESH_LOG_CAP`]. The composition root calls this
    /// once at boot after `Store::migrate`; tests may call it ad-hoc.
    ///
    /// Storage returns newest-first; the in-memory ring is oldest-first
    /// (the snapshot caller reverses for display, matching the
    /// `CostTimelinePanel` convention) so we push in reverse here.
    ///
    /// Failure modes are non-fatal: a load error warn-logs and leaves
    /// the ring empty. The product is still functional with an empty
    /// strip; persistence is recovery-only.
    pub fn hydrate_entity_refresh_log(&self) {
        let rows = match self.load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP) {
            Ok(rs) => rs,
            Err(e) => {
                warn!(
                    error = %e,
                    "entity_refresh_log hydrate: load failed; starting with empty ring"
                );
                return;
            }
        };

        if rows.is_empty() {
            return;
        }

        let mut guard = match self.entity_refresh_log.lock() {
            Ok(g) => g,
            Err(poison) => {
                warn!(
                    "entity_refresh_log hydrate: in-memory ring mutex poisoned on cold boot \
                     — recovering"
                );
                poison.into_inner()
            }
        };
        guard.clear();
        // Disk view is newest-first; reverse so the ring stays
        // oldest-first (matching `entity_refresh_log_snapshot`'s
        // contract).
        for ev in rows.into_iter().rev() {
            guard.push_back(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_changed_returns_true_when_strings_differ() {
        let ev = EntityRefreshEvent {
            at: Utc::now(),
            entity_id: "company:tsla".into(),
            previous_canonical_name: "tsla".into(),
            new_canonical_name: "Tesla, Inc.".into(),
            previous_tier: EntityProvenanceTier::SlugHumanised,
            new_tier: EntityProvenanceTier::DocumentExtracted,
        };
        assert!(ev.name_changed());
    }

    #[test]
    fn name_changed_returns_false_when_strings_match() {
        // Edge case: two pipelines independently produced the same
        // display name but at different tiers. The tier elevation is
        // real (governs future refresh behaviour) but the operator's
        // eye won't see a visible change in the panel — the panel
        // can render a "tier-only" hint instead of a `→` arrow.
        let ev = EntityRefreshEvent {
            at: Utc::now(),
            entity_id: "company:catl".into(),
            previous_canonical_name: "CATL".into(),
            new_canonical_name: "CATL".into(),
            previous_tier: EntityProvenanceTier::SlugHumanised,
            new_tier: EntityProvenanceTier::DocumentExtracted,
        };
        assert!(!ev.name_changed());
    }

    #[test]
    fn entity_refresh_log_cap_is_at_least_one_fetch_run_worth() {
        // Pin the cap rationale: a single fetch run currently triggers
        // at most a handful of refreshes (one per article-shape Doc
        // that names a previously-slug-only entity). 50 covers many
        // runs' worth without growing unboundedly.
        assert!(
            ENTITY_REFRESH_LOG_CAP >= 20,
            "cap must hold at least an interactive operator session's refreshes"
        );
    }

    // -------------------------------------------------------------------
    // Sn-100 #3 — tier ↔ string round-trip + persistence
    // -------------------------------------------------------------------

    #[test]
    fn tier_round_trips_through_str_and_back() {
        // Pin every closed-vocab variant; if a future variant is added
        // without updating `tier_as_str` / `tier_from_str`, this test
        // fails at the new variant.
        for tier in [
            EntityProvenanceTier::Unknown,
            EntityProvenanceTier::RecipeIterator,
            EntityProvenanceTier::SlugHumanised,
            EntityProvenanceTier::DocumentExtracted,
        ] {
            let s = tier_as_str(tier);
            assert_eq!(tier_from_str(s), Some(tier), "round-trip for {tier:?}");
            assert!(
                TIER_STRINGS.contains(&s),
                "tier_as_str output {s:?} must be in TIER_STRINGS"
            );
        }
    }

    #[test]
    fn tier_from_str_rejects_unknown_string() {
        assert_eq!(tier_from_str("auto_after_fetch"), None);
        assert_eq!(tier_from_str(""), None);
        assert_eq!(tier_from_str("DocumentExtracted"), None); // wrong case
    }

    #[test]
    fn insert_and_load_round_trip_one_row() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let ev = EntityRefreshEvent {
            at: Utc::now(),
            entity_id: "company:tsla".into(),
            previous_canonical_name: "tsla".into(),
            new_canonical_name: "Tesla, Inc.".into(),
            previous_tier: EntityProvenanceTier::SlugHumanised,
            new_tier: EntityProvenanceTier::DocumentExtracted,
        };
        store
            .insert_entity_refresh_history_entry(&ev, ENTITY_REFRESH_LOG_CAP)
            .expect("persist refresh event");
        let loaded = store
            .load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP)
            .expect("load refresh history");
        assert_eq!(loaded.len(), 1);
        let back = &loaded[0];
        assert_eq!(back.entity_id, "company:tsla");
        assert_eq!(back.previous_canonical_name, "tsla");
        assert_eq!(back.new_canonical_name, "Tesla, Inc.");
        assert_eq!(back.previous_tier, EntityProvenanceTier::SlugHumanised);
        assert_eq!(back.new_tier, EntityProvenanceTier::DocumentExtracted);
    }

    #[test]
    fn load_returns_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let t0 = Utc::now();
        let mk = |secs: i64, eid: &str| EntityRefreshEvent {
            at: t0 + chrono::Duration::seconds(secs),
            entity_id: eid.into(),
            previous_canonical_name: "old".into(),
            new_canonical_name: "new".into(),
            previous_tier: EntityProvenanceTier::SlugHumanised,
            new_tier: EntityProvenanceTier::DocumentExtracted,
        };
        // Insert in non-monotonic order so the ORDER BY does the work.
        store
            .insert_entity_refresh_history_entry(&mk(10, "mid"), ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        store
            .insert_entity_refresh_history_entry(&mk(100, "new"), ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        store
            .insert_entity_refresh_history_entry(&mk(1, "old"), ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        let loaded = store
            .load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].entity_id, "new");
        assert_eq!(loaded[1].entity_id, "mid");
        assert_eq!(loaded[2].entity_id, "old");
    }

    #[test]
    fn insert_prunes_table_to_cap() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let t0 = Utc::now();
        let cap = 3usize;
        for i in 0..5 {
            let ev = EntityRefreshEvent {
                at: t0 + chrono::Duration::seconds(i as i64),
                entity_id: format!("e{i}"),
                previous_canonical_name: "old".into(),
                new_canonical_name: "new".into(),
                previous_tier: EntityProvenanceTier::SlugHumanised,
                new_tier: EntityProvenanceTier::DocumentExtracted,
            };
            store
                .insert_entity_refresh_history_entry(&ev, cap)
                .unwrap();
        }
        // Only the 3 newest survive on disk.
        let loaded = store
            .load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        assert_eq!(loaded.len(), cap);
        // Newest-first order — e4, e3, e2.
        let ids: Vec<&str> = loaded.iter().map(|e| e.entity_id.as_str()).collect();
        assert_eq!(ids, vec!["e4", "e3", "e2"]);
    }

    #[test]
    fn insert_with_cap_zero_leaves_empty_table() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let ev = EntityRefreshEvent {
            at: Utc::now(),
            entity_id: "x".into(),
            previous_canonical_name: "a".into(),
            new_canonical_name: "b".into(),
            previous_tier: EntityProvenanceTier::SlugHumanised,
            new_tier: EntityProvenanceTier::DocumentExtracted,
        };
        store
            .insert_entity_refresh_history_entry(&ev, 0)
            .unwrap();
        let loaded = store
            .load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP)
            .unwrap();
        assert!(loaded.is_empty(), "cap=0 must leave the table empty");
    }

    #[test]
    fn hydrate_seeds_ring_from_disk() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let t0 = Utc::now();
        for i in 0..3 {
            let ev = EntityRefreshEvent {
                at: t0 + chrono::Duration::seconds(i as i64),
                entity_id: format!("e{i}"),
                previous_canonical_name: "slug".into(),
                new_canonical_name: "Prose".into(),
                previous_tier: EntityProvenanceTier::SlugHumanised,
                new_tier: EntityProvenanceTier::DocumentExtracted,
            };
            store
                .insert_entity_refresh_history_entry(&ev, ENTITY_REFRESH_LOG_CAP)
                .unwrap();
        }
        // Pre-hydrate: in-memory ring empty.
        assert!(store.entity_refresh_log_snapshot().is_empty());
        store.hydrate_entity_refresh_log();
        let snap = store.entity_refresh_log_snapshot();
        assert_eq!(snap.len(), 3);
        // Snapshot is oldest-first per its contract.
        let ids: Vec<&str> = snap.iter().map(|e| e.entity_id.as_str()).collect();
        assert_eq!(ids, vec!["e0", "e1", "e2"]);
    }

    #[test]
    fn hydrate_on_empty_table_leaves_ring_empty() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.hydrate_entity_refresh_log();
        assert!(store.entity_refresh_log_snapshot().is_empty());
    }

    #[test]
    fn load_rejects_unknown_tier_string_in_row() {
        // Hand-edit a tier string outside the closed-vocab to confirm
        // the load path errors rather than silently coercing.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO entity_refresh_history
                  (id, recorded_at, entity_id, previous_canonical_name,
                   new_canonical_name, previous_tier, new_tier)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    Uuid::now_v7(),
                    Utc::now(),
                    "x",
                    "old",
                    "new",
                    "weird-tier-string",
                    "document_extracted",
                ],
            )
            .unwrap();
        }
        let err = store
            .load_recent_entity_refresh_history(ENTITY_REFRESH_LOG_CAP)
            .expect_err("unknown previous_tier must error on load");
        let msg = err.to_string();
        assert!(
            msg.contains("weird-tier-string"),
            "error should name the bad value, got {msg}"
        );
    }
}
