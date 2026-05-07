//! Normalization — deterministic post-apply transforms.
//!
//! Per ADR 0007, normalization is a small, boring, deterministic
//! stage that sits between raw extraction and record storage. It
//! does **not** guess and it does **not** rewrite meaningful content
//! — when something is ambiguous or wrong, it errors rather than
//! papering over the problem. That bar is from Session 2's lesson:
//! silent degradation produced wrong-looking output that took a
//! human to notice.
//!
//! ## What this stage does today
//!
//! - Ensures the session's [`ResearchPlan::topic_tags`] appear on
//!   `envelope.subjects.topics`, de-duplicated.
//! - Stamps the recipe's id + version into the provenance chain
//!   (already done in `recipe_apply`, idempotent here).
//! - Nothing else yet. Unit normalization, date parsing, entity
//!   resolution, and range-based rejection are planned but defer
//!   until there is a second source feeding them — implementing
//!   "normalize metric tonnes → t" against one source is the wrong
//!   motivation (premature generalization). See the ADR.
//!
//! ## What this stage refuses to do
//!
//! - Coerce the extracted value. If the recipe maps a non-numeric
//!   extraction to a numeric field, that fails at
//!   [`recipe_apply::build_record`] during content deserialization,
//!   **not** here with a fallback to 0.
//! - Guess units. Unit strings flow through exactly as the recipe
//!   produced them. A unit-mapping table lives in configuration if
//!   it lives anywhere; it does not live hidden in this function.

use crate::recipes::FetchRecipe;
use crate::research::ResearchPlan;
use situation_room_core::schema::records::Record;
use situation_room_core::vocab::Topic;

/// Finalize a record after apply.
///
/// Takes ownership of the record, attaches session context, returns
/// it. Returns `Result` not because any current path errors, but so
/// callers don't need to rewrite the call site when real
/// normalization rejections land.
pub fn finalize(
    mut record: Record,
    plan: &ResearchPlan,
    _recipe: &FetchRecipe,
) -> Result<Record, crate::recipe_apply::ApplyError> {
    // Attach session topic tags to the envelope, de-duped.
    let envelope = envelope_mut(&mut record);
    merge_topics(&mut envelope.subjects.topics, &plan.topic_tags);

    Ok(record)
}

/// Access the envelope of any record variant.
fn envelope_mut(
    record: &mut Record,
) -> &mut situation_room_core::schema::envelope::Envelope {
    match record {
        Record::Observation(r) => &mut r.envelope,
        Record::Event(r) => &mut r.envelope,
        Record::Entity(r) => &mut r.envelope,
        Record::Relation(r) => &mut r.envelope,
        Record::Document(r) => &mut r.envelope,
        Record::Assertion(r) => &mut r.envelope,
    }
}

/// Merge `additional` into `existing`, preserving order and
/// de-duplicating. Linear scan is fine — topic lists are small
/// (single-digit counts in practice).
fn merge_topics(existing: &mut Vec<Topic>, additional: &[Topic]) {
    for t in additional {
        if !existing.iter().any(|e| e == t) {
            existing.push(t.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{
        ExtractionSpec, FetchRecipe,
    };
    use crate::research::RecordExpectations;
    use chrono::{TimeZone, Utc};
    use situation_room_core::schema::content::{ObservationContent, ObservationPeriod};
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::schema::records::Observation;
    use situation_room_core::vocab::{Confidence, Topic, Unit};
    use url::Url;
    use uuid::Uuid;

    fn plan_with_topics(topics: Vec<&str>) -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "test".into(),
            interpretation: "test".into(),
            topic_tags: topics
                .into_iter()
                .map(|t| Topic::new(t).unwrap())
                .collect(),
            geographic_scope: vec![],
            historical_window_days: 1,
            expectations: RecordExpectations::default(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
        }
    }

    fn obs_with_topics(topics: Vec<&str>) -> Observation {
        Observation {
            id: Uuid::now_v7(),
            dedup_key: None,
            envelope: Envelope {
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
                    topics: topics
                        .into_iter()
                        .map(|t| Topic::new(t).unwrap())
                        .collect(),
                },
                tags: vec![],
                valid_at: None,
                observed_at: Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap(),
                confidence: Confidence::ONE,
            },
            content: ObservationContent {
                metric: "production".into(),
                value: 49000.0,
                unit: Unit::new("t").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Annual,
                geometry: None,
            },
        }
    }

    fn recipe() -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "test".into(),
            source_url: Url::parse("https://example.com/x").unwrap(),
            extraction: ExtractionSpec::RegexCapture {
                pattern: "x".into(),
                group: 1,
            },
            produces: vec![],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
            authored_by: "test".into(),
            version: 1,
            static_payload: None,
            // ADR 0014: normalize tests don't exercise authoring.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
        }
    }

    #[test]
    fn finalize_adds_plan_topic_tags() {
        let rec = Record::Observation(obs_with_topics(vec![]));
        let p = plan_with_topics(vec!["Li", "batteries"]);
        let out = finalize(rec, &p, &recipe()).unwrap();
        if let Record::Observation(o) = out {
            assert_eq!(o.envelope.subjects.topics.len(), 2);
            assert_eq!(o.envelope.subjects.topics[0].as_str(), "Li");
            assert_eq!(o.envelope.subjects.topics[1].as_str(), "batteries");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn finalize_dedupes_topics_when_already_present() {
        let rec = Record::Observation(obs_with_topics(vec!["Li"]));
        let p = plan_with_topics(vec!["Li", "batteries"]);
        let out = finalize(rec, &p, &recipe()).unwrap();
        if let Record::Observation(o) = out {
            assert_eq!(o.envelope.subjects.topics.len(), 2);
            // Existing order preserved, new appended after.
            assert_eq!(o.envelope.subjects.topics[0].as_str(), "Li");
            assert_eq!(o.envelope.subjects.topics[1].as_str(), "batteries");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn finalize_preserves_record_shape() {
        let rec = Record::Observation(obs_with_topics(vec![]));
        let p = plan_with_topics(vec![]);
        let out = finalize(rec, &p, &recipe()).unwrap();
        assert!(matches!(out, Record::Observation(_)));
    }
}

