//! Fetch recipes — Level 2 output of the research function.
//!
//! See `docs/adr/0007-research-function.md`. In short: the research
//! function is a two-level LLM architecture. Level 1 produces a
//! [`ResearchPlan`](super::research::ResearchPlan) describing *what* to
//! research. Level 2 produces a set of [`FetchRecipe`] records
//! describing *where and how* to fetch the data — one recipe per
//! **source-binding pair**.
//!
//! A recipe is an *instruction*, not a fact. The runtime applies
//! recipes deterministically to produce records of the six types. The
//! LLM is involved only at recipe authoring time; it never runs at
//! refresh time.
//!
//! ## What a recipe captures
//!
//! - The URL the runtime will fetch (validated through
//!   `stockpile_secure::url_guard::UrlGuard` before storage).
//! - The extraction mode (one of five closed-enum variants — see
//!   [`ExtractionSpec`]).
//! - The mapping from extracted values to record fields (see
//!   [`ProductionBinding`] and [`FieldMap`]).
//! - Provenance of the recipe itself: who authored it (fingerprint,
//!   not key), when, and which version this is.
//!
//! ## Versioning
//!
//! Semantic changes to a recipe (different URL, different extraction
//! path, different field mapping) bump [`FetchRecipe::version`]. Old
//! versions stay in storage; records produced by v1 remain traceable
//! to v1. Cosmetic changes (struct refactors, serialization format
//! changes) are handled by migration, not versioning. See ADR 0007's
//! "Versioning vs migration" section for the full distinction.
//!
//! ## Phase 2 status
//!
//! This module currently defines the types only. The runtime that
//! *applies* a recipe — fetching, extracting, and emitting records —
//! lands in Phase 3 once source adapters are real.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use stockpile_core::RecordType;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// FetchRecipe — the top-level recipe type
// ---------------------------------------------------------------------------

/// A deterministic instruction for fetching data from a single source
/// and producing one or more records.
///
/// Identity is the UUIDv7 `id`. Natural key for idempotent re-authoring
/// is `dedup_key` (see ADR 0007 and the handoff document's commitment
/// to UUIDv7 + dedup_key identity).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchRecipe {
    /// UUIDv7 primary key. Chronologically orderable.
    pub id: Uuid,

    /// Natural key for idempotent re-authoring. Convention:
    /// `{plan_id}:{source_id}:{binding_tag}` where `binding_tag` is a
    /// deterministic slug derived from the binding's target
    /// expectation. Two Level-2 runs producing recipes for the same
    /// plan × source × binding should collide on this key and upsert
    /// (bumping `version`) rather than create duplicates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,

    /// Back-reference to the [`ResearchPlan`](super::research::ResearchPlan)
    /// this recipe was authored for.
    pub plan_id: Uuid,

    /// The registered source this recipe targets. Must resolve in the
    /// source registry; recipes cannot point at unregistered sources.
    pub source_id: String,

    /// The exact URL the runtime fetches. Must pass
    /// `stockpile_secure::url_guard::UrlGuard` at authoring time —
    /// recipes with rejected URLs never reach storage.
    pub source_url: Url,

    /// How to pull values out of the fetched content.
    pub extraction: ExtractionSpec,

    /// What records this recipe produces. A single recipe can produce
    /// multiple records per fetch (e.g. a CSV row that yields one
    /// Observation and one Event).
    pub produces: Vec<ProductionBinding>,

    /// When the recipe was authored.
    pub authored_at: DateTime<Utc>,

    /// Fingerprint of the LLM API key that authored this recipe.
    /// Non-secret; used for auditing which provider wrote what.
    /// Never stores the raw key — see
    /// `stockpile_secure::secrets::ApiKey::fingerprint`.
    pub authored_by: String,

    /// Monotonically increasing version, starting at 1. Incremented on
    /// semantic re-authoring (see module docs).
    pub version: u32,
}

// ---------------------------------------------------------------------------
// ExtractionSpec — the closed enum of supported extraction modes
// ---------------------------------------------------------------------------

/// How the runtime extracts values from fetched content.
///
/// **This is a closed enum.** Adding a new extraction mode is a
/// deliberate schema change requiring a PR and tests — not something
/// the LLM can invent at authoring time. See ADR 0007's rationale on
/// why we reject open-ended extraction DSLs.
///
/// Each variant is designed to be deterministic, cheap, and debuggable
/// when it fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ExtractionSpec {
    /// JSON structured-data extraction via a JSONPath-like expression.
    /// For API responses and machine-readable feeds.
    JsonPath {
        /// Path expression evaluated against the JSON root.
        path: String,
    },

    /// HTML extraction via a CSS selector. Optionally reads an
    /// attribute from the selected element rather than its text.
    CssSelect {
        selector: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribute: Option<String>,
    },

    /// Tabular extraction from CSV / TSV by column name.
    CsvCell {
        /// Column name (must match the header row).
        column: String,
        /// Optional row filter. If `None`, the recipe expects a
        /// single-row source and fails on multi-row content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_filter: Option<RowFilter>,
    },

    /// PDF table cell extraction. Uses a deterministic table index
    /// (not fuzzy matching). Requires the source structure to be
    /// stable across fetches — which is the case for authoritative
    /// annual reports (USGS MCS, SEC filings).
    PdfTable {
        page: u32,
        table_index: u32,
        row: u32,
        col: u32,
    },

    /// Regex capture against the raw fetched body. Last-resort mode
    /// for sources that don't fit any structured extractor. The
    /// pattern is expected to contain at least one capture group; the
    /// value comes from the specified group index.
    RegexCapture {
        pattern: String,
        group: u32,
    },
}

/// Row selection criterion for [`ExtractionSpec::CsvCell`].
///
/// Kept simple on purpose — equality and equality-with-another-column
/// cover the realistic cases without pulling in a query language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowFilter {
    /// Select the row where `column` equals `value` (string compare).
    Equals {
        column: String,
        value: String,
    },
    /// Select the row where the first-column match equals the given
    /// value, i.e. "the row labeled X". Convention: `label_column` is
    /// often the first column of a pivoted table.
    LabeledAs {
        label_column: String,
        label: String,
    },
}

// ---------------------------------------------------------------------------
// ProductionBinding — what the recipe emits
// ---------------------------------------------------------------------------

/// What one recipe emits per successful apply.
///
/// A recipe produces one or more bindings. Each binding targets one
/// [`RecordType`] and says which fields of that record's content come
/// from which extracted paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductionBinding {
    /// Which of the six record types this binding produces.
    ///
    /// Only four are valid targets in practice — `Document` is
    /// produced directly by ingestion (it *is* the fetch), and
    /// `Entity` is produced by registry lookup rather than by recipe.
    /// The other four (`Observation`, `Event`, `Relation`, `Assertion`)
    /// are the realistic targets. Validation of which types are
    /// actually permitted happens in the apply stage, not here.
    pub record_type: RecordType,

    /// Which expectation from the [`ResearchPlan`] this binding
    /// fulfills. The reference is by position (index into the plan's
    /// expectation lists) plus a discriminator for which list.
    pub expectation: ExpectationRef,

    /// One entry per field of the target record's content type,
    /// mapping an extracted path to the field.
    pub field_mappings: Vec<FieldMap>,
}

/// Reference into a `ResearchPlan`'s `RecordExpectations`.
///
/// Position-based because `RecordExpectations` is a struct of `Vec`s
/// and indexing is stable per-session. A recipe points at "the Nth
/// `MetricExpectation` in the plan" rather than repeating the
/// metric definition — the plan is the source of truth; the recipe
/// is a fulfillment of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "list", rename_all = "snake_case")]
pub enum ExpectationRef {
    /// `ResearchPlan::expectations::observation_metrics[index]`
    ObservationMetric { index: u32 },
    /// `ResearchPlan::expectations::event_types[index]`
    EventType { index: u32 },
    /// `ResearchPlan::expectations::entity_kinds[index]`
    EntityKind { index: u32 },
    /// `ResearchPlan::expectations::relation_kinds[index]`
    RelationKind { index: u32 },
    /// `ResearchPlan::expectations::document_sources[index]`
    DocumentSource { index: u32 },
}

/// Maps one extracted value to one field of the target record.
///
/// The `path` is a dot-separated path into the target content type
/// (e.g. `"metric"`, `"value"`, `"unit"`, `"period"` for an
/// `ObservationContent`). The `source` describes where the value
/// comes from.
///
/// When a recipe's extraction mode returns a single scalar, every
/// `FieldMap` in its bindings will typically have
/// [`FieldValueSource::Extracted`] — the extracted value is the
/// source for that field. When the recipe needs to populate fields
/// from elsewhere (the session's plan, a literal constant, a derived
/// computation), the other `FieldValueSource` variants apply.
///
/// The deliberate design: keep this enum small. If a recipe needs
/// arbitrary computation, that's a signal to add a new extraction
/// mode or reshape the source, not to grow the field-value enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldMap {
    /// Dot-separated path into the target record's content type.
    /// Matched against the content type's serde field names.
    pub path: String,

    /// Where the value comes from.
    pub source: FieldValueSource,
}

/// Where a field's value comes from, when applying a recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldValueSource {
    /// The value from the recipe's extraction step. For a recipe
    /// that extracts a single scalar, this is the common case.
    Extracted,

    /// A literal constant baked into the recipe. For fields the
    /// source doesn't carry but the recipe author knows — e.g. the
    /// unit for a source that's always in tonnes but doesn't label it.
    Literal { value: serde_json::Value },

    /// A value taken from the session's [`ResearchPlan`]. Common for
    /// `Observation::metric` (the canonical metric name lives in the
    /// plan's expectations).
    FromPlan { pointer: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_recipe() -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some("plan_abc:usgs_mcs:obs_production".into()),
            plan_id: Uuid::now_v7(),
            source_id: "usgs_mcs".into(),
            source_url: Url::parse("https://pubs.usgs.gov/periodicals/mcs2025/mcs2025-lithium.pdf")
                .unwrap(),
            extraction: ExtractionSpec::PdfTable {
                page: 2,
                table_index: 0,
                row: 3,
                col: 1,
            },
            produces: vec![ProductionBinding {
                record_type: RecordType::Observation,
                expectation: ExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "value".into(),
                        source: FieldValueSource::Extracted,
                    },
                    FieldMap {
                        path: "unit".into(),
                        source: FieldValueSource::Literal {
                            value: serde_json::json!("t"),
                        },
                    },
                    FieldMap {
                        path: "metric".into(),
                        source: FieldValueSource::FromPlan {
                            pointer: "expectations.observation_metrics.0.name".into(),
                        },
                    },
                ],
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            authored_by: "sk-a...z9qp".into(), // fingerprint format per ApiKey::fingerprint
            version: 1,
        }
    }

    #[test]
    fn fetch_recipe_roundtrips_through_json() {
        let r = sample_recipe();
        let json = serde_json::to_string(&r).unwrap();
        let back: FetchRecipe = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn recipe_id_is_uuidv7() {
        // Per ADR 0003: every record-shaped persistent thing has a
        // UUIDv7 id. Recipes are not records but follow the same
        // identity convention.
        let r = sample_recipe();
        assert_eq!(r.id.get_version_num(), 7);
    }

    #[test]
    fn extraction_spec_serializes_with_mode_tag() {
        // The `mode` tag is the discriminator. The frontend and the
        // Level-2 prompt both rely on it.
        let spec = ExtractionSpec::JsonPath {
            path: "$.data.spot.usd_per_tonne".into(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"mode\":\"json_path\""));
        let back: ExtractionSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn extraction_spec_all_variants_roundtrip() {
        let specs = [
            ExtractionSpec::JsonPath { path: "$.a.b".into() },
            ExtractionSpec::CssSelect {
                selector: "table.prices tr:nth-child(2) td:nth-child(3)".into(),
                attribute: None,
            },
            ExtractionSpec::CssSelect {
                selector: "a.download".into(),
                attribute: Some("href".into()),
            },
            ExtractionSpec::CsvCell {
                column: "production_kt".into(),
                row_filter: Some(RowFilter::Equals {
                    column: "country".into(),
                    value: "Chile".into(),
                }),
            },
            ExtractionSpec::CsvCell {
                column: "value".into(),
                row_filter: None,
            },
            ExtractionSpec::PdfTable {
                page: 1,
                table_index: 0,
                row: 2,
                col: 3,
            },
            ExtractionSpec::RegexCapture {
                pattern: r"(\d+(?:\.\d+)?)\s*tonnes".into(),
                group: 1,
            },
        ];
        for s in specs {
            let json = serde_json::to_string(&s).unwrap();
            let back: ExtractionSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn expectation_ref_discriminates_by_list() {
        // Each variant should serialize with its own `list` tag so the
        // apply stage can route without a pattern-match tree.
        let refs = [
            ExpectationRef::ObservationMetric { index: 0 },
            ExpectationRef::EventType { index: 1 },
            ExpectationRef::EntityKind { index: 2 },
            ExpectationRef::RelationKind { index: 3 },
            ExpectationRef::DocumentSource { index: 4 },
        ];
        for r in refs {
            let json = serde_json::to_string(&r).unwrap();
            assert!(json.contains("\"list\":"));
            let back: ExpectationRef = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
        // Spot-check one name explicitly to guard against accidental
        // rename_all changes.
        let s = serde_json::to_string(&ExpectationRef::ObservationMetric { index: 0 }).unwrap();
        assert!(s.contains("\"observation_metric\""));
    }

    #[test]
    fn field_value_source_variants_roundtrip() {
        let vs = [
            FieldValueSource::Extracted,
            FieldValueSource::Literal {
                value: serde_json::json!(42),
            },
            FieldValueSource::Literal {
                value: serde_json::json!("USD"),
            },
            FieldValueSource::FromPlan {
                pointer: "topic_tags.0".into(),
            },
        ];
        for v in vs {
            let json = serde_json::to_string(&v).unwrap();
            let back: FieldValueSource = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn dedup_key_is_optional_and_omits_when_absent() {
        let mut r = sample_recipe();
        r.dedup_key = None;
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("dedup_key"));
    }

    #[test]
    fn row_filter_variants_roundtrip() {
        let filters = [
            RowFilter::Equals {
                column: "country".into(),
                value: "Chile".into(),
            },
            RowFilter::LabeledAs {
                label_column: "row_label".into(),
                label: "Total production".into(),
            },
        ];
        for f in filters {
            let json = serde_json::to_string(&f).unwrap();
            let back: RowFilter = serde_json::from_str(&json).unwrap();
            assert_eq!(f, back);
        }
    }

    #[test]
    fn record_type_roundtrips_as_snake_case() {
        // Verifies the addition to core: RecordType serializes the same
        // way Record's `type` tag does. A recipe's `produces[i].record_type`
        // and a resulting Record's type discriminator should match
        // byte-for-byte so the apply stage can emit records that
        // deserialize back through the Record enum.
        let all = [
            RecordType::Observation,
            RecordType::Event,
            RecordType::Entity,
            RecordType::Relation,
            RecordType::Document,
            RecordType::Assertion,
        ];
        for rt in all {
            let json = serde_json::to_string(&rt).unwrap();
            let back: RecordType = serde_json::from_str(&json).unwrap();
            assert_eq!(rt, back);
        }
        assert_eq!(
            serde_json::to_string(&RecordType::Observation).unwrap(),
            "\"observation\""
        );
    }
}
