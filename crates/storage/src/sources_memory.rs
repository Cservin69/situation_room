//! Sources memory — derived view over `recipes` ⨝
//! `recipe_fetch_attempts` ⨝ `research_plans` (ADR 0015, Session 37).
//!
//! This module replaces the static `Vec<SourceDescriptor>` the
//! classifier used to consume from `config/sources.toml`. The memory
//! is a *summary of past successes*: every URL the operator has
//! actually fetched against successfully appears here, with its
//! `source_id`, recency, success count, and the `topic_tags` of the
//! plans it appeared in.
//!
//! The classifier consumes the result through
//! `ClassificationContext::sources_memory`. It is **context, not
//! constraint**: the prompt teaches the LLM to use the memory when it
//! fits but to emit URLs from training knowledge when memory is empty
//! or doesn't cover the topic. ADR 0015 §"`{{REGISTERED_SOURCES}}`
//! becomes `{{SOURCES_MEMORY}}`".
//!
//! ## No migration required
//!
//! The query reads three tables that already exist:
//!
//!   - `recipes(plan_id, source_id, source_url, …)` — Migration 0003.
//!   - `recipe_fetch_attempts(recipe_id, succeeded, attempted_at, …)`
//!     — Migration 0013.
//!   - `research_plans(id, topic_tags_json, …)` — Migration 0004.
//!
//! ADR 0015 §"Storage" calls this out explicitly: the memory is a
//! read-only derived view, no schema change.
//!
//! ## Cap and ordering
//!
//! Top-N by `last_attempted_at` descending; the cap is 30. The cap is
//! coarser than the topics-in-use cap (200) because sources are
//! coarser than topics, and the prompt body has to fit the memory
//! injection alongside the rest of the context. ADR 0015 §"Memory
//! query".
//!
//! ## What an empty memory means
//!
//! A first-time installation with no successful fetches sees an empty
//! result here. The classifier prompt's worked examples teach the
//! cold-start pattern: emit URLs from training knowledge alone. Empty
//! memory is **honest**; the alternative (seeding from the deleted
//! `config/sources.toml` entries) would have lied about the audit
//! trail. ADR 0015 §"Configuration".
//!
//! ## Why unfiltered by topic
//!
//! The query is unfiltered by the user's topic. A topic-overlap
//! filter is plausible but creates a chicken-and-egg problem: at the
//! time the classifier reads the memory it has not yet picked
//! `topic_tags`, so the filter would have to match the user's literal
//! topic string heuristically against past plans' tags. Defer until
//! unfiltered top-30 stops fitting. ADR 0015 §"Memory query".

use chrono::{DateTime, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};

use crate::connection::Store;
use crate::{Result, StorageError};

/// Default cap for [`Store::sources_memory`] surfaced to the
/// classifier. ADR 0015 §"Memory query": 30 entries.
///
/// Sized for the prompt body — the classifier's
/// `{{SOURCES_MEMORY}}` substitution renders one bullet per entry; 30
/// is comfortably under the prompt body's 256 KiB ceiling while wide
/// enough to convey breadth across several recent plans.
pub const SOURCES_MEMORY_LIMIT: usize = 30;

/// One row in the sources memory the classifier consumes.
///
/// Mirrors ADR 0015 §"Memory query" exactly. The classifier crate
/// imports this type directly (the pipeline crate already depends on
/// storage; no parallel mirror needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySource {
    /// The URL the recipe fetched against successfully. Used to
    /// stamp `known_id` recognition in the LLM's emitted nominations
    /// — the classifier prompt teaches "stamp `known_id` when your
    /// emitted URL corresponds to a memory entry."
    pub endpoint_url: String,
    /// The `source_id` recorded on the recipe row. Today's stored
    /// recipes carry registry-shaped ids (`world_bank_indicators`,
    /// `usgs_mcs`); newly authored recipes after Session 37 carry
    /// either an LLM-stamped `known_id` or a host-derived id
    /// (`api.worldbank.org`, `apps.fas.usda.gov`). Both shapes are
    /// valid; the classifier prompt does not distinguish them.
    pub source_id: String,
    /// Total number of `recipe_fetch_attempts` rows for this
    /// (`source_url`, `source_id`) pair where `succeeded = true`. The
    /// memory only surfaces sources that have at least one success.
    pub successful_attempts: u32,
    /// Timestamp of the most recent successful fetch attempt.
    /// `ORDER BY last_attempted_at DESC` is what makes the listing
    /// "recency-sorted top 30."
    pub last_attempted_at: DateTime<Utc>,
    /// Distinct topic-tag strings drawn from every plan whose recipes
    /// fetched this `(source_url, source_id)` successfully. Lets the
    /// LLM recognize "this URL has been useful for plans about X, Y."
    /// Bounded by the number of plans + tags per plan; in practice
    /// 0–10 entries per `MemorySource`.
    pub associated_topics: Vec<String>,
}

impl Store {
    /// Top-N successful sources, sorted by recency descending.
    ///
    /// Implementation is one SQL statement. The join key on the
    /// `recipes ⨝ recipe_fetch_attempts` half is `recipe_id`; the
    /// `recipes ⨝ research_plans` half is `plan_id` (so we can read
    /// `topic_tags_json`). We group by `(source_url, source_id)` so
    /// repeated fetches against the same URL collapse into one
    /// `MemorySource` row.
    ///
    /// `limit` is clamped to a sane ceiling internally — pass
    /// [`SOURCES_MEMORY_LIMIT`] for the production path.
    pub fn sources_memory(&self, limit: usize) -> Result<Vec<MemorySource>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        // The query joins recipes to fetch attempts and to plans, then
        // aggregates per (source_url, source_id). The two
        // string_agg-shaped fields:
        //   - successful_attempts: COUNT of attempts where succeeded.
        //   - last_attempted_at:   MAX(attempted_at) where succeeded.
        //   - topic_tags_json:     a STRING_AGG of the plan's
        //                          topic_tags_json columns, separated
        //                          by U+001F (unit separator). We
        //                          parse-and-uniq in Rust because
        //                          DuckDB's JSON-array-flatten across
        //                          a GROUP BY is unwieldy.
        //
        // The HAVING clause restricts to (source_url, source_id) pairs
        // with at least one successful attempt — sources that have
        // only failed in the past are not surfaced. Empty memory is
        // therefore honest about what's worked.
        //
        // Capping in SQL with LIMIT keeps the IPC payload small even
        // when the user's history grows beyond hundreds of recipes.
        let clamped: i64 = (limit.min(1024)) as i64;

        let mut stmt = conn
            .prepare(
                "SELECT
                    r.source_url AS endpoint_url,
                    r.source_id  AS source_id,
                    COUNT(CASE WHEN a.succeeded THEN 1 END) AS successful_attempts,
                    MAX(CASE WHEN a.succeeded THEN a.attempted_at END) AS last_attempted_at,
                    STRING_AGG(CAST(p.topic_tags AS TEXT), CHR(31)) AS topic_tags_concat
                 FROM recipes r
                 JOIN recipe_fetch_attempts a ON a.recipe_id = r.id
                 JOIN research_plans       p ON p.id        = r.plan_id
                 GROUP BY r.source_url, r.source_id
                 HAVING COUNT(CASE WHEN a.succeeded THEN 1 END) > 0
                 ORDER BY last_attempted_at DESC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt.query(params![clamped]).map_err(StorageError::DuckDb)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            let endpoint_url: String = row.get(0).map_err(StorageError::DuckDb)?;
            let source_id: String = row.get(1).map_err(StorageError::DuckDb)?;
            let successful_attempts_raw: i64 = row.get(2).map_err(StorageError::DuckDb)?;
            let last_attempted_at: DateTime<Utc> = row.get(3).map_err(StorageError::DuckDb)?;
            let topic_tags_concat: String = row.get(4).map_err(StorageError::DuckDb)?;

            let associated_topics = parse_topic_tags(&topic_tags_concat);
            out.push(MemorySource {
                endpoint_url,
                source_id,
                successful_attempts: successful_attempts_raw.max(0) as u32,
                last_attempted_at,
                associated_topics,
            });
        }

        Ok(out)
    }
}

/// Parse the SQL `STRING_AGG` blob into a deduplicated, sorted list of
/// topic-tag strings. The blob contains one or more JSON arrays
/// separated by U+001F (unit separator). Each array is the
/// `topic_tags` field a `research_plans` row carries.
///
/// Errors during JSON parsing collapse to "no tags" for that segment
/// — a corrupt `topic_tags` column is a storage-side concern that
/// belongs in `research_plans_store`, not here. The memory view stays
/// best-effort: if half the rows have parseable tags, the result
/// surfaces the half that worked.
fn parse_topic_tags(blob: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    if blob.is_empty() {
        return Vec::new();
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for segment in blob.split('\u{001f}') {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Vec<String>>(trimmed) {
            Ok(tags) => {
                for t in tags {
                    let t = t.trim();
                    if !t.is_empty() {
                        seen.insert(t.to_string());
                    }
                }
            }
            Err(_) => {
                // The column wasn't a JSON array of strings. Skip
                // this segment rather than poisoning the whole
                // result — see the function-level doc for why.
                continue;
            }
        }
    }
    seen.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe_fetch_attempts::RecipeFetchAttemptRow;
    use crate::recipes::{AuthoredFrom, RecipeRow};
    use crate::research_plans::{PlanStatus, ResearchPlanRow};
    use chrono::TimeZone;
    use uuid::Uuid;

    fn open_in_memory() -> Store {
        let s = Store::open_in_memory().expect("open in-memory store");
        s.migrate().expect("migrate");
        s
    }

    fn ts(ymd: (i32, u32, u32), hms: (u32, u32, u32)) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(ymd.0, ymd.1, ymd.2, hms.0, hms.1, hms.2)
            .single()
            .expect("valid timestamp")
    }

    fn insert_plan(store: &Store, id: Uuid, topic_tags: &[&str], topic: &str) {
        let tags_json = serde_json::to_string(topic_tags).unwrap();
        let row = ResearchPlanRow {
            id,
            topic: topic.to_string(),
            interpretation: "test".to_string(),
            topic_tags_json: tags_json,
            geographic_scope_json: "[]".to_string(),
            historical_window_days: 365,
            expectations_json: "{}".to_string(),
            classified_by: "test".to_string(),
            created_at: ts((2026, 5, 1), (0, 0, 0)),
            status: PlanStatus::Accepted,
            rejection_reason: None,
            reclassified_from: None,
        };
        store
            .insert_research_plan(&row)
            .expect("insert research plan");
    }

    fn insert_recipe(
        store: &Store,
        id: Uuid,
        plan_id: Uuid,
        source_id: &str,
        source_url: &str,
    ) {
        let row = RecipeRow {
            id,
            dedup_key: Some(format!("{plan_id}:{source_id}")),
            plan_id,
            source_id: source_id.to_string(),
            source_url: source_url.to_string(),
            extraction_json: r#"{"mode":"json_path","path":"$.value"}"#.to_string(),
            produces_json: "[]".to_string(),
            authored_at: ts((2026, 5, 1), (0, 0, 0)),
            authored_by: "test".to_string(),
            version: 1,
            static_payload: None,
            authored_from: AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: this fixture predates iteration.
            iterator: None,
        };
        store.insert_recipe(&row).expect("insert recipe");
    }

    fn insert_attempt(store: &Store, recipe_id: Uuid, succeeded: bool, when: DateTime<Utc>) {
        let row = RecipeFetchAttemptRow {
            id: Uuid::now_v7(),
            recipe_id,
            run_id: Uuid::now_v7(),
            attempted_at: when,
            succeeded,
            failure_message: None,
            bytes_excerpt: None,
            response_content_type: None,
        };
        store
            .insert_recipe_fetch_attempt(&row)
            .expect("insert attempt");
    }

    #[test]
    fn empty_store_returns_empty_memory() {
        let store = open_in_memory();
        let mem = store.sources_memory(SOURCES_MEMORY_LIMIT).unwrap();
        assert!(mem.is_empty(), "fresh store should have no memory");
    }

    #[test]
    fn returns_only_sources_with_at_least_one_success() {
        let store = open_in_memory();
        let plan_id = Uuid::now_v7();
        let r_succ = Uuid::now_v7();
        let r_fail = Uuid::now_v7();

        insert_plan(&store, plan_id, &["lithium"], "lithium supply chain");
        insert_recipe(
            &store,
            r_succ,
            plan_id,
            "world_bank_indicators",
            "https://api.worldbank.org/v2/foo",
        );
        insert_recipe(
            &store,
            r_fail,
            plan_id,
            "comtrade",
            "https://comtrade.un.org/api/foo",
        );
        insert_attempt(&store, r_succ, true, ts((2026, 5, 1), (10, 0, 0)));
        insert_attempt(&store, r_fail, false, ts((2026, 5, 1), (11, 0, 0)));

        let mem = store.sources_memory(SOURCES_MEMORY_LIMIT).unwrap();
        assert_eq!(mem.len(), 1, "only the successful source is surfaced");
        assert_eq!(mem[0].source_id, "world_bank_indicators");
        assert_eq!(mem[0].successful_attempts, 1);
        assert_eq!(mem[0].associated_topics, vec!["lithium".to_string()]);
    }

    #[test]
    fn orders_by_last_attempted_at_descending() {
        let store = open_in_memory();
        let plan = Uuid::now_v7();
        let r_old = Uuid::now_v7();
        let r_new = Uuid::now_v7();

        insert_plan(&store, plan, &["t"], "t");
        insert_recipe(&store, r_old, plan, "old", "https://a.example.com/x");
        insert_recipe(&store, r_new, plan, "new", "https://b.example.com/x");
        insert_attempt(&store, r_old, true, ts((2026, 4, 1), (0, 0, 0)));
        insert_attempt(&store, r_new, true, ts((2026, 5, 1), (0, 0, 0)));

        let mem = store.sources_memory(SOURCES_MEMORY_LIMIT).unwrap();
        assert_eq!(mem.len(), 2);
        assert_eq!(mem[0].source_id, "new", "newest first");
        assert_eq!(mem[1].source_id, "old");
    }

    #[test]
    fn aggregates_topic_tags_across_plans() {
        let store = open_in_memory();
        let plan_a = Uuid::now_v7();
        let plan_b = Uuid::now_v7();
        let r_a = Uuid::now_v7();
        let r_b = Uuid::now_v7();

        insert_plan(&store, plan_a, &["lithium", "battery_supply_chain"], "a");
        insert_plan(&store, plan_b, &["lithium", "critical_minerals"], "b");
        // Both recipes fetched the same URL with the same source_id —
        // the memory query collapses them into one row whose
        // associated_topics is the union of the two plans' tags.
        let url = "https://api.worldbank.org/v2/foo";
        insert_recipe(&store, r_a, plan_a, "wb", url);
        insert_recipe(&store, r_b, plan_b, "wb", url);
        insert_attempt(&store, r_a, true, ts((2026, 5, 1), (0, 0, 0)));
        insert_attempt(&store, r_b, true, ts((2026, 5, 2), (0, 0, 0)));

        let mem = store.sources_memory(SOURCES_MEMORY_LIMIT).unwrap();
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].successful_attempts, 2);
        // BTreeSet ordering: alphabetical, deduplicated.
        assert_eq!(
            mem[0].associated_topics,
            vec![
                "battery_supply_chain".to_string(),
                "critical_minerals".to_string(),
                "lithium".to_string(),
            ]
        );
    }

    #[test]
    fn limit_caps_results() {
        let store = open_in_memory();
        let plan = Uuid::now_v7();
        insert_plan(&store, plan, &["t"], "t");
        for i in 0..5u32 {
            let r = Uuid::now_v7();
            insert_recipe(
                &store,
                r,
                plan,
                &format!("src{i}"),
                &format!("https://host{i}.example.com/path"),
            );
            insert_attempt(&store, r, true, ts((2026, 5, 1), (0, i, 0)));
        }
        let mem = store.sources_memory(2).unwrap();
        assert_eq!(mem.len(), 2, "limit honoured");
    }

    #[test]
    fn parse_topic_tags_dedupes_and_handles_corrupt_segments() {
        let blob = format!(
            "[\"lithium\",\"battery\"]\u{001f}corrupt-not-json\u{001f}[\"battery\",\"critical_minerals\"]"
        );
        let tags = parse_topic_tags(&blob);
        assert_eq!(
            tags,
            vec![
                "battery".to_string(),
                "critical_minerals".to_string(),
                "lithium".to_string(),
            ]
        );
    }

    #[test]
    fn parse_topic_tags_empty_input_is_empty() {
        assert!(parse_topic_tags("").is_empty());
    }
}
