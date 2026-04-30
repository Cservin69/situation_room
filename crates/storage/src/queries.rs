//! Queries that span record types.
//!
//! So far: `topics_in_use`, which supports the Level-1 classifier
//! injection documented in ADR 0007. The classifier is shown the
//! topic strings already in use across past sessions, so related
//! queries converge on shared topic strings.

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
}
