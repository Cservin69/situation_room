//! Queries that span record types.
//!
//! Two cross-cutting queries live here:
//!
//! - `topics_in_use` — supports Level-1 classifier injection (ADR 0007).
//! - `record_counts_for_plan` — supports the plan ↔ record satisfaction
//!   view (Session 14 P2). Given a plan's topic tags, returns how many
//!   records of each type were found bearing those tags.
//!
//! Both queries read from the `record_subjects_topics` junction table,
//! which is the authoritative link between records and the topics a plan
//! cares about.

use duckdb::params;
use situation_room_core::vocab::Topic;

use crate::connection::Store;
use crate::{Result, StorageError};

/// One row from `topics_in_use`: a topic string and its usage count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicUsage {
    pub topic: Topic,
    pub count: u64,
}

impl Store {
    /// Return the top-N topic strings in use, sorted by usage count
    /// descending. Counts are the number of record-topic junction
    /// rows — a record tagged with two topics contributes one to
    /// each.
    ///
    /// Passed to the Level-1 classifier as hygiene context: "here are
    /// the topics this system has used before; prefer them when a
    /// new query is plausibly about the same subject." See ADR 0007.
    pub fn topics_in_use(&self, limit: usize) -> Result<Vec<TopicUsage>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT topic, COUNT(*) AS n
                 FROM record_subjects_topics
                 GROUP BY topic
                 ORDER BY n DESC, topic ASC
                 LIMIT ?",
            )
            .map_err(StorageError::DuckDb)?;

        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(StorageError::DuckDb)?;

        let mut out = Vec::new();
        for r in rows {
            let (topic_s, count) = r.map_err(StorageError::DuckDb)?;
            let topic = Topic::new(topic_s)
                .map_err(|e| StorageError::Other(format!("topic round-trip: {e}")))?;
            out.push(TopicUsage {
                topic,
                count: count as u64,
            });
        }
        Ok(out)
    }
}

/// Record counts broken down by type, for all records that share at
/// least one topic tag with a given plan. This is the data source for
/// the plan ↔ record satisfaction view (Session 14, P2).
///
/// A count of 0 for a type means no records of that type have landed
/// yet; the plan expected them but fetching hasn't produced them. This
/// is the Class-D blind-spot surface: the user can now see "I expected
/// observations but got zero" rather than having to query DuckDB
/// directly.
///
/// The join is deliberately broad: **any** record tagged with **any**
/// of the plan's topic tags is counted. This is the correct semantics
/// because the plan's topic_tags are what the normalize stage stamps
/// onto every record produced for the plan (see ADR 0007
/// §"Normalization stage" and `pipeline::normalize::finalize`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordCountsForPlan {
    pub observations: u64,
    pub events: u64,
    pub entities: u64,
    pub relations: u64,
    pub documents: u64,
    pub assertions: u64,
}

impl RecordCountsForPlan {
    /// Total records across all types. Useful for the "did anything
    /// land at all?" check in the satisfaction banner.
    pub fn total(&self) -> u64 {
        self.observations
            + self.events
            + self.entities
            + self.relations
            + self.documents
            + self.assertions
    }
}

impl Store {
    /// Return record counts (by type) for all records that share at
    /// least one topic tag with `topic_tags`.
    ///
    /// An empty `topic_tags` slice returns all-zeros immediately
    /// without a database round-trip — a plan with no topic tags
    /// cannot have produced any tagged records.
    ///
    /// ## Query strategy
    ///
    /// We query `record_subjects_topics` for each record type
    /// separately using `COUNT(DISTINCT record_id)`. `DISTINCT` is
    /// load-bearing: a record tagged with two of the plan's topics
    /// would be double-counted without it.
    ///
    /// DuckDB's `IN (?)` with a repeated-param approach is used
    /// rather than a JSON array because the driver handles the
    /// parameter repetition cleanly and avoids a parse-then-JOIN.
    pub fn record_counts_for_plan(
        &self,
        topic_tags: &[String],
    ) -> Result<RecordCountsForPlan> {
        if topic_tags.is_empty() {
            return Ok(RecordCountsForPlan::default());
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let placeholders = topic_tags
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");

        // Build the IN-list query once and re-run per record type.
        // `params_from_iter` handles dynamic param counts cleanly;
        // `params![]` requires a compile-time-known count.
        let sql = format!(
            "SELECT COUNT(DISTINCT record_id)
             FROM record_subjects_topics
             WHERE record_type = ?
               AND topic IN ({placeholders})"
        );

        let count_for_type = |record_type: &str| -> Result<u64> {
            // Params: record_type first, then the topic strings.
            let params_iter = std::iter::once(record_type.to_string())
                .chain(topic_tags.iter().cloned());
            let count: i64 = conn
                .query_row(&sql, duckdb::params_from_iter(params_iter), |r| r.get(0))
                .map_err(StorageError::DuckDb)?;
            Ok(count as u64)
        };

        Ok(RecordCountsForPlan {
            observations: count_for_type("observation")?,
            events:       count_for_type("event")?,
            entities:     count_for_type("entity")?,
            relations:    count_for_type("relation")?,
            documents:    count_for_type("document")?,
            assertions:   count_for_type("assertion")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::connection::Store;
    use chrono::Utc;
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, Topic, Unit};
    use situation_room_core::Observation;

    fn obs_with_topics(topics: Vec<Topic>) -> Observation {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "test".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics,
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Observation::new(
            envelope,
            ObservationContent {
                metric: "m".into(),
                value: 1.0,
                unit: Unit::new("1").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Instant,
                geometry: None,
            },
        )
    }

    #[test]
    fn topics_in_use_empty_store_returns_empty() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let topics = store.topics_in_use(100).unwrap();
        assert!(topics.is_empty());
    }

    #[test]
    fn topics_in_use_ranks_by_frequency() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let li = Topic::new("Li").unwrap();
        let semi = Topic::new("semiconductors").unwrap();
        let cu = Topic::new("Cu").unwrap();

        // Li appears on 3 records, semiconductors on 2, Cu on 1.
        for _ in 0..3 {
            store
                .insert_observation(&obs_with_topics(vec![li.clone()]))
                .unwrap();
        }
        for _ in 0..2 {
            store
                .insert_observation(&obs_with_topics(vec![semi.clone()]))
                .unwrap();
        }
        store
            .insert_observation(&obs_with_topics(vec![cu.clone()]))
            .unwrap();

        let topics = store.topics_in_use(10).unwrap();
        assert_eq!(topics.len(), 3);
        assert_eq!(topics[0].topic, li);
        assert_eq!(topics[0].count, 3);
        assert_eq!(topics[1].topic, semi);
        assert_eq!(topics[1].count, 2);
        assert_eq!(topics[2].topic, cu);
        assert_eq!(topics[2].count, 1);
    }

    #[test]
    fn topics_in_use_respects_limit() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        for t in ["Li", "Cu", "Ni"] {
            store
                .insert_observation(&obs_with_topics(vec![Topic::new(t).unwrap()]))
                .unwrap();
        }

        let topics = store.topics_in_use(2).unwrap();
        assert_eq!(topics.len(), 2);
    }

    #[test]
    fn topics_in_use_counts_multiple_topics_per_record() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // One record with two topics — both should get count 1.
        let li = Topic::new("Li").unwrap();
        let semi = Topic::new("semiconductors").unwrap();
        store
            .insert_observation(&obs_with_topics(vec![li.clone(), semi.clone()]))
            .unwrap();

        let topics = store.topics_in_use(10).unwrap();
        assert_eq!(topics.len(), 2);
        assert_eq!(topics[0].count, 1);
        assert_eq!(topics[1].count, 1);
    }

    // -----------------------------------------------------------------------
    // record_counts_for_plan tests
    // -----------------------------------------------------------------------

    #[test]
    fn record_counts_for_plan_empty_tags_returns_zeros() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // Even with records in the store, empty tag list → all zeros.
        store
            .insert_observation(&obs_with_topics(vec![Topic::new("Li").unwrap()]))
            .unwrap();

        let counts = store.record_counts_for_plan(&[]).unwrap();
        assert_eq!(counts, super::RecordCountsForPlan::default());
        assert_eq!(counts.total(), 0);
    }

    #[test]
    fn record_counts_for_plan_empty_store_returns_zeros() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let counts = store
            .record_counts_for_plan(&["Li".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 0);
        assert_eq!(counts.total(), 0);
    }

    #[test]
    fn record_counts_for_plan_counts_matching_observations() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let li = Topic::new("Li").unwrap();
        let cu = Topic::new("Cu").unwrap();

        // 2 Li records, 1 Cu record.
        store
            .insert_observation(&obs_with_topics(vec![li.clone()]))
            .unwrap();
        store
            .insert_observation(&obs_with_topics(vec![li.clone()]))
            .unwrap();
        store
            .insert_observation(&obs_with_topics(vec![cu.clone()]))
            .unwrap();

        // Plan with Li tag sees 2 observations.
        let counts = store
            .record_counts_for_plan(&["Li".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 2);
        assert_eq!(counts.events, 0);

        // Plan with Cu tag sees 1 observation.
        let counts = store
            .record_counts_for_plan(&["Cu".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 1);

        // Plan with both tags sees 3 observations.
        let counts = store
            .record_counts_for_plan(&["Li".to_string(), "Cu".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 3);
    }

    #[test]
    fn record_counts_for_plan_does_not_double_count_multi_tagged_record() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let li = Topic::new("Li").unwrap();
        let batteries = Topic::new("batteries").unwrap();

        // One record tagged with BOTH topics.
        store
            .insert_observation(&obs_with_topics(vec![li.clone(), batteries.clone()]))
            .unwrap();

        // The plan has both topic tags — COUNT(DISTINCT) must return 1,
        // not 2.
        let counts = store
            .record_counts_for_plan(&["Li".to_string(), "batteries".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 1, "double-count guard failed");
        assert_eq!(counts.total(), 1);
    }

    #[test]
    fn record_counts_for_plan_unrelated_topics_return_zero() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        // Record tagged with "Li"; plan asks for "Cu" — should see 0.
        store
            .insert_observation(&obs_with_topics(vec![Topic::new("Li").unwrap()]))
            .unwrap();

        let counts = store
            .record_counts_for_plan(&["Cu".to_string()])
            .unwrap();
        assert_eq!(counts.observations, 0);
        assert_eq!(counts.total(), 0);
    }
}
