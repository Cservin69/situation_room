//! Recipe apply runtime — Level 2 execution (ADR 0007).
//!
//! Given a [`FetchRecipe`] authored by `recipe_author`, the bytes of
//! the source, and the session's [`ResearchPlan`], produce
//! `Vec<Record>` deterministically. **No LLM, no non-determinism, no
//! wall-clock-dependent behaviour beyond the `fetched_at` the caller
//! supplies.** Same inputs → same outputs. That property is what makes
//! refreshes cheap, auditable, and offline-capable.
//!
//! ## Scope
//!
//! This module does **not** fetch. The caller fetches through
//! [`stockpile_secure::SecureHttpClient`] and hands us the bytes.
//! Keeping I/O out of here makes the module trivially testable with
//! inline fixtures and keeps all network-facing defences (SSRF,
//! bounded size, TLS) in one place.
//!
//! Four of the five extraction modes are implemented. `PdfTable`
//! returns a structured `NotImplemented` error — see the arm for the
//! full rationale. The demo binary exercises the end-to-end path
//! using a non-PDF source; USGS / PDF sources unblock when positional
//! PDF table extraction lands as its own session.
//!
//! ## Flow
//!
//! ```text
//!   recipe + bytes + plan
//!        │
//!        ▼
//!   extract(extraction_spec, bytes) → String
//!        │
//!        ▼
//!   for each binding:
//!     build content JSON from field_mappings
//!     deserialize to concrete content type
//!     wrap in record with envelope (provenance + topic_tags)
//!        │
//!        ▼
//!   normalize::finalize(record, plan, recipe)
//!        │
//!        ▼
//!   Vec<Record>
//! ```
//!
//! Errors identify the stage that failed so production logs point at
//! the right piece of the recipe. No stage silently degrades.

use chrono::{DateTime, Utc};
use csv::ReaderBuilder;
use jsonpath_rust::JsonPath;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::{json, Map, Value};
use thiserror::Error;
use uuid::Uuid;

use stockpile_core::schema::content::{
    EventContent, ObservationContent, RelationContent,
};
use stockpile_core::schema::envelope::{Envelope, Provenance, Subjects};
use stockpile_core::schema::records::{Event, Observation, Record, Relation};
use stockpile_core::vocab::Confidence;
use stockpile_core::RecordType;

use crate::recipes::{
    ExpectationRef, ExtractionSpec, FetchRecipe, FieldMap, FieldValueSource,
    ProductionBinding, RowFilter,
};
use crate::research::ResearchPlan;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Inputs to one apply call.
#[derive(Debug)]
pub struct ApplyContext<'a> {
    pub recipe: &'a FetchRecipe,
    pub plan: &'a ResearchPlan,
    pub bytes: &'a [u8],
    /// When the caller fetched the bytes. Stamped onto the resulting
    /// envelope as `observed_at`. Explicit (not `Utc::now()`) so tests
    /// are deterministic and the pipeline stage above controls time.
    pub fetched_at: DateTime<Utc>,
}

/// Errors that can arise during apply.
///
/// Each variant names the stage so logs surface actionable info
/// without the reader cross-referencing the recipe.
#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("extraction [{mode}]: {reason}")]
    Extraction { mode: &'static str, reason: String },

    #[error("extraction mode not implemented: {mode} ({reason})")]
    NotImplemented { mode: &'static str, reason: String },

    #[error("binding[{index}]: {reason}")]
    Binding { index: usize, reason: String },

    #[error("field mapping: {reason}")]
    FieldMapping { reason: String },

    #[error("content assembly failed: {reason}")]
    ContentAssembly { reason: String },

    #[error("normalization rejected record: {reason}")]
    Normalization { reason: String },
}

/// Apply a recipe to fetched bytes, producing zero or more records.
///
/// Returns `Vec<Record>` rather than `Record` because a single recipe
/// may produce multiple records per fetch (each binding → one record).
pub fn apply(ctx: ApplyContext<'_>) -> Result<Vec<Record>, ApplyError> {
    // 1. Extract a single scalar value from the bytes. Every mode
    //    currently returns one value; multi-value extraction would
    //    be a mode-shape change, not just a different path.
    let extracted = extract(&ctx.recipe.extraction, ctx.bytes)?;

    // 2. For each binding, build one record.
    let mut records = Vec::with_capacity(ctx.recipe.produces.len());
    for (idx, binding) in ctx.recipe.produces.iter().enumerate() {
        let record = build_record(binding, idx, &extracted, &ctx)
            .and_then(|r| crate::normalize::finalize(r, ctx.plan, ctx.recipe))?;
        records.push(record);
    }

    Ok(records)
}

// ---------------------------------------------------------------------------
// Extraction: one function per ExtractionSpec variant
// ---------------------------------------------------------------------------

/// Dispatch to the per-mode extractor.
///
/// Each extractor is a pure function from bytes → string. No stage
/// above the extractor mutates or re-interprets the extracted string
/// — it flows directly into the binding stage.
fn extract(spec: &ExtractionSpec, bytes: &[u8]) -> Result<String, ApplyError> {
    match spec {
        ExtractionSpec::JsonPath { path } => extract_json_path(bytes, path),
        ExtractionSpec::CssSelect {
            selector,
            attribute,
        } => extract_css_select(bytes, selector, attribute.as_deref()),
        ExtractionSpec::CsvCell { column, row_filter } => {
            extract_csv_cell(bytes, column, row_filter.as_ref())
        }
        ExtractionSpec::PdfTable { .. } => Err(ApplyError::NotImplemented {
            mode: "pdf_table",
            reason: "positional PDF table extraction is not yet implemented; \
                     pure-rust positional cell access is a known hard problem \
                     that will land as its own focused session. Recipes for PDF \
                     sources are authored and stored correctly but currently \
                     fail at apply time."
                .into(),
        }),
        ExtractionSpec::RegexCapture { pattern, group } => {
            extract_regex(bytes, pattern, *group)
        }
    }
}

fn extract_json_path(bytes: &[u8], path: &str) -> Result<String, ApplyError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| ApplyError::Extraction {
        mode: "json_path",
        reason: format!("bytes did not parse as JSON: {e}"),
    })?;

    // jsonpath-rust 1.x exposes `.query(path) -> Result<Vec<&Value>, _>`
    // on `serde_json::Value` via the `JsonPath` trait.
    let nodes: Vec<&Value> = value.query(path).map_err(|e| ApplyError::Extraction {
        mode: "json_path",
        reason: format!("path query failed: {e}"),
    })?;

    let first = nodes.into_iter().next().ok_or_else(|| ApplyError::Extraction {
        mode: "json_path",
        reason: format!("path {path:?} matched no nodes"),
    })?;

    // Preserve the value's natural JSON representation. Strings come
    // out unquoted; numbers, bools, objects keep their JSON form.
    Ok(match first {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

fn extract_css_select(
    bytes: &[u8],
    selector: &str,
    attribute: Option<&str>,
) -> Result<String, ApplyError> {
    let html_str = std::str::from_utf8(bytes).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("bytes were not UTF-8: {e}"),
    })?;
    let doc = Html::parse_document(html_str);
    let sel = Selector::parse(selector).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("selector did not parse: {e}"),
    })?;

    let first = doc
        .select(&sel)
        .next()
        .ok_or_else(|| ApplyError::Extraction {
            mode: "css_select",
            reason: format!("selector {selector:?} matched no elements"),
        })?;

    let out = match attribute {
        Some(attr_name) => first
            .value()
            .attr(attr_name)
            .ok_or_else(|| ApplyError::Extraction {
                mode: "css_select",
                reason: format!("element has no attribute {attr_name:?}"),
            })?
            .to_string(),
        None => first
            .text()
            .collect::<String>()
            .trim()
            .to_string(),
    };

    if out.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: "selection resolved to empty string".into(),
        });
    }
    Ok(out)
}

fn extract_csv_cell(
    bytes: &[u8],
    column: &str,
    row_filter: Option<&RowFilter>,
) -> Result<String, ApplyError> {
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);

    let headers = reader
        .headers()
        .map_err(|e| ApplyError::Extraction {
            mode: "csv_cell",
            reason: format!("could not read headers: {e}"),
        })?
        .clone();

    let col_idx = headers
        .iter()
        .position(|h| h == column)
        .ok_or_else(|| ApplyError::Extraction {
            mode: "csv_cell",
            reason: format!("column {column:?} not in headers {:?}", headers.iter().collect::<Vec<_>>()),
        })?;

    // Resolve the filter column index up front too (if any).
    let filter_col_idx = match row_filter {
        Some(RowFilter::Equals { column, .. })
        | Some(RowFilter::LabeledAs {
            label_column: column,
            ..
        }) => Some(
            headers
                .iter()
                .position(|h| h == column)
                .ok_or_else(|| ApplyError::Extraction {
                    mode: "csv_cell",
                    reason: format!("filter column {column:?} not in headers"),
                })?,
        ),
        None => None,
    };

    let expected_value = match row_filter {
        Some(RowFilter::Equals { value, .. }) => Some(value.as_str()),
        Some(RowFilter::LabeledAs { label, .. }) => Some(label.as_str()),
        None => None,
    };

    let mut matching: Vec<String> = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|e| ApplyError::Extraction {
            mode: "csv_cell",
            reason: format!("row parse error: {e}"),
        })?;
        let matches = match (filter_col_idx, expected_value) {
            (Some(i), Some(v)) => rec.get(i).map(|c| c == v).unwrap_or(false),
            _ => true, // no filter = every row matches
        };
        if matches {
            let cell = rec
                .get(col_idx)
                .ok_or_else(|| ApplyError::Extraction {
                    mode: "csv_cell",
                    reason: format!("row has no cell at index {col_idx}"),
                })?
                .to_string();
            matching.push(cell);
        }
    }

    match matching.len() {
        0 => Err(ApplyError::Extraction {
            mode: "csv_cell",
            reason: "no rows matched the filter".into(),
        }),
        1 => Ok(matching.into_iter().next().unwrap()),
        n => Err(ApplyError::Extraction {
            mode: "csv_cell",
            reason: format!(
                "ambiguous: {n} rows matched the filter (a recipe must \
                 pick exactly one row; tighten the filter or change the recipe \
                 to extract multi-value, which is a separate extraction mode)"
            ),
        }),
    }
}

fn extract_regex(bytes: &[u8], pattern: &str, group: u32) -> Result<String, ApplyError> {
    let text = std::str::from_utf8(bytes).map_err(|e| ApplyError::Extraction {
        mode: "regex_capture",
        reason: format!("bytes were not UTF-8: {e}"),
    })?;
    let re = Regex::new(pattern).map_err(|e| ApplyError::Extraction {
        mode: "regex_capture",
        reason: format!("pattern did not compile: {e}"),
    })?;
    let caps = re.captures(text).ok_or_else(|| ApplyError::Extraction {
        mode: "regex_capture",
        reason: "pattern matched nothing".into(),
    })?;
    let m = caps
        .get(group as usize)
        .ok_or_else(|| ApplyError::Extraction {
            mode: "regex_capture",
            reason: format!("capture group {group} not present in match"),
        })?;
    Ok(m.as_str().to_string())
}

// ---------------------------------------------------------------------------
// Binding stage: turn an extracted value + mappings into a record.
// ---------------------------------------------------------------------------

fn build_record(
    binding: &ProductionBinding,
    index: usize,
    extracted: &str,
    ctx: &ApplyContext<'_>,
) -> Result<Record, ApplyError> {
    // Build a JSON object from the field mappings, then deserialize
    // into the concrete content type. This keeps the record types
    // authoritative about their own shape — we don't hand-write
    // ObservationContent assembly.
    let mut content_json: Map<String, Value> = Map::new();
    for fm in &binding.field_mappings {
        let value = resolve_field_value(fm, extracted, ctx.plan).map_err(|e| {
            ApplyError::Binding {
                index,
                reason: e.to_string(),
            }
        })?;
        insert_at_path(&mut content_json, &fm.path, value).map_err(|e| {
            ApplyError::Binding {
                index,
                reason: e.to_string(),
            }
        })?;
    }

    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            ctx.recipe.source_id, ctx.recipe.id, ctx.recipe.version
        ),
        source_url: Some(ctx.recipe.source_url.to_string()),
        source_published_at: None,
        license: "unknown".into(), // the caller (ingest) can override
                                   // with the registry's license string.
        derived_from: vec![],
    };

    let subjects = Subjects {
        entities: vec![],
        places: vec![],
        time: None,
        topics: ctx.plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: None,
        observed_at: ctx.fetched_at,
        confidence: Confidence::ONE,
    };

    let content_value = Value::Object(content_json);

    let record = match binding.record_type {
        RecordType::Observation => {
            let content: ObservationContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: format!("observation content: {e}"),
                })?;
            Record::Observation(Observation {
                id: Uuid::now_v7(),
                dedup_key: None,
                envelope,
                content,
            })
        }
        RecordType::Event => {
            let content: EventContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: format!("event content: {e}"),
                })?;
            Record::Event(Event {
                id: Uuid::now_v7(),
                dedup_key: None,
                envelope,
                content,
            })
        }
        RecordType::Relation => {
            let content: RelationContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: format!("relation content: {e}"),
                })?;
            Record::Relation(Relation {
                id: Uuid::now_v7(),
                dedup_key: None,
                envelope,
                content,
            })
        }
        RecordType::Document | RecordType::Entity | RecordType::Assertion => {
            return Err(ApplyError::Binding {
                index,
                reason: format!(
                    "record_type {:?} is not producible from a recipe. \
                     Documents come from ingest; entities come from registry lookup. \
                     Assertions come from the LLM extraction layer (they carry a \
                     claimant and stance that recipe field-mappings don't populate). \
                     See ADR 0007 and ADR 0004.",
                    binding.record_type
                ),
            });
        }
    };

    let _ = binding.expectation; // ExpectationRef is carried forward
                                 // for observability / debugging; it
                                 // informs the provenance chain but
                                 // doesn't change the record's shape.
                                 // See ADR 0007 on how the UI shows
                                 // which expectation a cell fulfills.

    Ok(record)
}

/// Resolve one field mapping to a `serde_json::Value`.
fn resolve_field_value(
    fm: &FieldMap,
    extracted: &str,
    plan: &ResearchPlan,
) -> Result<Value, ApplyError> {
    match &fm.source {
        FieldValueSource::Extracted => {
            // The extracted scalar is a String. Without knowing the
            // target field's type we can only guess — and guessing
            // is exactly what we refuse to do. Strategy: try to parse
            // as number first (since numeric observation values are
            // the common case); if that fails, keep as string.
            // The content type's deserialization will reject a
            // type-mismatched value and the error surfaces to the
            // caller. That is the desired behaviour: a recipe that
            // maps a non-numeric extraction into `value: f64` fails
            // loudly, not silently.
            Ok(parse_extracted_scalar(extracted))
        }
        FieldValueSource::Literal { value } => Ok(value.clone()),
        FieldValueSource::FromPlan { pointer } => {
            // `pointer` is a dot-separated path into the plan's JSON
            // representation. We serialize the plan to JSON once
            // per mapping — small plans, cheap — and use a manual
            // walker so we control index semantics (numeric segments
            // are Vec indices, string segments are object keys).
            let plan_json = serde_json::to_value(plan).map_err(|e| {
                ApplyError::FieldMapping {
                    reason: format!("plan → json: {e}"),
                }
            })?;
            walk_pointer(&plan_json, pointer).ok_or_else(|| ApplyError::FieldMapping {
                reason: format!("from_plan pointer {pointer:?} resolved to nothing"),
            })
        }
    }
}

/// Parse an extracted string as a JSON scalar, preferring numbers.
///
/// Strategy: strip commas and surrounding whitespace, trim a trailing
/// unit-like suffix (everything after the last digit). If what
/// remains parses as a number, return a `Value::Number`. Otherwise
/// return the original trimmed string as `Value::String`.
///
/// Known limitation: assumes comma-as-thousands, period-as-decimal
/// (US/UK convention). European decimals (`"88.000,0"`) will parse
/// wrong. Documented, accepted, fixable when we hit it.
fn parse_extracted_scalar(s: &str) -> Value {
    let trimmed = s.trim();

    // Try direct number parse first — handles `"3.14"`, `"42"`.
    if let Ok(n) = trimmed.parse::<f64>() {
        if let Some(v) = serde_json::Number::from_f64(n).map(Value::Number) {
            return v;
        }
    }

    // Strip commas and trailing non-numeric suffix, retry.
    let stripped: String = trimmed.chars().filter(|c| *c != ',').collect();
    let numeric_prefix: String = stripped
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if !numeric_prefix.is_empty() {
        if let Ok(n) = numeric_prefix.parse::<f64>() {
            if let Some(v) = serde_json::Number::from_f64(n).map(Value::Number) {
                return v;
            }
        }
    }

    Value::String(trimmed.to_string())
}

/// Walk a dot-separated pointer through a `Value`. Numeric segments
/// address `Value::Array`; other segments address `Value::Object`.
fn walk_pointer<'v>(root: &'v Value, pointer: &str) -> Option<Value> {
    let mut current = root;
    for seg in pointer.split('.') {
        current = match current {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => {
                let idx: usize = seg.parse().ok()?;
                a.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current.clone())
}

/// Insert a value at a dotted path in a `Map`, creating intermediate
/// `Object`s as needed. Arrays are not created on the fly — a recipe
/// that tries to land a value inside an array field should use a
/// literal that already has that shape.
fn insert_at_path(
    map: &mut Map<String, Value>,
    path: &str,
    value: Value,
) -> Result<(), ApplyError> {
    let mut segs = path.split('.').peekable();
    let first = segs.next().ok_or_else(|| ApplyError::FieldMapping {
        reason: "empty field path".into(),
    })?;

    if segs.peek().is_none() {
        map.insert(first.to_string(), value);
        return Ok(());
    }

    let mut current = map
        .entry(first.to_string())
        .or_insert_with(|| Value::Object(Map::new()));

    while let Some(seg) = segs.next() {
        if segs.peek().is_none() {
            let inner = current.as_object_mut().ok_or_else(|| ApplyError::FieldMapping {
                reason: format!("path segment {seg:?} expected an object"),
            })?;
            inner.insert(seg.to_string(), value);
            return Ok(());
        } else {
            let inner = current.as_object_mut().ok_or_else(|| ApplyError::FieldMapping {
                reason: format!("path segment {seg:?} expected an object"),
            })?;
            current = inner
                .entry(seg.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{
        ExpectationRef, ExtractionSpec, FetchRecipe, FieldMap, FieldValueSource,
        ProductionBinding, RowFilter,
    };
    use crate::research::{
        DocumentSourceHint, EntityKindExpectation, EventTypeExpectation,
        MetricExpectation, RecordExpectations, RelationKindExpectation,
    };
    use chrono::TimeZone;
    use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};
    use url::Url;

    fn plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium production".into(),
            interpretation: "Global lithium production figures.".into(),
            topic_tags: vec![Topic::new("Li").unwrap()],
            geographic_scope: vec!["CL".into()],
            historical_window_days: 365,
            expectations: RecordExpectations {
                observation_metrics: vec![MetricExpectation {
                    name: "production".into(),
                    unit_hint: Some(Unit::new("t").unwrap()),
                    rationale: "Primary".into(),
                }],
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("mine_opened").unwrap(),
                    rationale: "Capacity expansion".into(),
                }],
                entity_kinds: vec![EntityKindExpectation {
                    kind: "mine".into(),
                    exemplars: vec![EntityId::new("mine:greenbushes").unwrap()],
                    rationale: "Unit of supply".into(),
                }],
                relation_kinds: vec![RelationKindExpectation {
                    kind: "operator_of".into(),
                    rationale: "Asset link".into(),
                }],
                document_sources: vec![DocumentSourceHint {
                    description: "USGS".into(),
                    preferred_source_ids: vec!["usgs_mcs".into()],
                }],
                assertion_guidance: None,
            },
            created_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
        }
    }

    fn recipe_with(extraction: ExtractionSpec) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "usgs_mcs".into(),
            source_url: Url::parse(
                "https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.csv",
            )
            .unwrap(),
            extraction,
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
                            value: json!("t"),
                        },
                    },
                    FieldMap {
                        path: "metric".into(),
                        source: FieldValueSource::FromPlan {
                            pointer: "expectations.observation_metrics.0.name".into(),
                        },
                    },
                    FieldMap {
                        path: "period".into(),
                        source: FieldValueSource::Literal { value: json!("annual") },
                    },
                ],
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
        }
    }

    fn fetched_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap()
    }

    // -----------------------------------------------------------------------
    // JsonPath extractor
    // -----------------------------------------------------------------------

    #[test]
    fn json_path_extracts_a_scalar() {
        let bytes = br#"{"data": {"production": {"chile": 49000}}}"#;
        let out = extract_json_path(bytes, "$.data.production.chile").unwrap();
        assert_eq!(out, "49000");
    }

    #[test]
    fn json_path_extracts_a_string_without_quotes() {
        let bytes = br#"{"label": "Annual production"}"#;
        let out = extract_json_path(bytes, "$.label").unwrap();
        assert_eq!(out, "Annual production");
    }

    #[test]
    fn json_path_errors_when_no_match() {
        let bytes = br#"{"a": 1}"#;
        let err = extract_json_path(bytes, "$.missing").unwrap_err();
        assert!(matches!(err, ApplyError::Extraction { mode: "json_path", .. }), "got {err:?}");
    }

    #[test]
    fn json_path_errors_when_bytes_not_json() {
        let bytes = b"not json";
        let err = extract_json_path(bytes, "$.a").unwrap_err();
        assert!(matches!(err, ApplyError::Extraction { mode: "json_path", .. }));
    }

    // -----------------------------------------------------------------------
    // CssSelect extractor
    // -----------------------------------------------------------------------

    #[test]
    fn css_select_extracts_text() {
        let html = b"<html><body><table><tr><td class='prod'>49,000</td></tr></table></body></html>";
        let out = extract_css_select(html, "td.prod", None).unwrap();
        assert_eq!(out, "49,000");
    }

    #[test]
    fn css_select_extracts_attribute() {
        let html = b"<a href='https://example.com/file.pdf'>report</a>";
        let out = extract_css_select(html, "a", Some("href")).unwrap();
        assert_eq!(out, "https://example.com/file.pdf");
    }

    #[test]
    fn css_select_errors_when_selector_matches_nothing() {
        let html = b"<p>hi</p>";
        let err = extract_css_select(html, "table", None).unwrap_err();
        assert!(matches!(err, ApplyError::Extraction { mode: "css_select", .. }));
    }

    // -----------------------------------------------------------------------
    // CsvCell extractor
    // -----------------------------------------------------------------------

    #[test]
    fn csv_cell_extracts_by_row_filter() {
        let csv = b"country,production\nAustralia,88000\nChile,49000\nArgentina,6200\n";
        let out = extract_csv_cell(
            csv,
            "production",
            Some(&RowFilter::Equals {
                column: "country".into(),
                value: "Chile".into(),
            }),
        )
        .unwrap();
        assert_eq!(out, "49000");
    }

    #[test]
    fn csv_cell_extracts_single_row_without_filter() {
        let csv = b"metric,value\nproduction,49000\n";
        let out = extract_csv_cell(csv, "value", None).unwrap();
        assert_eq!(out, "49000");
    }

    #[test]
    fn csv_cell_errors_on_ambiguous_multi_row_without_filter() {
        let csv = b"metric,value\nproduction,49000\nreserves,9300000\n";
        let err = extract_csv_cell(csv, "value", None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ambiguous"), "got {msg}");
    }

    #[test]
    fn csv_cell_errors_when_column_missing() {
        let csv = b"country,production\nChile,49000\n";
        let err = extract_csv_cell(
            csv,
            "reserves",
            Some(&RowFilter::Equals {
                column: "country".into(),
                value: "Chile".into(),
            }),
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not in headers"), "got {msg}");
    }

    #[test]
    fn csv_cell_labeled_as_filter() {
        let csv = b"row_label,value\nTotal production,49000\nTotal reserves,9300000\n";
        let out = extract_csv_cell(
            csv,
            "value",
            Some(&RowFilter::LabeledAs {
                label_column: "row_label".into(),
                label: "Total production".into(),
            }),
        )
        .unwrap();
        assert_eq!(out, "49000");
    }

    // -----------------------------------------------------------------------
    // RegexCapture extractor
    // -----------------------------------------------------------------------

    #[test]
    fn regex_capture_pulls_group_1() {
        let bytes = b"Production: 49,000 tonnes in 2024";
        let out = extract_regex(bytes, r"Production:\s*([\d,]+)\s*tonnes", 1).unwrap();
        assert_eq!(out, "49,000");
    }

    #[test]
    fn regex_capture_errors_when_no_match() {
        let err = extract_regex(b"empty", r"\d+ tonnes", 1).unwrap_err();
        assert!(matches!(err, ApplyError::Extraction { mode: "regex_capture", .. }));
    }

    #[test]
    fn regex_capture_errors_on_bad_pattern() {
        let err = extract_regex(b"x", "(unclosed", 1).unwrap_err();
        assert!(matches!(err, ApplyError::Extraction { mode: "regex_capture", .. }));
    }

    // -----------------------------------------------------------------------
    // PdfTable extractor — must fail loudly and predictably
    // -----------------------------------------------------------------------

    #[test]
    fn pdf_table_returns_not_implemented_with_clear_reason() {
        let spec = ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 3,
            col: 1,
        };
        let err = extract(&spec, b"%PDF-1.4...").unwrap_err();
        match err {
            ApplyError::NotImplemented { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("positional"), "got reason {reason}");
            }
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Scalar parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_extracted_scalar_handles_plain_number() {
        assert_eq!(parse_extracted_scalar("49000"), json!(49000.0));
    }

    #[test]
    fn parse_extracted_scalar_strips_commas() {
        assert_eq!(parse_extracted_scalar("49,000"), json!(49000.0));
    }

    #[test]
    fn parse_extracted_scalar_strips_trailing_unit() {
        assert_eq!(parse_extracted_scalar("49,000 t"), json!(49000.0));
    }

    #[test]
    fn parse_extracted_scalar_leaves_non_numeric_as_string() {
        assert_eq!(parse_extracted_scalar("Chile"), json!("Chile"));
    }

    #[test]
    fn parse_extracted_scalar_handles_decimal() {
        assert_eq!(parse_extracted_scalar("3.14"), json!(3.14));
    }

    #[test]
    fn parse_extracted_scalar_handles_negative() {
        assert_eq!(parse_extracted_scalar("-42"), json!(-42.0));
    }

    // -----------------------------------------------------------------------
    // Pointer walker
    // -----------------------------------------------------------------------

    #[test]
    fn walk_pointer_handles_object_and_array() {
        let root = json!({
            "expectations": {
                "observation_metrics": [
                    {"name": "production"},
                    {"name": "reserves"}
                ]
            }
        });
        assert_eq!(
            walk_pointer(&root, "expectations.observation_metrics.0.name"),
            Some(json!("production"))
        );
        assert_eq!(
            walk_pointer(&root, "expectations.observation_metrics.1.name"),
            Some(json!("reserves"))
        );
    }

    #[test]
    fn walk_pointer_returns_none_for_missing() {
        let root = json!({"a": 1});
        assert_eq!(walk_pointer(&root, "a.b.c"), None);
        assert_eq!(walk_pointer(&root, "missing"), None);
    }

    // -----------------------------------------------------------------------
    // insert_at_path
    // -----------------------------------------------------------------------

    #[test]
    fn insert_at_path_sets_top_level_field() {
        let mut m = Map::new();
        insert_at_path(&mut m, "value", json!(42)).unwrap();
        assert_eq!(m.get("value"), Some(&json!(42)));
    }

    #[test]
    fn insert_at_path_creates_nested_objects() {
        let mut m = Map::new();
        insert_at_path(&mut m, "a.b.c", json!("x")).unwrap();
        assert_eq!(
            Value::Object(m),
            json!({"a": {"b": {"c": "x"}}})
        );
    }

    // -----------------------------------------------------------------------
    // End-to-end: CSV recipe → Observation
    // -----------------------------------------------------------------------

    #[test]
    fn end_to_end_csv_recipe_produces_observation() {
        let csv = b"country,production\nAustralia,88000\nChile,49000\n";
        let recipe = recipe_with(ExtractionSpec::CsvCell {
            column: "production".into(),
            row_filter: Some(RowFilter::Equals {
                column: "country".into(),
                value: "Chile".into(),
            }),
        });
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: csv,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).unwrap();
        assert_eq!(records.len(), 1);
        let obs = match &records[0] {
            Record::Observation(o) => o,
            other => panic!("expected Observation, got {other:?}"),
        };
        assert_eq!(obs.content.metric, "production");
        assert_eq!(obs.content.value, 49000.0);
        assert_eq!(obs.content.unit.as_str(), "t");
        // Envelope: topic tags from plan carried through
        assert_eq!(obs.envelope.subjects.topics.len(), 1);
        assert_eq!(obs.envelope.subjects.topics[0].as_str(), "Li");
        // Provenance: recipe id embedded
        let src = &obs.envelope.provenance.source_id;
        assert!(src.starts_with("usgs_mcs#recipe:"), "got {src}");
        assert!(src.contains("@v1"), "got {src}");
        // observed_at is the caller-supplied fetched_at
        assert_eq!(obs.envelope.observed_at, fetched_at());
        // id is UUIDv7
        assert_eq!(obs.id.get_version_num(), 7);
    }

    #[test]
    fn end_to_end_pdf_recipe_fails_cleanly_with_not_implemented() {
        let recipe = recipe_with(ExtractionSpec::PdfTable {
            page: 1,
            table_index: 0,
            row: 2,
            col: 3,
        });
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: b"%PDF-1.4 fake",
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        assert!(matches!(err, ApplyError::NotImplemented { mode: "pdf_table", .. }));
    }

    #[test]
    fn end_to_end_csv_recipe_fails_cleanly_when_extraction_gives_non_numeric() {
        // A recipe that maps `value: f64` but the CSV cell is non-numeric
        // should fail at content assembly, not silently produce 0.0.
        let csv = b"country,production\nChile,unavailable\n";
        let recipe = recipe_with(ExtractionSpec::CsvCell {
            column: "production".into(),
            row_filter: Some(RowFilter::Equals {
                column: "country".into(),
                value: "Chile".into(),
            }),
        });
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: csv,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        assert!(matches!(err, ApplyError::ContentAssembly { .. }), "got {err:?}");
    }
}
