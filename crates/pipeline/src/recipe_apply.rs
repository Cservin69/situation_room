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
//! [`situation_room_secure::SecureHttpClient`] and hands us the bytes.
//! Keeping I/O out of here makes the module trivially testable with
//! inline fixtures and keeps all network-facing defences (SSRF,
//! bounded size, TLS) in one place.
//!
//! All five extraction modes are implemented as of Session 29.
//! `PdfTable` was the last to land — see ADR 0007 amendment 5 for the
//! layout-heuristic rationale.
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

use situation_room_core::schema::content::{
    EventContent, ObservationContent, RelationContent,
};
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::{Event, Observation, Record, Relation};
use situation_room_core::vocab::Confidence;
use situation_room_core::RecordType;

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

    /// Reserved for a future extraction mode that ships its enum
    /// variant before its runtime. As of Session 29 (Track C) all five
    /// modes in [`ExtractionSpec`] are wired, so no production path
    /// returns this. Kept in the enum because removing it is a
    /// breaking API change and adding a sixth mode (ADR-level) is the
    /// only legitimate way to need it again.
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
        ExtractionSpec::PdfTable {
            page,
            table_index,
            row,
            col,
        } => extract_pdf_table(bytes, *page, *table_index, *row, *col),
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
// PdfTable extractor — Session 29 (ADR 0007 amendment 5).
//
// The runtime previously returned `ApplyError::NotImplemented` for
// `pdf_table` per the Session-3 review note. The handoff for Session 29
// chose a pure-Rust layout-heuristic approach over an external Tabula
// or JBIG2 dependency: read the page's text via pdf-extract's
// `extract_text_from_mem_by_pages` (which preserves whitespace between
// text fragments), cluster contiguous lines with matching token-counts
// into "tables", and address cells positionally.
//
// **Whitespace note (Session-29 fix-1).** pdf-extract's
// `PlainTextOutput` normalizes horizontal gaps to single spaces — a
// PDF that visually has thirteen spaces between "Country" and
// "Production" comes out as `"Country Production"`. The first
// Session-29 implementation tried to require 2+ whitespace as the
// cell-boundary signal and found zero tables on every real PDF. The
// fix is to tokenize on any whitespace run (`tokenize_line`) and
// accept the multi-word-cell limitation as a known one. See
// `tokenize_line`'s docstring and ADR 0007 amendment 5 for the
// remediation paths when a recipe needs to address a multi-word
// cell value.
//
// What this *is*:
//
// - A working `pdf_table` extractor for the realistic case of an
//   authoritative annual report (USGS MCS, SEC 10-K filing, EUR-Lex
//   statistical table) whose tables are column-aligned and whose
//   cells are single-word.
// - Deterministic. Same bytes in → same string out. No glyph-cluster
//   heuristics that could drift across rustc versions; the only
//   non-determinism would come from pdf-extract itself.
//
// What this *is not*:
//
// - A general-purpose PDF table parser. Multi-word cell values
//   tokenize as multiple tokens and terminate the table at the
//   ragged-token-count row. Scanned PDFs without OCR produce no
//   extractable text. Tables with merged cells produce ragged token
//   counts that fail detection.
// - A replacement for HTML-first authoring. The recipe-author prompt
//   continues to teach "if the source has an HTML companion, author
//   against the HTML." `pdf_table` is the fallback when there
//   genuinely is no HTML.
//
// Failures are loud and structurally specific (page out of range,
// table not found, row/col out of range, empty cell), not silent.
// Each error reason names the addressing path so the operator sees
// exactly which coordinate failed.
// ---------------------------------------------------------------------------

fn extract_pdf_table(
    bytes: &[u8],
    page: u32,
    table_index: u32,
    row: u32,
    col: u32,
) -> Result<String, ApplyError> {
    // pdf-extract's per-page text reader returns a `Vec<String>`
    // where each entry is one page's text. The library does its own
    // line-reconstruction; we trust that and do the table detection
    // on top.
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes).map_err(|e| {
        ApplyError::Extraction {
            mode: "pdf_table",
            reason: format!("pdf parse failed: {e}"),
        }
    })?;

    // `page` is 1-indexed in the recipe schema (matching how PDFs are
    // referenced in publications). `0` is structurally invalid; reject
    // it explicitly so a recipe with a typo doesn't silently address
    // page 1.
    if page == 0 {
        return Err(ApplyError::Extraction {
            mode: "pdf_table",
            reason: "page must be 1-indexed; 0 is not a valid PDF page".into(),
        });
    }
    let page_idx = (page as usize) - 1;
    let page_text = pages.get(page_idx).ok_or_else(|| ApplyError::Extraction {
        mode: "pdf_table",
        reason: format!(
            "page {page} out of range (PDF has {} pages)",
            pages.len()
        ),
    })?;

    let tables = detect_pdf_tables(page_text);
    let table = tables
        .get(table_index as usize)
        .ok_or_else(|| ApplyError::Extraction {
            mode: "pdf_table",
            reason: format!(
                "table_index {table_index} not found on page {page} \
                 ({} tables detected; use table_index 0..{})",
                tables.len(),
                tables.len().saturating_sub(1)
            ),
        })?;

    let row_data = table.get(row as usize).ok_or_else(|| ApplyError::Extraction {
        mode: "pdf_table",
        reason: format!(
            "row {row} out of range on page {page}, table {table_index} \
             (table has {} rows)",
            table.len()
        ),
    })?;

    let cell = row_data.get(col as usize).ok_or_else(|| ApplyError::Extraction {
        mode: "pdf_table",
        reason: format!(
            "col {col} out of range at page {page}, table {table_index}, row {row} \
             (row has {} cells)",
            row_data.len()
        ),
    })?;

    let trimmed = cell.trim();
    if trimmed.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "pdf_table",
            reason: format!(
                "cell at page {page}, table {table_index}, row {row}, col {col} \
                 is empty after trimming"
            ),
        });
    }
    Ok(trimmed.to_string())
}

/// Detect tabular regions in a single page's text.
///
/// Algorithm (deliberately simple, deliberately explicit):
///
/// 1. Split the page text into lines.
/// 2. Tokenize each non-blank line with [`tokenize_line`] — any run
///    of whitespace separates cells. (See `tokenize_line`'s docstring
///    for why any-whitespace and not 2+-whitespace.)
/// 3. **Blank lines are skipped**, not table-terminators. pdf-extract's
///    `PlainTextOutput` is known to emit `\n\n` between adjacent text
///    objects in some PDFs (one `\n` from `end_line`, another from
///    `end_word`/`begin_word`); treating every blank line as a flush
///    would split single tables into N one-row clusters that all get
///    dropped by the min-2-rows rule. The Session-29 fix-1 lesson:
///    pdf-extract's emit-spacing is not stable enough to use blanks
///    as a structural signal.
/// 4. A *table* is a maximal run of non-blank lines whose token counts
///    are all equal and ≥ 2. Token-count change terminates the
///    current table and starts a new one. Single-token lines also
///    terminate (they're typically prose / footers / section headings).
/// 5. A run of fewer than 2 lines does not become a table.
///
/// The minimum-2-rows rule prevents stray prose lines from being
/// mistaken for tables. The minimum-2-tokens rule rejects single-
/// column "tables" which are usually paragraphs.
///
/// The returned shape is `tables[i][row][col]`. `tables` is in
/// page-reading order; `row` is in line order; `col` is in
/// left-to-right order.
fn detect_pdf_tables(page_text: &str) -> Vec<Vec<Vec<String>>> {
    let mut tables: Vec<Vec<Vec<String>>> = Vec::new();
    let mut current_table: Vec<Vec<String>> = Vec::new();
    let mut current_token_count: Option<usize> = None;

    let flush = |current_table: &mut Vec<Vec<String>>, tables: &mut Vec<Vec<Vec<String>>>| {
        if current_table.len() >= 2 {
            tables.push(std::mem::take(current_table));
        } else {
            current_table.clear();
        }
    };

    for line in page_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank lines are skipped, not terminators. See the
            // docstring above for the rationale.
            continue;
        }

        let tokens = tokenize_line(trimmed);
        if tokens.len() < 2 {
            // Single-token lines DO terminate the current table —
            // they're typically section headings, footnote markers,
            // or single-column footers, never legitimate table rows.
            flush(&mut current_table, &mut tables);
            current_token_count = None;
            continue;
        }

        match current_token_count {
            Some(n) if n == tokens.len() => {
                current_table.push(tokens);
            }
            Some(_) => {
                // Token-count mismatch: close the current table and
                // start a new one with this line.
                flush(&mut current_table, &mut tables);
                current_table.push(tokens.clone());
                current_token_count = Some(tokens.len());
            }
            None => {
                current_token_count = Some(tokens.len());
                current_table.push(tokens);
            }
        }
    }
    flush(&mut current_table, &mut tables);
    tables
}

/// Split a single line into tokens by any run of whitespace.
///
/// **Why any whitespace, not 2+ whitespace.** pdf-extract's
/// `PlainTextOutput` normalizes whitespace at extraction time: it
/// emits one space for a horizontal gap regardless of how many spaces
/// were in the original Tj operand. So a PDF that *visually* has
/// thirteen spaces between "Country" and "Production" comes out of
/// pdf-extract as `"Country Production"` (single space). The Session-29
/// initial implementation tried to require 2+ whitespace as the
/// cell-boundary signal; against real pdf-extract output it found
/// zero tables on every PDF and every test failed. Splitting on any
/// whitespace run is the one signal pdf-extract reliably preserves.
///
/// Tabs count as whitespace. Empty/whitespace-only input returns
/// an empty Vec (the caller's table detector treats that as a
/// single-token line, which terminates the current table).
///
/// **Known limitation: multi-word cell values.** A cell value with
/// internal whitespace (e.g. `"United States"`) gets tokenized as
/// two tokens. When the LLM authors a recipe targeting such a PDF,
/// the row's token count will differ from rows with single-word
/// cells, terminating the table at that row. The remediation is
/// editorial: address the cleaner sub-region with a different
/// `table_index`, switch to `regex_capture`, or fall back to
/// `static_payload` with a transcribed CSV. Documented in ADR 0007
/// amendment 5 under "What this amendment does NOT do."
fn tokenize_line(s: &str) -> Vec<String> {
    s.split_whitespace().map(|t| t.to_string()).collect()
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
fn walk_pointer(root: &Value, pointer: &str) -> Option<Value> {
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
        DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
        MetricExpectation, RecordExpectations, RelationKindExpectation,
    };
    use chrono::TimeZone;
    use situation_room_core::vocab::{EntityId, EventType, Topic, Unit};
    use url::Url;

    fn plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium production".into(),
            interpretation: "Global lithium production figures.".into(),
            topic_tags: vec![Topic::new("Li").unwrap()],
            geographic_scope: vec![GeoScope::code_only("CL")],
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
            static_payload: None,
            // ADR 0014: recipe_apply tests don't exercise authoring;
            // FetchedBytes is the optimistic-case default.
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
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
    // PdfTable extractor — Session 29 (ADR 0007 amendment 5)
    // -----------------------------------------------------------------------

    /// Synthetic 2-page PDF used for PDF-table extractor tests.
    ///
    /// Page 1: filler prose (no table; exercises the "page 1's loose
    /// prose isn't mis-detected as a table" guarantee — a recipe for
    /// `page=1, table_index=0` should fail with "table_index 0 not
    /// found", not silently return prose).
    /// Page 2: a clean 4-row × 2-column table:
    ///
    /// ```text
    ///   Country     Production
    ///   Australia   88000
    ///   Chile       49000
    ///   Argentina   6200
    /// ```
    ///
    /// See `tests/fixtures/pdf/README.md` for how this fixture was
    /// generated and how to swap in a real USGS MCS PDF when network
    /// access is available.
    const LITHIUM_PDF: &[u8] = include_bytes!(
        "../tests/fixtures/pdf/lithium_production.pdf"
    );

    #[test]
    fn tokenize_line_splits_on_any_whitespace_run() {
        // Single space, multi-space, and tab all separate cells.
        // pdf-extract collapses gaps to single spaces in its
        // PlainTextOutput, so the splitter must work on single-space
        // input (the common case in practice).
        assert_eq!(
            tokenize_line("Country Production"),
            vec!["Country".to_string(), "Production".to_string()]
        );
        assert_eq!(
            tokenize_line("Country     Production"),
            vec!["Country".to_string(), "Production".to_string()]
        );
        assert_eq!(
            tokenize_line("a\tb"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn tokenize_line_treats_multi_word_cells_as_separate_tokens() {
        // Known limitation (ADR 0007 amendment 5): pdf-extract collapses
        // multi-space gaps so the splitter cannot distinguish "a cell
        // value with internal whitespace" from "two cells separated by
        // whitespace." Multi-word cells become multiple tokens; the
        // table detector handles this by terminating the table at the
        // ragged-token-count row.
        let v = tokenize_line("United States 1234");
        assert_eq!(
            v,
            vec!["United".to_string(), "States".to_string(), "1234".to_string()]
        );
    }

    #[test]
    fn tokenize_line_returns_empty_for_blank() {
        assert!(tokenize_line("   ").is_empty());
        assert!(tokenize_line("").is_empty());
        assert!(tokenize_line("\t\n").is_empty());
    }

    #[test]
    fn detect_pdf_tables_finds_one_clean_table() {
        // Input shaped like what pdf-extract emits: single spaces
        // between cells (the multi-space PDF gaps got normalized at
        // extraction time).
        let txt = "\
Country Production
Australia 88000
Chile 49000
Argentina 6200
";
        let tables = detect_pdf_tables(txt);
        assert_eq!(tables.len(), 1, "got {tables:?}");
        assert_eq!(tables[0].len(), 4);
        assert_eq!(tables[0][0], vec!["Country".to_string(), "Production".to_string()]);
        assert_eq!(tables[0][2], vec!["Chile".to_string(), "49000".to_string()]);
    }

    #[test]
    fn detect_pdf_tables_treats_blank_lines_as_skipped_not_terminators() {
        // Per the Session-29 fix-1 lesson: pdf-extract is known to
        // emit `\n\n` between rows on some PDFs (one `\n` from
        // `end_line`, another from text-object boundaries). Treating
        // every blank line as a flush would split single tables into
        // many one-row clusters that all get dropped. The algorithm
        // skips blanks instead.
        let txt = "\
A 1
B 2

C 3
D 4
";
        let tables = detect_pdf_tables(txt);
        // Pre-fix-1 expectation was 2 tables; with skip-blanks
        // semantics it's one continuous 4-row table.
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].len(), 4);
        assert_eq!(tables[0][0], vec!["A".to_string(), "1".to_string()]);
        assert_eq!(tables[0][3], vec!["D".to_string(), "4".to_string()]);
    }

    #[test]
    fn detect_pdf_tables_terminates_on_token_count_change_even_across_blanks() {
        // A blank line on its own is not a terminator, but a
        // post-blank line with a different token count IS — the
        // mismatch is what closes the current table.
        let txt = "\
A 1
B 2

X y z
P q r
";
        let tables = detect_pdf_tables(txt);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0][0].len(), 2);
        assert_eq!(tables[1][0].len(), 3);
    }

    #[test]
    fn detect_pdf_tables_breaks_on_token_count_change() {
        // A two-cell table followed by a three-cell table — the change
        // in token count terminates the first and starts the second.
        let txt = "\
A 1
B 2
X y z
P q r
";
        let tables = detect_pdf_tables(txt);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0][0].len(), 2);
        assert_eq!(tables[1][0].len(), 3);
    }

    #[test]
    fn detect_pdf_tables_rejects_single_row_clusters() {
        // Two prose lines with mismatched token counts — each forms a
        // 1-row "cluster" which is rejected by the min-2-rows rule.
        // This is the failure mode for stray prose, footnotes, etc.
        let txt = "\
Just one line here
And then nothing.
";
        let tables = detect_pdf_tables(txt);
        // "Just one line here" → 4 tokens; "And then nothing." → 3
        // tokens; mismatch → flushes the 1-row cluster, starts a new
        // 1-row cluster, end-of-input flush drops that too.
        assert_eq!(tables.len(), 0, "got {tables:?}");
    }

    #[test]
    fn detect_pdf_tables_rejects_single_column_lines() {
        let txt = "\
just_one_token
another
third
";
        let tables = detect_pdf_tables(txt);
        assert!(tables.is_empty());
    }

    #[test]
    fn detect_pdf_tables_terminates_at_multi_word_cell_row() {
        // ADR 0007 amendment 5 known limitation, with a regression
        // test pinning the behaviour: a row with a multi-word cell
        // (e.g. "United States") tokenizes to one extra token and
        // mismatches the surrounding rows, terminating the table at
        // that row. The remediation is editorial (different
        // table_index, regex_capture, or static_payload).
        let txt = "\
A 1
B 2
United States 3
C 4
";
        let tables = detect_pdf_tables(txt);
        // "A 1" + "B 2" form the first 2-token table.
        // "United States 3" mismatches (3 tokens) → flush, start new.
        // "C 4" mismatches the 3-token current → flush 1-row "United
        //   States 3" cluster (dropped), start new 2-token cluster.
        // End: flush the 1-row "C 4" cluster (dropped).
        // Result: just the first {A 1, B 2} table.
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].len(), 2);
        assert_eq!(tables[0][0], vec!["A".to_string(), "1".to_string()]);
    }

    #[test]
    fn extract_pdf_table_happy_path_against_fixture() {
        // Page 2, table 0, row 2 (Chile data row), col 1 (production
        // value): "49000". This mirrors the recipe a USGS-authored
        // PDF recipe would use.
        let out = extract_pdf_table(LITHIUM_PDF, 2, 0, 2, 1).unwrap();
        assert_eq!(out, "49000");
    }

    #[test]
    fn extract_pdf_table_addresses_header_row() {
        // row 0 = header. col 0 = "Country", col 1 = "Production".
        let header_country = extract_pdf_table(LITHIUM_PDF, 2, 0, 0, 0).unwrap();
        let header_prod = extract_pdf_table(LITHIUM_PDF, 2, 0, 0, 1).unwrap();
        assert_eq!(header_country, "Country");
        assert_eq!(header_prod, "Production");
    }

    #[test]
    fn extract_pdf_table_errors_on_zero_page() {
        let err = extract_pdf_table(LITHIUM_PDF, 0, 0, 0, 0).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("1-indexed"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_table_errors_on_page_out_of_range() {
        // The fixture has 2 pages; asking for page 99 fails predictably.
        let err = extract_pdf_table(LITHIUM_PDF, 99, 0, 0, 0).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("out of range"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_table_errors_on_table_not_found() {
        // Page 1 of the fixture is loose prose; no table is detected.
        // Recipes addressing page=1, table_index=0 fail predictably
        // rather than silently returning prose tokens.
        let err = extract_pdf_table(LITHIUM_PDF, 1, 0, 0, 0).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("table_index"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_table_errors_on_row_out_of_range() {
        // Page 2's table has 4 rows (header + 3 data); asking for row
        // 99 fails predictably.
        let err = extract_pdf_table(LITHIUM_PDF, 2, 0, 99, 0).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("row 99"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_table_errors_on_col_out_of_range() {
        // Page 2's table has 2 columns; asking for col 99 fails.
        let err = extract_pdf_table(LITHIUM_PDF, 2, 0, 1, 99).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("col 99"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_table_errors_on_invalid_pdf_bytes() {
        let err = extract_pdf_table(b"not a pdf at all", 1, 0, 0, 0).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "pdf_table");
                assert!(reason.contains("pdf parse failed"), "got {reason}");
            }
            other => panic!("expected Extraction, got {other:?}"),
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
        // A bland decimal — earlier this test used `3.14`, which
        // clippy's `approx_constant` flagged as a stand-in for π.
        // The test cares about decimal-parsing correctness, not the
        // numeric value, so `1.5` works fine.
        assert_eq!(parse_extracted_scalar("1.5"), json!(1.5));
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
    fn end_to_end_pdf_recipe_produces_observation() {
        // Session 29: pdf_table is wired. A recipe that addresses
        // (page=2, table_index=0, row=2, col=1) on the lithium fixture
        // extracts "49000", which `parse_extracted_scalar` parses as
        // f64 and which flows into the Observation's `value` field —
        // identical end-state to the CSV / JSON / CSS / regex paths.
        let recipe = recipe_with(ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 2,
            col: 1,
        });
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: LITHIUM_PDF,
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
        // Provenance carries the recipe id + version like the other
        // wired modes.
        let src = &obs.envelope.provenance.source_id;
        assert!(src.starts_with("usgs_mcs#recipe:"), "got {src}");
        assert!(src.contains("@v1"), "got {src}");
    }

    #[test]
    fn end_to_end_pdf_recipe_fails_cleanly_when_address_is_out_of_range() {
        // A recipe addressing a non-existent row surfaces as
        // ApplyError::Extraction at the apply boundary, not
        // ApplyError::NotImplemented. Replaces the pre-Session-29
        // canary that asserted NotImplemented.
        let recipe = recipe_with(ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 99,
            col: 0,
        });
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: LITHIUM_PDF,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        assert!(
            matches!(err, ApplyError::Extraction { mode: "pdf_table", .. }),
            "got {err:?}"
        );
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
