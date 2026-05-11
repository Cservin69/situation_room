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

    // -----------------------------------------------------------------------
    // ADR 0019 Phase 2A — multi-leaf integration (Session 62, B)
    //
    // The recipe_apply test module pins multi-leaf at the apply()
    // boundary (record count, per-row extracted leaves, dedup_key
    // resolution). These tests extend coverage one stage further:
    // apply() composed with finalize() — the same call chain the
    // production pipeline uses. The handoff named this as ADR 0019's
    // missing integration test: a multi-leaf recipe applied
    // end-to-end through the apply-stage normalize layer with the
    // session's topic-tag envelope merge.
    //
    // No LLM, no network, no real source. Hand-authored multi-leaf
    // recipe + synthetic listing-shaped HTML fixture exercise the
    // ExtractedInner runtime + finalize composition without
    // depending on the recipe-author prompt's behaviour. These tests
    // serve as a structural smoke-test for the multi-leaf path even
    // if a future prompt regression causes the LLM to stop authoring
    // ExtractedInner recipes.
    // -----------------------------------------------------------------------

    use crate::recipe_apply::{apply, ApplyContext};
    use crate::recipes::{
        ExpectationRef, FieldMap, FieldValueSource, ProductionBinding,
    };
    use crate::research::EventTypeExpectation;
    use situation_room_core::vocab::EventType;
    use situation_room_core::RecordType;
    use serde_json::json;

    /// Plan with one event_type expectation and two topic tags. The
    /// topic tags are the normalize-stage finalization signal we
    /// assert appears on every output record's envelope.
    fn multi_leaf_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "test-multi-leaf".into(),
            interpretation: "exercise ADR 0019 multi-leaf path end-to-end".into(),
            topic_tags: vec![
                Topic::new("storms").unwrap(),
                Topic::new("weather").unwrap(),
            ],
            geographic_scope: vec![],
            historical_window_days: 30,
            expectations: RecordExpectations {
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("milestone_announced").unwrap(),
                    rationale: "fixture test".into(),
                }],
                ..Default::default()
            },
            created_at: Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap(),
        }
    }

    /// Build a multi-leaf css_select iterator recipe matching the
    /// v1.20 "position-only table" worked example. The iterator is
    /// `tr.row` (a class-bearing iterator, no per-cell classes); the
    /// inner sub-selectors are positional `td:nth-child(N)` per
    /// FieldMap. `headline` and `direction` are both ExtractedInner;
    /// `event_type` is Literal.
    fn multi_leaf_positional_recipe() -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "fixture".into(),
            source_url: Url::parse("https://example.com/listing").unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: "td:nth-child(1)".into(),
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: "tr.row".into(),
                attribute: None,
            }),
            produces: vec![ProductionBinding {
                record_type: RecordType::Event,
                expectation: ExpectationRef::EventType { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "event_type".into(),
                        source: FieldValueSource::Literal {
                            value: json!("milestone_announced"),
                        },
                    },
                    FieldMap {
                        path: "headline".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::CssSelect {
                                selector: "td:nth-child(1)".into(),
                                attribute: None,
                            },
                        },
                    },
                    FieldMap {
                        path: "direction".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::CssSelect {
                                selector: "td:nth-child(2)".into(),
                                attribute: None,
                            },
                        },
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap(),
            authored_by: "fixture-test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    #[test]
    fn adr_0019_multi_leaf_position_only_table_applies_and_finalizes_end_to_end() {
        // Three rows, no per-cell class names. Each row has a
        // headline-shaped first column and a direction-tag second
        // column. Mirrors the v1.20 worked example shape.
        let html = br#"
            <html><body><table>
              <tr class="row"><td>Hurricane Alpha</td><td>supply_negative</td></tr>
              <tr class="row"><td>Hurricane Beta</td><td>context</td></tr>
              <tr class="row"><td>Hurricane Gamma</td><td>supply_negative</td></tr>
            </table></body></html>
        "#;
        let recipe = multi_leaf_positional_recipe();
        let plan = multi_leaf_plan();

        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &plan,
            bytes: html,
            fetched_at: Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap(),
        };
        let applied = apply(ctx).expect("multi-leaf recipe must apply");
        assert_eq!(applied.len(), 3, "one record per row");

        // Compose with finalize() — the production pipeline calls
        // finalize on each record returned by apply(). The integration
        // contract: every record carries the session's topic tags AND
        // its per-row extracted leaves AND its per-row dedup_key.
        let finalized: Vec<Record> = applied
            .into_iter()
            .map(|r| finalize(r, &plan, &recipe).expect("finalize must succeed"))
            .collect();

        assert_eq!(finalized.len(), 3);

        // Collect (headline, direction, topic_tags, dedup_key) per
        // record so we can assert on each axis at once.
        let mut rows: Vec<(String, Option<String>, Vec<String>, String)> = finalized
            .iter()
            .map(|r| match r {
                Record::Event(e) => {
                    let dir = e.content.direction.map(|d| match d {
                        situation_room_core::schema::content::EventDirection::SupplyNegative => "supply_negative".to_string(),
                        situation_room_core::schema::content::EventDirection::SupplyPositive => "supply_positive".to_string(),
                        situation_room_core::schema::content::EventDirection::DemandNegative => "demand_negative".to_string(),
                        situation_room_core::schema::content::EventDirection::DemandPositive => "demand_positive".to_string(),
                        situation_room_core::schema::content::EventDirection::Context => "context".to_string(),
                    });
                    let tags: Vec<String> = e.envelope.subjects.topics.iter()
                        .map(|t| t.as_str().to_string()).collect();
                    let key = e.dedup_key.clone().expect("dedup_key must be set");
                    (e.content.headline.clone(), dir, tags, key)
                }
                other => panic!("expected Event, got {other:?}"),
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));

        // Per-row extracted leaves — the multi-leaf invariant.
        assert_eq!(rows[0].0, "Hurricane Alpha");
        assert_eq!(rows[0].1.as_deref(), Some("supply_negative"));
        assert_eq!(rows[1].0, "Hurricane Beta");
        assert_eq!(rows[1].1.as_deref(), Some("context"));
        assert_eq!(rows[2].0, "Hurricane Gamma");
        assert_eq!(rows[2].1.as_deref(), Some("supply_negative"));

        // Topic tags from the plan reach every record's envelope —
        // the finalize composition's job. This is the integration
        // axis the unit-level recipe_apply tests don't exercise.
        for (_, _, tags, _) in &rows {
            assert_eq!(tags, &vec!["storms".to_string(), "weather".to_string()],
                "every record must carry the session's topic tags after finalize");
        }

        // Per-row dedup_key derived from the headline ExtractedInner
        // FieldMap — confirms compute_dedup_key threaded the inner-
        // extractions map correctly, end-to-end.
        for (headline, _, _, key) in &rows {
            let expected = format!("{}:{}", recipe.id, headline);
            assert_eq!(key, &expected,
                "dedup_key must resolve to {{recipe_id}}:{{per-row headline}}");
        }
    }

    #[test]
    fn adr_0019_multi_leaf_preserves_record_shape_after_finalize() {
        // Lightweight smoke check: finalize on a multi-leaf-produced
        // Event yields an Event (not an Observation, not a Document).
        // Catches accidental shape regressions in finalize's
        // envelope_mut match if a future change adds a new variant
        // and miswires it.
        let html = br#"
            <html><body><table>
              <tr class="row"><td>Solo headline</td><td>context</td></tr>
            </table></body></html>
        "#;
        let recipe = multi_leaf_positional_recipe();
        let plan = multi_leaf_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &plan,
            bytes: html,
            fetched_at: Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap(),
        };
        let applied = apply(ctx).expect("apply ok");
        assert_eq!(applied.len(), 1);
        let out = finalize(applied.into_iter().next().unwrap(), &plan, &recipe).unwrap();
        assert!(matches!(out, Record::Event(_)),
            "multi-leaf Event recipe must finalize to an Event record");
    }
}

