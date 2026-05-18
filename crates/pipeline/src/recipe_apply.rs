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

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use csv::ReaderBuilder;
use jsonpath_rust::JsonPath;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::{json, Map, Value};
use thiserror::Error;
use tracing::debug;
use uuid::Uuid;

use situation_room_core::schema::content::{
    EventContent, ObservationContent, RelationContent,
};
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::{Entity, Event, Observation, Record, Relation};
use situation_room_core::vocab::{Confidence, EntityId};
use situation_room_core::RecordType;

use crate::recipes::{
    ExpectationRef, ExtractionSpec, FetchRecipe, FieldMap, FieldValueSource,
    ProductionBinding, RowFilter,
};
use crate::research::ResearchPlan;

// ---------------------------------------------------------------------------
// Runtime bounds on extracted values and error messages
// ---------------------------------------------------------------------------
//
// Session 33: live evidence from a hungarian-barley-production run
// against `usgs_mcs` produced an apply error whose Display rendered a
// ~12 KB HTML/JSON blob into the `RecipeOutcome::Failed.message`
// field. The blob was the rendered text of the USGS MCS landing
// page (the `<noscript>` block plus a Drupal page-state JSON
// embedded at the bottom), which the LLM-authored CSS selector had
// pulled wholesale. `parse_extracted_scalar` kept it as a String,
// content assembly tried to coerce it to `f64`, and serde_json's
// error message included the entire offending value. The 12 KB
// message then propagated through:
//
//   - the run log line in `fetch_executor::run_recipes`
//   - the `recipe_fetch_attempts.error_summary` SQLite column
//   - the desktop UI's recipe panel
//
// rendering each unreadable.
//
// The fix has two layers, both narrow:
//
//   - At each extractor's exit, reject any scalar above
//     `EXTRACTED_SCALAR_MAX_BYTES` with a small named error. Recipes
//     produce records whose largest scalar field — an event title or
//     a multi-paragraph description — does not legitimately exceed a
//     couple of kilobytes. A multi-KB extraction is a recipe that
//     doesn't fit the data; ADR 0007 says the runtime catches that.
//
//   - When content assembly does fail despite the upstream bound (a
//     legitimate-sized non-numeric string mapped to `f64`, say),
//     truncate the serde_json error message at a generous cap before
//     wrapping into `ApplyError::ContentAssembly`. The structurally
//     informative parts of the message — `invalid type: string …,
//     expected f64` — sit at the head and tail; the middle is the
//     offending value, which we summarise rather than reproduce.
//
// Document records are not producible from recipes (`build_record`
// rejects them), so no recipe-extracted field legitimately needs the
// large-body affordance. Bound applies uniformly to every mode.

/// Maximum byte length of an extracted scalar value, across all five
/// extraction modes. A multi-KB extraction means the recipe targets
/// a container, not a leaf — refining the recipe is the right
/// response, and the runtime's job is to surface that cleanly.
const EXTRACTED_SCALAR_MAX_BYTES: usize = 2048;

/// Maximum char length of an `ApplyError::ContentAssembly` reason
/// before truncation. The body of an apply error is surfaced in run
/// logs, in the `recipe_fetch_attempts` table, and in the desktop
/// UI's recipe panel. A multi-KB blob makes all three illegible.
const CONTENT_ASSEMBLY_REASON_MAX_CHARS: usize = 600;

/// ADR 0016 §Consequences: hard cap on records-per-recipe in
/// iterator mode. A listing page with 10 000 items would otherwise
/// produce 10 000 records per fetch; capping at 500 surfaces the
/// truncation as a structured error rather than letting the recipe
/// silently flood the database.
///
/// Phase 1 chose 500 as a generous bound for real-world listings:
/// Nature subjects (~30), arXiv recent (~50), RSS feeds (typically
/// ≤ 50), USPTO patent search (≤ 100 per page), agency publication
/// indexes (≤ 100 per page). The largest realistic listings sit
/// well under the cap; anything above is either a
/// pagination-shaped source (a separate ADR) or a recipe pointed at
/// the wrong tier of resource (an authoring error the operator
/// should see and reject).
///
/// Overflow surfaces as `ApplyError::Extraction { mode: <iter_mode>,
/// reason: "iterator produced N matches; cap is 500" }` so the
/// operator's mental model — "the runtime catches mismatches; that's
/// the trust contract" — stays intact under iteration.
pub const MAX_RECORDS_PER_RECIPE: usize = 500;

/// Reject an extraction whose result exceeds the scalar bound.
/// Returns the original string unchanged when within bound.
///
/// `mode` is the same string used in the extractor's other
/// `ApplyError::Extraction` returns (`"json_path"`, `"css_select"`,
/// etc.) so log filters and UI categorisation continue to work.
fn bound_extracted(out: String, mode: &'static str) -> Result<String, ApplyError> {
    if out.len() <= EXTRACTED_SCALAR_MAX_BYTES {
        return Ok(out);
    }
    // Preview is short on purpose — it's there to give the operator a
    // hint at *which* container got selected, not to reproduce its
    // contents. Char-based truncation keeps the preview UTF-8-safe
    // even when the extracted bytes are arbitrary text.
    let preview: String = out.chars().take(120).collect();
    Err(ApplyError::Extraction {
        mode,
        reason: format!(
            "extraction returned {} bytes; recipes produce single \
             scalar values and the runtime caps individual field \
             values at {} bytes. Likely cause: the selector matches a \
             container element (body, div, table) instead of a leaf, \
             or the JSON path resolves to an object/array instead of \
             a scalar. Refine the recipe to target a leaf value. \
             Preview: {:?}",
            out.len(),
            EXTRACTED_SCALAR_MAX_BYTES,
            preview
        ),
    })
}

/// Truncate a content-assembly reason that grew too long because
/// serde_json stamped a multi-byte value into its error string.
///
/// Strategy: preserve a head and a tail so both the prefix
/// (`observation content: invalid type: string`) and the suffix
/// (`, expected f64`) survive. The middle — the offending value —
/// is replaced by a length marker.
fn truncate_content_assembly_reason(reason: String) -> String {
    let total_chars = reason.chars().count();
    if total_chars <= CONTENT_ASSEMBLY_REASON_MAX_CHARS {
        return reason;
    }
    // Reserve room for the marker. The head + tail share the
    // remainder roughly 2:1 — the head carries the type description
    // (more useful) and the tail carries the expected-type hint.
    let marker_len = 48;
    let body_budget = CONTENT_ASSEMBLY_REASON_MAX_CHARS.saturating_sub(marker_len);
    let head_target = (body_budget * 2) / 3;
    let tail_target = body_budget - head_target;

    let head: String = reason.chars().take(head_target).collect();
    let tail_rev: Vec<char> = reason.chars().rev().take(tail_target).collect();
    let tail: String = tail_rev.into_iter().rev().collect();

    format!(
        "{head} … [value truncated, total {total_chars} chars] … {tail}"
    )
}

// ---------------------------------------------------------------------------
// Schema-aware coercion at content assembly (Session 64)
// ---------------------------------------------------------------------------
//
// Background. `parse_extracted_scalar` is type-blind: every numeric-
// looking leaf becomes a `serde_json::Value::Number`. That is the
// correct default for `ObservationContent::value: f64` — the most
// common recipe target — but it is wrong whenever the binding's path
// resolves to a JSON String at the schema level.
//
// The Session 64 incident: a persisted recipe for federalreserve.gov
// produced records on its first apply (the leaf rendered as a
// non-numeric date string), then began failing on subsequent applies
// with `event content: invalid type: floating point '22.0', expected
// a string`. The recipe was unchanged (`load_or_author_recipes`
// short-circuits when a stored recipe exists). The source's leaf had
// drifted to a numeric-looking shape between fetches. The type-blind
// coercion turned that drift into a hard schema failure instead of a
// still-acceptable string.
//
// Fix shape. Resolve every field value as today, then — *before*
// inserting into the assembled JSON — consult the target field's
// expected JSON shape and stringify `Value::Number` when the schema
// expects a String. This sits at the same layer as the rest of
// `build_record`: above `serde_json::from_value` (so the assembled
// JSON we hand to serde is already type-consistent) and below
// `resolve_field_value` (so each `FieldValueSource` keeps its
// independent semantics).
//
// Why not push the decision into `parse_extracted_scalar`. The
// extractor doesn't know the binding's target record type or path —
// `FieldMap` carries that information, but `parse_extracted_scalar`
// receives only the raw extracted string. Threading the schema
// expectation down would require changing the recipe wire shape
// (`FieldMap` would need a target-type field) and re-authoring every
// existing recipe. The build-time coercion is a narrower change that
// fixes the same volatility without disturbing the recipe schema.
//
// Why not catch the serde error and retry. The error doesn't carry
// the offending field's path in a stable, parseable form, and a
// blanket "stringify all numerics on retry" would clobber legitimate
// f64 fields (`ObservationContent::value`) on records whose failure
// was elsewhere. The proactive coercion path is both narrower (only
// known-String paths) and stable (no error-message parsing).
//
// The String-path set is hardcoded per `RecordType`. Each entry
// corresponds to a `String`, an enum-as-string, or a newtype-around-
// String (`Unit`, `EntityId`) on the respective content struct in
// `crates/core/src/schema/content.rs`. Nested paths through
// `magnitude: Option<ObservationContent>` are included on `Event` and
// `Relation` because the runtime can bind into them. The set is
// deliberately small and explicit; if a new String-typed field is
// added to a content struct, an entry must be added here, and a
// failing assembly will keep the operator honest until it is.

/// Paths on a record's content tree whose schema type is JSON String
/// (a `String`, an enum, or a newtype around `String`). When a
/// FieldMap's path matches one of these, the resolved
/// `serde_json::Value` is coerced from `Number` to `String` before
/// it lands in the assembled JSON. Matching is exact on the
/// dot-separated path the FieldMap carries.
fn path_expects_string(record_type: RecordType, path: &str) -> bool {
    let entries: &[&str] = match record_type {
        // ObservationContent: `metric: String`, `unit: Unit(String)`,
        // `currency: Option<Currency>` (enum), `period:
        // ObservationPeriod` (enum). `value` and `value_uncertainty`
        // are f64; `geometry` is an object.
        RecordType::Observation => &[
            "metric",
            "unit",
            "currency",
            "period",
        ],
        // EventContent: `event_type: EventType` (enum), `headline:
        // String`, `direction: Option<EventDirection>` (enum).
        // `magnitude: Option<ObservationContent>` — nested
        // String-paths inherited from Observation.
        RecordType::Event => &[
            "event_type",
            "headline",
            "direction",
            "magnitude.metric",
            "magnitude.unit",
            "magnitude.currency",
            "magnitude.period",
        ],
        // RelationContent: `kind: String`, `from: EntityId(String)`,
        // `to: EntityId(String)`. `magnitude` mirrors Event.
        RecordType::Relation => &[
            "kind",
            "from",
            "to",
            "magnitude.metric",
            "magnitude.unit",
            "magnitude.currency",
            "magnitude.period",
        ],
        // Session 97 Lever B — EntityContent's three required fields
        // (entity_id, kind, canonical_name) are all schema-typed as
        // String / String-newtype. Geometry is an object and not in
        // this set; future versions can plumb geometry once
        // recipe-author learns to bind it.
        RecordType::Entity => &[
            "entity_id",
            "kind",
            "canonical_name",
        ],
        // Document / Assertion are not recipe-producible (build_record
        // rejects them with a Binding error before reaching this
        // layer). Return the empty set so a future hand-rolled call
        // site doesn't silently mis-coerce.
        RecordType::Document | RecordType::Assertion => &[],
    };
    entries.iter().any(|e| *e == path)
}

/// Coerce `Value::Number` to `Value::String` in-place when the field
/// at `path` is schema-typed as String. No-op for non-Number values
/// and for paths that are not in the String set. The numeric
/// representation comes from `serde_json`'s own `Display` impl on
/// `Number` so `22.0` round-trips to `"22.0"` and `22` round-trips
/// to `"22"`.
fn coerce_for_string_path(value: &mut Value, record_type: RecordType, path: &str) {
    if !path_expects_string(record_type, path) {
        return;
    }
    if let Value::Number(n) = value {
        *value = Value::String(n.to_string());
    }
    // Booleans, nulls, and nested objects/arrays are intentionally
    // left alone. Serde will reject them at deserialization with the
    // record content type's actual constraints — the right failure
    // signal for those shapes, which the operator should see.
}

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
/// may produce multiple records per fetch. Two cardinality cases:
///
/// - **Scalar recipe** (`recipe.iterator` is `None`, the
///   pre-Session-38 contract): one `extract` call, then one record
///   per `produces` binding. Output length equals
///   `recipe.produces.len()`.
///
/// - **Iterator recipe** (ADR 0016, Session 38, `recipe.iterator`
///   is `Some`): the iterator selects N matches against the fetched
///   document; for each match the recipe's `extraction` field is
///   evaluated *scoped to that match's sub-tree*; one record is
///   produced per match per binding. Output length is
///   `N * recipe.produces.len()`, capped at
///   [`MAX_RECORDS_PER_RECIPE`] across the whole recipe.
pub fn apply(ctx: ApplyContext<'_>) -> Result<Vec<Record>, ApplyError> {
    match &ctx.recipe.iterator {
        None => apply_scalar(ctx),
        Some(iter_spec) => apply_iterator(ctx, iter_spec),
    }
}

/// Pre-Session-38 scalar contract: one extract, one record per
/// binding. ADR 0019 Phase 2A extends this with the
/// **scalar + multi-leaf** case — a scalar recipe (no iterator)
/// whose binding uses `FieldValueSource::ExtractedInner` for several
/// fields. The runtime resolves the inner sub-specs against the
/// outer extraction's scope: for `css_select` against the first
/// element matching the outer selector; for `json_path` against the
/// first value resolved by the outer path. csv_cell / pdf_table /
/// regex_capture outer modes are rejected at the validator (ADR 0019
/// rule iv); reaching them here would mean a hand-edited recipe.
fn apply_scalar(ctx: ApplyContext<'_>) -> Result<Vec<Record>, ApplyError> {
    // 1. Extract a single scalar value from the bytes. This populates
    //    the legacy `Extracted`-source FieldMaps; multi-leaf bindings
    //    don't read it.
    let extracted = extract(&ctx.recipe.extraction, ctx.bytes)?;

    // 2. Detect whether any binding uses ExtractedInner. If so, we
    //    must resolve a per-binding scope so inner sub-specs can run
    //    against it. The detection is cheap (a single pass over the
    //    bindings); doing it once up-front lets the legacy single-
    //    scalar path keep its zero-allocation contract.
    let any_inner = ctx.recipe.produces.iter().any(|b| {
        b.field_mappings
            .iter()
            .any(|fm| matches!(fm.source, FieldValueSource::ExtractedInner { .. }))
    });

    // 3. Compute the inner-extractions map per binding once (scalar
    //    mode produces one record per binding, so the "match scope"
    //    is shared across bindings — the document/JSON root scoped to
    //    the outer extraction's first hit).
    let inner_by_binding: Option<Vec<Option<HashMap<String, String>>>> = if any_inner {
        Some(scalar_inner_extractions(&ctx)?)
    } else {
        None
    };

    // 4. For each binding, build one record. Scalar-mode records
    //    carry `dedup_key: None` — the pre-Session-38 contract,
    //    documented in ADR 0016 §Carry-forward dependencies. A
    //    future session may backfill scalar dedup keys; iteration
    //    is not the place to clean it up universally.
    let mut records = Vec::with_capacity(ctx.recipe.produces.len());
    for (idx, binding) in ctx.recipe.produces.iter().enumerate() {
        let inner = inner_by_binding.as_ref().and_then(|v| v[idx].as_ref());
        let record = build_record(binding, idx, &extracted, inner, &ctx, None)
            .and_then(|r| crate::normalize::finalize(r, ctx.plan, ctx.recipe))?;
        records.push(record);
    }

    Ok(records)
}

/// ADR 0019 Phase 2A scalar+multi-leaf scope resolution.
///
/// For css_select outer: the scope is the first element matching the
/// outer selector. Inner sub-selectors then run within that element's
/// sub-tree (`ElementRef::select`).
///
/// For json_path outer: the scope is the first non-null value
/// resolved by the outer path. Inner sub-paths then run against that
/// value.
///
/// Returns a vector of optional inner-extraction maps, one slot per
/// binding (same index as `ctx.recipe.produces`). A `None` slot means
/// the binding has no ExtractedInner FieldMaps and the legacy single-
/// scalar path applies.
fn scalar_inner_extractions(
    ctx: &ApplyContext<'_>,
) -> Result<Vec<Option<HashMap<String, String>>>, ApplyError> {
    match &ctx.recipe.extraction {
        ExtractionSpec::CssSelect { selector, .. } => {
            let html_str =
                std::str::from_utf8(ctx.bytes).map_err(|e| ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!("bytes were not UTF-8: {e}"),
                })?;
            let doc = Html::parse_document(html_str);
            let outer_sel = Selector::parse(selector).map_err(|e| ApplyError::Extraction {
                mode: "css_select",
                reason: format!(
                    "outer selector did not parse (scalar+multi-leaf scope): {e}"
                ),
            })?;
            let scope = doc.select(&outer_sel).next().ok_or_else(|| {
                ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!(
                        "outer selector {selector:?} matched no elements; \
                         scalar+multi-leaf bindings need an outer scope \
                         (ADR 0019 §\"Semantics, by recipe shape\")"
                    ),
                }
            })?;
            ctx.recipe
                .produces
                .iter()
                .map(|b| compute_inner_extractions_css(scope, b))
                .collect()
        }
        ExtractionSpec::JsonPath { path } => {
            let value: Value =
                serde_json::from_slice(ctx.bytes).map_err(|e| ApplyError::Extraction {
                    mode: "json_path",
                    reason: format!("bytes did not parse as JSON: {e}"),
                })?;
            let nodes: Vec<&Value> = value.query(path).map_err(|e| ApplyError::Extraction {
                mode: "json_path",
                reason: format!("outer path query failed: {e}"),
            })?;
            let scope = nodes
                .iter()
                .find(|n| !matches!(n, Value::Null))
                .copied()
                .ok_or_else(|| ApplyError::Extraction {
                    mode: "json_path",
                    reason: format!(
                        "outer path {path:?} matched no non-null nodes; \
                         scalar+multi-leaf bindings need an outer scope \
                         (ADR 0019 §\"Semantics, by recipe shape\")"
                    ),
                })?;
            ctx.recipe
                .produces
                .iter()
                .map(|b| compute_inner_extractions_json(scope, b))
                .collect()
        }
        other => {
            // Validator rule (iv) blocks ExtractedInner in non-CSS/
            // non-JSON outer modes; reaching here means a hand-edit
            // or pre-validation legacy. Phase 2B will extend this.
            Err(ApplyError::NotImplemented {
                mode: "extracted_inner",
                reason: format!(
                    "scalar+multi-leaf bindings under outer mode {} are \
                     not implemented in Phase 2A; supported outer modes \
                     are css_select and json_path (ADR 0019 §\"Two-phase \
                     rollout\"). csv_cell / pdf_table / regex_capture \
                     defer to Phase 2B.",
                    mode_name(other),
                ),
            })
        }
    }
}

/// ADR 0016 iterator path. Evaluate the iterator against the bytes
/// to obtain N matches; for each match, evaluate the recipe's
/// `extraction` scoped to that match's sub-tree; build one record
/// per match per binding, stamping a per-record `dedup_key` from
/// the binding's `dedup_key_field`.
///
/// Phase 1 wires the `css_select` × `css_select` pair only. Other
/// modes return `ApplyError::NotImplemented` with a precise message
/// pointing the operator at the Phase 2 boundary. The validator
/// (`build_validated_recipe`) only persists congruent pairs, so an
/// iterator-bearing recipe's iter-mode and inner-mode always agree
/// at this point — but defensive checks are cheap and the failure
/// shape is more informative than a panic.
fn apply_iterator(
    ctx: ApplyContext<'_>,
    iter_spec: &ExtractionSpec,
) -> Result<Vec<Record>, ApplyError> {
    match (iter_spec, &ctx.recipe.extraction) {
        (
            ExtractionSpec::CssSelect {
                selector: iter_selector,
                attribute: _,
            },
            ExtractionSpec::CssSelect {
                selector: inner_selector,
                attribute: inner_attribute,
            },
        ) => apply_css_iterator(
            ctx,
            iter_selector,
            inner_selector,
            inner_attribute.as_deref(),
        ),
        (
            ExtractionSpec::JsonPath {
                path: iter_path,
            },
            ExtractionSpec::JsonPath {
                path: inner_path,
            },
        ) => apply_json_iterator(ctx, iter_path, inner_path),
        (iter_other, inner_other) => {
            // The validator should reject these at authoring time;
            // surfacing here means a hand-edit or a Phase-2-shaped
            // recipe arrived at the runtime. Name both modes so
            // the operator can act.
            let iter_name = mode_name(iter_other);
            let inner_name = mode_name(inner_other);
            Err(ApplyError::NotImplemented {
                mode: "iterator",
                reason: format!(
                    "iterator runtime is wired for css_select × css_select \
                     (ADR 0016 Phase 1) and json_path × json_path (ADR 0019 \
                     Phase 2A). Got iter={iter_name}, inner={inner_name}. \
                     Other modes (csv_cell, pdf_table, regex_capture) are \
                     tracked for Phase 2B; until then, recipes against \
                     listing-shaped sources of those modes should decline \
                     at authoring time."
                ),
            })
        }
    }
}

/// Tag a closed-vocabulary mode with its serde-canonical name. Used
/// for iterator-path error messages and the Phase-2 `NotImplemented`
/// branch above. Centralized so the strings can't drift from the
/// `serde(rename_all = "snake_case")` discriminator on
/// [`ExtractionSpec`].
fn mode_name(spec: &ExtractionSpec) -> &'static str {
    match spec {
        ExtractionSpec::JsonPath { .. } => "json_path",
        ExtractionSpec::CssSelect { .. } => "css_select",
        ExtractionSpec::CsvCell { .. } => "csv_cell",
        ExtractionSpec::PdfTable { .. } => "pdf_table",
        ExtractionSpec::RegexCapture { .. } => "regex_capture",
    }
}

/// Iterate over CSS-selected DOM nodes, applying the inner CSS
/// selector to each match's sub-tree. ADR 0016's Phase 1 path.
///
/// `scraper::ElementRef::select` is the load-bearing primitive: it
/// runs the inner selector against the matched element's *sub-tree*
/// rather than the whole document, which is what "scope to the
/// matched node" means for the CSS mode. We use `next()` on the
/// per-match select iterator (first-match-within-scope) — the same
/// posture as scalar `extract_css_select` — because Phase 1 is
/// "single extracted field per match" (ADR 0016 §"Phase 1"). Phase
/// 2 will move to per-field sub-extractors per binding.
///
/// The cap [`MAX_RECORDS_PER_RECIPE`] applies to the *number of
/// matches*, not the post-binding total. We bound the iterator's
/// output before doing any per-match work so a runaway listing
/// (10 000 cards) costs ~one `Selector::parse` call plus the cap
/// check, not 10 000 sub-extractions.
fn apply_css_iterator(
    ctx: ApplyContext<'_>,
    iter_selector: &str,
    inner_selector: &str,
    inner_attribute: Option<&str>,
) -> Result<Vec<Record>, ApplyError> {
    let html_str = std::str::from_utf8(ctx.bytes).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("bytes were not UTF-8: {e}"),
    })?;
    let doc = Html::parse_document(html_str);

    let iter_sel = Selector::parse(iter_selector).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("iterator selector did not parse: {e}"),
    })?;
    let inner_sel = Selector::parse(inner_selector).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("inner selector did not parse: {e}"),
    })?;

    // Materialise matches into a Vec so we can cap and count up
    // front. `Html::select` returns an iterator; collecting is
    // cheap (it stores `ElementRef`, which is a thin reference,
    // not a node copy).
    let matches: Vec<scraper::ElementRef<'_>> = doc.select(&iter_sel).collect();
    if matches.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: format!(
                "iterator selector {iter_selector:?} matched no elements"
            ),
        });
    }
    if matches.len() > MAX_RECORDS_PER_RECIPE {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: format!(
                "iterator produced {} matches; cap is {} (ADR 0016 \
                 §Consequences). Likely cause: the iterator selector \
                 matches too broadly (every link rather than every card), \
                 or the source is a pagination-shaped listing whose \
                 first page already exceeds the cap. Refine the selector \
                 to target distinct cards, or pick a narrower listing \
                 endpoint.",
                matches.len(),
                MAX_RECORDS_PER_RECIPE,
            ),
        });
    }

    let mut records = Vec::with_capacity(matches.len() * ctx.recipe.produces.len());
    for matched in matches.iter() {
        // Per-match extraction: run the inner selector against the
        // matched node's *sub-tree* (`ElementRef::select`), not
        // against the whole document. Take the first match within
        // scope — Phase 1's single-extracted-field-per-match
        // contract.
        //
        // For multi-leaf bindings (ADR 0019 Phase 2A) the
        // `extracted` value is unused at the FieldMap level, but
        // we still run the outer inner-selector so dedup_key_field
        // resolution and shape-validator parity remain stable, and
        // so legacy bindings with `FieldValueSource::Extracted` in
        // a recipe that also has ExtractedInner-bearing siblings
        // continue to see a value. The validator's mutual-exclusion
        // rule keeps these populations from intermingling within
        // one binding, so the same extracted scalar can be passed
        // to every binding without ambiguity.
        let extracted = extract_css_within(*matched, &inner_sel, inner_attribute)?;

        for (idx, binding) in ctx.recipe.produces.iter().enumerate() {
            // ADR 0019 Phase 2A: pre-compute inner extractions for
            // this (binding, match) pair. Returns `None` when the
            // binding uses no ExtractedInner FieldMaps; in that case
            // the build path is the legacy single-scalar contract.
            let inner_extractions = compute_inner_extractions_css(*matched, binding)
                .map_err(|e| match e {
                    ApplyError::Extraction { mode, reason } => ApplyError::Extraction {
                        mode,
                        reason: format!("binding[{idx}] ExtractedInner: {reason}"),
                    },
                    other => other,
                })?;

            // Resolve the dedup_key_field's value from the just-
            // -extracted scalar (or the inner-extractions map, if
            // the dedup_key_field's source is ExtractedInner). The
            // validator guaranteed `dedup_key_field` is present and
            // references a path in `field_mappings`; we re-read the
            // binding here because that resolution is per-record
            // and depends on the per-match `extracted` value.
            let dedup_key = compute_dedup_key(
                binding,
                &extracted,
                inner_extractions.as_ref(),
                ctx.recipe,
            )?;
            let record = build_record(
                binding,
                idx,
                &extracted,
                inner_extractions.as_ref(),
                &ctx,
                Some(dedup_key),
            )
            .and_then(|r| crate::normalize::finalize(r, ctx.plan, ctx.recipe))?;
            records.push(record);
        }
    }

    Ok(records)
}

/// ADR 0019 Phase 2A: pre-compute inner extractions for a CSS-mode
/// binding against a per-match DOM scope.
///
/// Returns `None` when the binding has no `FieldValueSource::ExtractedInner`
/// FieldMaps — the legacy single-scalar path is unchanged in that case
/// and we avoid the allocation. Returns `Some(map)` otherwise, with
/// one entry per `ExtractedInner` FieldMap, keyed by FieldMap path.
///
/// The inner sub-spec's mode is guaranteed `css_select` by the
/// validator's mode-congruence rule (ADR 0019 rule (i)); the runtime
/// surfaces a `NotImplemented` error for other modes as a belt-and-
/// braces guard against hand-edits.
fn compute_inner_extractions_css(
    scope: scraper::ElementRef<'_>,
    binding: &ProductionBinding,
) -> Result<Option<HashMap<String, String>>, ApplyError> {
    let needs_inner = binding
        .field_mappings
        .iter()
        .any(|fm| matches!(fm.source, FieldValueSource::ExtractedInner { .. }));
    if !needs_inner {
        return Ok(None);
    }

    let mut out: HashMap<String, String> = HashMap::new();
    for fm in &binding.field_mappings {
        let spec = match &fm.source {
            FieldValueSource::ExtractedInner { spec } => spec,
            _ => continue,
        };
        let leaf = match spec {
            ExtractionSpec::CssSelect {
                selector,
                attribute,
            } => {
                let sel = Selector::parse(selector).map_err(|e| ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!(
                        "FieldMap {:?} inner selector {selector:?} did not parse: {e}",
                        fm.path
                    ),
                })?;
                extract_css_within(scope, &sel, attribute.as_deref()).map_err(|e| {
                    // Wrap the underlying error with the FieldMap path so
                    // multi-field decline messages name which leaf failed.
                    match e {
                        ApplyError::Extraction { mode, reason } => ApplyError::Extraction {
                            mode,
                            reason: format!("FieldMap {:?}: {reason}", fm.path),
                        },
                        other => other,
                    }
                })?
            }
            other => {
                return Err(ApplyError::NotImplemented {
                    mode: "extracted_inner",
                    reason: format!(
                        "FieldMap {:?}: ExtractedInner sub-spec mode {} is not \
                         supported in Phase 2A inside a css_select scope. The \
                         validator should have rejected this at authoring time \
                         (ADR 0019 rule (iv)); the row may be a hand-edit or \
                         pre-validation legacy.",
                        fm.path,
                        mode_name(other),
                    ),
                });
            }
        };
        out.insert(fm.path.clone(), leaf);
    }
    Ok(Some(out))
}

/// ADR 0019 Phase 2A: pre-compute inner extractions for a JSON-mode
/// binding against a per-match JSON value scope.
///
/// Same shape as [`compute_inner_extractions_css`] but for JSONPath
/// inner sub-specs evaluated against a scoped JSON value (e.g. one
/// element of an array the iterator yielded).
fn compute_inner_extractions_json(
    scope: &Value,
    binding: &ProductionBinding,
) -> Result<Option<HashMap<String, String>>, ApplyError> {
    let needs_inner = binding
        .field_mappings
        .iter()
        .any(|fm| matches!(fm.source, FieldValueSource::ExtractedInner { .. }));
    if !needs_inner {
        return Ok(None);
    }

    let mut out: HashMap<String, String> = HashMap::new();
    for fm in &binding.field_mappings {
        let spec = match &fm.source {
            FieldValueSource::ExtractedInner { spec } => spec,
            _ => continue,
        };
        let leaf = match spec {
            ExtractionSpec::JsonPath { path } => {
                extract_json_within(scope, path).map_err(|e| match e {
                    ApplyError::Extraction { mode, reason } => ApplyError::Extraction {
                        mode,
                        reason: format!("FieldMap {:?}: {reason}", fm.path),
                    },
                    other => other,
                })?
            }
            other => {
                return Err(ApplyError::NotImplemented {
                    mode: "extracted_inner",
                    reason: format!(
                        "FieldMap {:?}: ExtractedInner sub-spec mode {} is not \
                         supported in Phase 2A inside a json_path scope. The \
                         validator should have rejected this at authoring time \
                         (ADR 0019 rule (iv)); the row may be a hand-edit or \
                         pre-validation legacy.",
                        fm.path,
                        mode_name(other),
                    ),
                });
            }
        };
        out.insert(fm.path.clone(), leaf);
    }
    Ok(Some(out))
}

/// ADR 0019 Phase 2A: iterate over JSON-pathed matches and apply the
/// recipe's bindings to each. Mirrors [`apply_css_iterator`] for the
/// JSON case: the iterator's outer path resolves to an array of
/// objects (or any sequence of values); per element, the recipe's
/// `extraction` path resolves to the legacy single-leaf value, and
/// any `ExtractedInner` FieldMaps resolve via per-FieldMap inner
/// paths against the same element scope.
///
/// The cap [`MAX_RECORDS_PER_RECIPE`] applies to the *number of
/// matches*. We bound the iterator's output before doing any
/// per-element work so a runaway listing (a 10 000-element API
/// response) costs ~one JSON parse plus the cap check, not 10 000
/// sub-extractions.
fn apply_json_iterator(
    ctx: ApplyContext<'_>,
    iter_path: &str,
    inner_path: &str,
) -> Result<Vec<Record>, ApplyError> {
    let value: Value =
        serde_json::from_slice(ctx.bytes).map_err(|e| ApplyError::Extraction {
            mode: "json_path",
            reason: format!("bytes did not parse as JSON: {e}"),
        })?;

    // Collect into an owned Vec<Value> so the per-match scopes
    // outlive the query iterator. jsonpath-rust returns `Vec<&Value>`
    // and we don't strictly need the clone — but cloning per scope
    // is cheaper than juggling lifetimes against the per-binding
    // closures below, and JSON listings are bounded by the cap.
    let scope_refs: Vec<&Value> =
        value.query(iter_path).map_err(|e| ApplyError::Extraction {
            mode: "json_path",
            reason: format!("iterator path query failed: {e}"),
        })?;

    if scope_refs.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!(
                "iterator path {iter_path:?} matched no nodes (the source \
                 may have changed shape, or the path is targeting the \
                 wrong array)"
            ),
        });
    }
    if scope_refs.len() > MAX_RECORDS_PER_RECIPE {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!(
                "iterator path {iter_path:?} matched {} elements; cap is \
                 {} (ADR 0016 §Consequences). Likely cause: the iterator \
                 path matches too broadly (every value rather than every \
                 row), or the source is a paginated API whose first page \
                 already exceeds the cap. Session 68: OData-shaped URLs \
                 (presence of $select|$filter|$orderby, or an \
                 /api/open/vN/ path) are auto-capped at fetch time; if \
                 you're still seeing this, the URL didn't match those \
                 shapes and the recipe's iterator/URL needs an explicit \
                 narrowing (a JsonPath filter expression, or a more \
                 specific URL).",
                scope_refs.len(),
                MAX_RECORDS_PER_RECIPE,
            ),
        });
    }

    let mut records = Vec::with_capacity(scope_refs.len() * ctx.recipe.produces.len());
    for &scope in scope_refs.iter() {
        // Per-element single-leaf extraction (the legacy path) — used
        // by bindings whose FieldMaps are `Extracted`. Multi-leaf
        // bindings ignore this value (validator mutual-exclusion
        // rule (ii)).
        let extracted = extract_json_within(scope, inner_path)?;

        for (idx, binding) in ctx.recipe.produces.iter().enumerate() {
            let inner_extractions = compute_inner_extractions_json(scope, binding)
                .map_err(|e| match e {
                    ApplyError::Extraction { mode, reason } => ApplyError::Extraction {
                        mode,
                        reason: format!("binding[{idx}] ExtractedInner: {reason}"),
                    },
                    other => other,
                })?;
            let dedup_key = compute_dedup_key(
                binding,
                &extracted,
                inner_extractions.as_ref(),
                ctx.recipe,
            )?;
            let record = build_record(
                binding,
                idx,
                &extracted,
                inner_extractions.as_ref(),
                &ctx,
                Some(dedup_key),
            )
            .and_then(|r| crate::normalize::finalize(r, ctx.plan, ctx.recipe))?;
            records.push(record);
        }
    }

    Ok(records)
}

/// ADR 0019 Phase 2A: evaluate a JSONPath sub-spec against a scoped
/// JSON value (one match of the outer query). Mirrors
/// [`extract_json_path`] for the per-match scope: returns the first
/// non-null result as a String, preserving the JSON natural
/// representation (strings unquoted; numbers / bools / objects keep
/// their JSON form).
fn extract_json_within(scope: &Value, path: &str) -> Result<String, ApplyError> {
    let nodes: Vec<&Value> = scope.query(path).map_err(|e| ApplyError::Extraction {
        mode: "json_path",
        reason: format!("inner path query failed: {e}"),
    })?;

    if nodes.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!("inner path {path:?} matched no nodes within scope"),
        });
    }

    let first_non_null = nodes.iter().find(|n| !matches!(n, Value::Null));
    let first = match first_non_null {
        Some(n) => *n,
        None => {
            return Err(ApplyError::Extraction {
                mode: "json_path",
                reason: format!(
                    "inner path {path:?} matched {} node(s), all JSON null",
                    nodes.len()
                ),
            });
        }
    };

    let out = match first {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    bound_extracted(out, "json_path")
}

/// Compute a per-record dedup_key for an iterator-produced record.
///
/// Shape: `{recipe.id}:{field_value}`. The recipe id is stable
/// across re-fetches (within one version) — re-authoring produces a
/// new recipe row, which is the boundary the version chain handles
/// (ADR 0012). The field value is the per-card identifier the
/// iterator-author named via `dedup_key_field` (the headline, the
/// article URL, the paper id).
///
/// Bounded length: the field value is truncated at 200 chars to
/// keep dedup keys index-friendly. ADR 0016 §"Per-match dedup
/// becomes load-bearing" notes this contract.
///
/// Resolution rule for `dedup_key_field`:
///   - If the named path's `FieldValueSource` is `Extracted`, the
///     dedup-key value is the extracted scalar.
///   - If `Literal`, the value is the literal (rare but legal —
///     a constant-keyed binding).
///   - If `FromPlan`, the value is the plan-derived string.
///
/// The validator already guaranteed `dedup_key_field` is `Some` and
/// references an existing `field_mappings.path` for iterator
/// recipes; the `expect`/`unreachable` here documents that
/// invariant for the next reader.
fn compute_dedup_key(
    binding: &ProductionBinding,
    extracted: &str,
    inner_extractions: Option<&HashMap<String, String>>,
    recipe: &FetchRecipe,
) -> Result<String, ApplyError> {
    let field_path = binding.dedup_key_field.as_deref().ok_or_else(|| {
        // Belt-and-braces: the validator should have rejected this
        // recipe at authoring time. If it didn't, surface a
        // concrete error rather than panicking — the operator's
        // re-author path is the right next step.
        ApplyError::FieldMapping {
            reason: "iterator recipe missing dedup_key_field (validator \
                     should have rejected this; the row may be a hand-edit \
                     or pre-validation legacy)"
                .into(),
        }
    })?;

    let fm = binding
        .field_mappings
        .iter()
        .find(|fm| fm.path == field_path)
        .ok_or_else(|| ApplyError::FieldMapping {
            reason: format!(
                "dedup_key_field {field_path:?} does not match any \
                 field_mappings path; the validator should have rejected \
                 this at authoring time"
            ),
        })?;

    let raw = match &fm.source {
        FieldValueSource::Extracted => extracted.to_string(),
        FieldValueSource::Literal { value } => match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        FieldValueSource::FromPlan { pointer: _ } => {
            // Plan-derived dedup keys are legal but unusual (the
            // value is constant per binding, which collapses N
            // records to 1 distinct dedup_key per binding). We
            // resolve via a placeholder that callers can recognise;
            // a real plan-derived dedup key would also be available
            // through `resolve_field_value`, but threading the plan
            // here would expand the function signature for a code
            // path no realistic recipe takes. Document the boundary.
            return Err(ApplyError::FieldMapping {
                reason: format!(
                    "dedup_key_field {field_path:?} uses FromPlan source; \
                     iterator recipes need per-record dedup, and a plan-\
                     derived field is constant across records. Author the \
                     recipe with `extracted` for the dedup_key_field's \
                     source, or pick a different path."
                ),
            });
        }
        FieldValueSource::ExtractedInner { spec: _ } => {
            // ADR 0019 Phase 2A. Multi-leaf records typically name an
            // ExtractedInner FieldMap as their dedup_key source (the
            // storm name, the article URL, the arXiv id). Look up the
            // pre-computed per-match leaf in the inner-extractions
            // map. Missing map / missing entry is a runtime bug —
            // same diagnostics as `resolve_field_value`.
            let map = inner_extractions.ok_or_else(|| ApplyError::FieldMapping {
                reason: format!(
                    "dedup_key_field {field_path:?} resolves to ExtractedInner \
                     but the apply path did not thread an inner-extractions \
                     map. ADR 0019 Phase 2A: iterator + multi-field flows \
                     must compute inner extractions before dispatch."
                ),
            })?;
            map.get(field_path)
                .cloned()
                .ok_or_else(|| ApplyError::FieldMapping {
                    reason: format!(
                        "dedup_key_field {field_path:?} resolves to ExtractedInner \
                         but produced no leaf for this match."
                    ),
                })?
        }
    };

    // Bound the field-value portion. 200 chars is generous for a
    // headline / URL / id and small enough to keep the dedup_key
    // column index-friendly. UTF-8-safe truncation.
    const FIELD_VALUE_BUDGET: usize = 200;
    let trimmed: String = raw.chars().take(FIELD_VALUE_BUDGET).collect();

    Ok(format!("{}:{}", recipe.id, trimmed))
}

/// Extract a single scalar from an `ElementRef`'s sub-tree by
/// running an inner CSS selector. Mirrors the scalar
/// `extract_css_select` shape (text-or-attribute, empty-string
/// rejection, scalar-size cap) but evaluates against the matched
/// node rather than the whole document.
///
/// Returns the extracted scalar verbatim; the bound check via
/// [`bound_extracted`] catches selectors that accidentally match a
/// container instead of a leaf, the same way the scalar path does.
fn extract_css_within(
    matched: scraper::ElementRef<'_>,
    inner_sel: &Selector,
    attribute: Option<&str>,
) -> Result<String, ApplyError> {
    let first = matched.select(inner_sel).next().ok_or_else(|| {
        ApplyError::Extraction {
            mode: "css_select",
            reason: "inner selector matched no elements within iterator \
                     match (the iterator's selector matched a card, but \
                     the inner selector found nothing inside it). Likely \
                     cause: the inner selector is targeted at a sibling \
                     rather than a descendant of the iterator's match."
                .into(),
        }
    })?;

    let out = match attribute {
        Some(attr_name) => first
            .value()
            .attr(attr_name)
            .ok_or_else(|| ApplyError::Extraction {
                mode: "css_select",
                reason: format!(
                    "iterator-matched element has no attribute {attr_name:?}"
                ),
            })?
            .to_string(),
        None => first.text().collect::<String>().trim().to_string(),
    };

    if out.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: "inner selection within iterator match resolved to \
                     empty string"
                .into(),
        });
    }
    bound_extracted(out, "css_select")
}

// ---------------------------------------------------------------------------
// Extraction: one function per ExtractionSpec variant
// ---------------------------------------------------------------------------

/// Dispatch to the per-mode extractor.
///
/// Each extractor is a pure function from bytes → string. No stage
/// above the extractor mutates or re-interprets the extracted string
/// — it flows directly into the binding stage.
///
/// **Visibility**: `pub(crate)` so [`validate_recipe_against_bytes`]
/// can reuse the same dispatch path the runtime uses at apply time.
/// Session 41 items 4–6: by construction, what the validator runs
/// is what the runtime will run.
pub(crate) fn extract(spec: &ExtractionSpec, bytes: &[u8]) -> Result<String, ApplyError> {
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

    if nodes.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!("path {path:?} matched no nodes"),
        });
    }

    // Session 32b: skip JSON `null` matches when picking the
    // first-of-many. Live evidence from the World Bank Open Data
    // API (`api.worldbank.org/v2/country/.../indicator/...`): the
    // most-recent rows in any indicator series carry `"value":
    // null` for years where data hasn't been published yet, so a
    // path like `$[1][*].value` legitimately matches a sequence of
    // nulls before the first real value. The previous code stringified
    // the leading null to the literal four-character string `"null"`,
    // which then failed downstream content assembly with
    // `invalid type: string "null", expected f64` (Session 32a live
    // run, hungarian barley production).
    //
    // The new behaviour: if the first non-null match exists, return
    // it. If every matched node is null, return a clear error that
    // names the pattern and suggests the standard fix (a JSONPath
    // filter expression that excludes nulls). This preserves the
    // first-match contract for non-null sources unchanged.
    //
    // We do NOT do the same for empty strings, zeros, or other
    // "falsy" values — those are real data and the source is
    // entitled to publish them. Only JSON `null` is treated as
    // "this slot has no value here, look further down."
    let first_non_null = nodes.iter().find(|n| !matches!(n, Value::Null));
    let first = match first_non_null {
        Some(n) => *n,
        None => {
            return Err(ApplyError::Extraction {
                mode: "json_path",
                reason: format!(
                    "path {path:?} matched {} node(s), all JSON null. \
                     The source publishes nulls for unavailable data; \
                     refine the path with a filter expression \
                     (e.g. `$[1][?(@.value)].value`) to skip nulls \
                     and select real values.",
                    nodes.len()
                ),
            });
        }
    };

    // Preserve the value's natural JSON representation. Strings come
    // out unquoted; numbers, bools, objects keep their JSON form.
    let out = match first {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    bound_extracted(out, "json_path")
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
    bound_extracted(out, "css_select")
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
        1 => bound_extracted(matching.into_iter().next().unwrap(), "csv_cell"),
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
    bound_extracted(m.as_str().to_string(), "regex_capture")
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
    bound_extracted(trimmed.to_string(), "pdf_table")
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
///
/// **Visibility**: `pub(crate)` so the executor's PDF prefetch path
/// can frame what the LLM sees in the runtime's coordinate space.
/// Session 41 item 1: the recipe-author needs to count rows the way
/// the runtime counts them. Same library, same detector, same
/// coordinates — by construction, no off-by-one between authoring
/// and apply.
pub(crate) fn detect_pdf_tables(page_text: &str) -> Vec<Vec<Vec<String>>> {
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

/// Session 87: render the recipe's outer extraction as a closed-vocabulary
/// selector_path string. Format:
///
///   - `"css:#price"` (CssSelect, no attribute)
///   - `"css:#price[data-v]"` (CssSelect with attribute)
///   - `"json:$.close"` (JsonPath)
///   - `"csv:close@row=3"` (CsvCell + Equals filter)
///   - `"csv:close@label=Asia"` (CsvCell + LabeledAs filter)
///   - `"csv:close"` (CsvCell, no filter — single-row source)
///   - `"pdf:p1/t0/r2/c3"` (PdfTable)
///   - `"regex:group=1"` (RegexCapture)
///
/// Centralised so storage/DTO/frontend can match against a stable
/// shape. Used by `build_record` to stamp `Provenance::selector_path`.
fn render_selector_path(spec: &ExtractionSpec) -> String {
    match spec {
        ExtractionSpec::CssSelect { selector, attribute } => match attribute {
            Some(a) => format!("css:{selector}[{a}]"),
            None => format!("css:{selector}"),
        },
        ExtractionSpec::JsonPath { path } => format!("json:{path}"),
        ExtractionSpec::CsvCell { column, row_filter } => match row_filter {
            None => format!("csv:{column}"),
            Some(RowFilter::Equals { column: c, value }) => {
                format!("csv:{column}@{c}={value}")
            }
            Some(RowFilter::LabeledAs { label_column, label }) => {
                format!("csv:{column}@{label_column}={label}")
            }
        },
        ExtractionSpec::PdfTable { page, table_index, row, col } => {
            format!("pdf:p{page}/t{table_index}/r{row}/c{col}")
        }
        ExtractionSpec::RegexCapture { group, .. } => format!("regex:group={group}"),
    }
}

/// Session 87: codepoint-cap an excerpt of the leaf bytes for the
/// `raw_bytes_excerpt` Provenance field. Truncation appends `"…"` so
/// the operator-facing display marks where the excerpt stops. The cap
/// itself lives in core (`RAW_BYTES_EXCERPT_CAP`).
fn truncate_excerpt(s: &str) -> String {
    let cap = situation_room_core::schema::envelope::RAW_BYTES_EXCERPT_CAP;
    // count codepoints; `chars().count()` is O(n) but n ≤ extracted leaf
    // length which is bounded by the per-fetch byte cap upstream.
    let cp_count = s.chars().count();
    if cp_count <= cap {
        s.to_string()
    } else {
        let head: String = s.chars().take(cap).collect();
        format!("{head}…")
    }
}

fn build_record(
    binding: &ProductionBinding,
    index: usize,
    extracted: &str,
    inner_extractions: Option<&HashMap<String, String>>,
    ctx: &ApplyContext<'_>,
    dedup_key: Option<String>,
) -> Result<Record, ApplyError> {
    // Build a JSON object from the field mappings, then deserialize
    // into the concrete content type. This keeps the record types
    // authoritative about their own shape — we don't hand-write
    // ObservationContent assembly.
    let mut content_json: Map<String, Value> = Map::new();
    for fm in &binding.field_mappings {
        let mut value = resolve_field_value(fm, extracted, ctx.plan, inner_extractions)
            .map_err(|e| ApplyError::Binding {
                index,
                reason: e.to_string(),
            })?;
        // Session 64: `parse_extracted_scalar` is type-blind and
        // promotes any numeric-looking leaf to `Value::Number`. When
        // the FieldMap's target path resolves to a JSON String at the
        // schema level (`event.headline`, `observation.metric`, …) we
        // stringify the Number here so the assembled JSON lines up
        // with serde's expectation. Same-plan volatility on
        // federalreserve.gov surfaced this — a recipe that succeeded
        // when the leaf rendered as a date string began failing with
        // `invalid type: floating point '22.0', expected a string`
        // after the source's leaf drifted to a numeric-looking shape.
        coerce_for_string_path(&mut value, binding.record_type, &fm.path);
        insert_at_path(&mut content_json, &fm.path, value).map_err(|e| {
            ApplyError::Binding {
                index,
                reason: e.to_string(),
            }
        })?;
    }

    // Session 87: render the per-record selector_path. Iterator-mode
    // recipes prefix `"<iter> >> "` so the operator can see both
    // selectors in one string (`ctx.recipe.iterator` carries the iter
    // spec; the inner spec is `ctx.recipe.extraction`). For scalar
    // recipes the iter half is absent.
    let inner_selector = render_selector_path(&ctx.recipe.extraction);
    let selector_path = match ctx.recipe.iterator.as_ref() {
        Some(iter) => Some(format!("{} >> {}", render_selector_path(iter), inner_selector)),
        None => Some(inner_selector),
    };
    // Session 87: stamp the leaf bytes excerpt. For multi-leaf bindings
    // (ADR 0019 Phase 2A) `extracted` is unused at the FieldMap level
    // but still carries the *outer* extraction's scalar; for the
    // operator-facing diagnostic this is the right scope — they want
    // to see what the recipe matched, which is the outer scope in
    // both scalar and iterator modes.
    let raw_bytes_excerpt = Some(truncate_excerpt(extracted));

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
        selector_path,
        raw_bytes_excerpt,
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

    // ADR 0016: `dedup_key` is `Some` for iterator-produced records
    // (computed from the binding's `dedup_key_field`) and `None` for
    // scalar recipes (today's contract). The value lands on every
    // record type's `dedup_key: Option<String>` field; storage's
    // upsert logic uses it for iterator records and treats NULL as
    // "no idempotency key" for scalar records, exactly as today.
    let record = match binding.record_type {
        RecordType::Observation => {
            let content: ObservationContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(format!("observation content: {e}")),
                })?;
            Record::Observation(Observation {
                id: Uuid::now_v7(),
                dedup_key: dedup_key.clone(),
                envelope,
                content,
            })
        }
        RecordType::Event => {
            let content: EventContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(format!("event content: {e}")),
                })?;
            Record::Event(Event {
                id: Uuid::now_v7(),
                dedup_key: dedup_key.clone(),
                envelope,
                content,
            })
        }
        RecordType::Relation => {
            let content: RelationContent = serde_json::from_value(content_value)
                .map_err(|e| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(format!("relation content: {e}")),
                })?;
            Record::Relation(Relation {
                id: Uuid::now_v7(),
                dedup_key,
                envelope,
                content,
            })
        }
        RecordType::Entity => {
            // Session 97 Lever B — open `entity` to recipe-driven
            // production. Iterator-bearing recipes against
            // `entity_kind` expectations can now emit Entity rows
            // directly (the "324 bulls from one fetch" pattern from
            // Sn-96's PBR baseline). Plan-accept-time exemplar
            // materialisation (Sn-76 `entity_synth`) is unchanged;
            // both paths converge on storage via
            // `Store::upsert_entity` (idempotent on entity_id).
            //
            // Entity is a flat-fields record (no `content` JSON
            // column at storage), so we hand-extract the three
            // required strings from the assembled content_json
            // rather than calling `serde_json::from_value` into an
            // EntityContent type (which doesn't exist — see
            // `crates/core/src/schema/records/entity.rs`).
            let map = match content_value {
                Value::Object(m) => m,
                _ => {
                    return Err(ApplyError::ContentAssembly {
                        reason: truncate_content_assembly_reason(
                            "entity content: expected JSON object".to_string(),
                        ),
                    });
                }
            };
            let entity_id_s = map
                .get("entity_id")
                .and_then(Value::as_str)
                .ok_or_else(|| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(
                        "entity content: missing or non-string `entity_id`".to_string(),
                    ),
                })?;
            let entity_id = EntityId::new(entity_id_s).map_err(|e| {
                ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(format!(
                        "entity content: invalid entity_id `{entity_id_s}`: {e}"
                    )),
                }
            })?;
            let kind = map
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(
                        "entity content: missing or non-string `kind`".to_string(),
                    ),
                })?
                .to_string();
            let canonical_name = map
                .get("canonical_name")
                .and_then(Value::as_str)
                .ok_or_else(|| ApplyError::ContentAssembly {
                    reason: truncate_content_assembly_reason(
                        "entity content: missing or non-string `canonical_name`"
                            .to_string(),
                    ),
                })?
                .to_string();
            Record::Entity(Entity {
                id: Uuid::now_v7(),
                entity_id,
                kind,
                canonical_name,
                geometry: None,
                envelope,
            })
        }
        RecordType::Document | RecordType::Assertion => {
            return Err(ApplyError::Binding {
                index,
                reason: format!(
                    "record_type {:?} is not producible from a recipe. \
                     Documents come from ingest (per-fetch Document synthesis, \
                     Session 69). Assertions come from the LLM extraction \
                     layer (they carry a claimant and stance that recipe \
                     field-mappings don't populate). See ADR 0007 and ADR 0004.",
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
///
/// `inner_extractions` carries the per-FieldMap inner-extracted leaves
/// for `ExtractedInner` sources (ADR 0019 Phase 2A). The map is keyed
/// by FieldMap path. `None` indicates the caller is on the legacy
/// single-scalar path (no binding in the recipe uses ExtractedInner),
/// in which case an ExtractedInner FieldMap is an error: the validator
/// should have rejected such a recipe before it reached apply.
fn resolve_field_value(
    fm: &FieldMap,
    extracted: &str,
    plan: &ResearchPlan,
    inner_extractions: Option<&HashMap<String, String>>,
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
        FieldValueSource::ExtractedInner { spec: _ } => {
            // ADR 0019 Phase 2A. The inner-extraction map is computed
            // upstream once per (binding, match) pair and threaded
            // through. The map lookup uses the FieldMap's `path` —
            // the same handle the validator's mutual-exclusion check
            // operates on. Missing entries are a runtime bug
            // (compute_inner_extractions failed to populate, or the
            // caller forgot to thread the map) and surface as
            // FieldMapping errors.
            let map = inner_extractions.ok_or_else(|| ApplyError::FieldMapping {
                reason: format!(
                    "FieldMap {:?} has ExtractedInner source but the apply \
                     path did not thread an inner-extractions map. This is \
                     a runtime bug; the validator should have ensured \
                     either the iterator or scalar+multi-field code path \
                     ran (ADR 0019 Phase 2A).",
                    fm.path
                ),
            })?;
            let leaf = map.get(&fm.path).ok_or_else(|| ApplyError::FieldMapping {
                reason: format!(
                    "ExtractedInner FieldMap {:?} produced no leaf for this \
                     match. The inner sub-spec ran but did not populate the \
                     per-match map — this is a runtime bug.",
                    fm.path
                ),
            })?;
            Ok(parse_extracted_scalar(leaf))
        }
    }
}

/// Parse an extracted string as a JSON scalar, preferring numbers.
///
/// Strategy:
/// 1. Trim surrounding whitespace.
/// 2. Try direct `f64::parse` — handles `"3.14"`, `"42"`, `"1.5e9"`
///    (scientific notation parses verbatim).
/// 3. If direct parse fails, apply the bounded numeric-format
///    normalizer ([`normalize_numeric_candidate`]) which strips
///    estimate prefixes (`e ` / `~` / `≈` / `est. `), currency
///    markers (`$`, `€`, `£`, `¥`, `USD`, `EUR`), and ASCII
///    thousand-separator commas in the canonical US-locale
///    position. EU-locale shapes (`"1.234,56"`) are detected and
///    returned untouched — they parse wrong if normalised, so we
///    fail honestly and let apply surface the original string.
/// 4. If the normalizer returns a value, return `Value::Number`.
///    Otherwise return the original trimmed string as
///    `Value::String`.
///
/// The normalizer is **bounded**: it doesn't try locale detection
/// (US `1,234.5` vs EU `1.234,5`), doesn't attempt currency
/// conversion, doesn't infer units. Edge cases stay in the
/// "apply fails honestly" path so the operator sees the real
/// shape, not a misleading post-strip fragment.
///
/// Session 53 Piece D — broadens what apply will accept from
/// formats the recipe-author reasonably authors against
/// human-readable tables (USGS MCS lithium chapter's `74,700`
/// shape, IEA fact sheets' `est. 1,200`-style estimates, FT/
/// Bloomberg headline-style `$1,234`).
fn parse_extracted_scalar(s: &str) -> Value {
    let trimmed = s.trim();

    // Try direct number parse first — handles `"3.14"`, `"42"`,
    // `"1.5e9"`. Scientific notation is preserved here because
    // Rust's f64 parser handles `e<digit>` as the exponent
    // separator.
    if let Ok(n) = trimmed.parse::<f64>() {
        if let Some(v) = serde_json::Number::from_f64(n).map(Value::Number) {
            return v;
        }
    }

    // Direct parse failed. Apply the bounded normalizer.
    if let Some(n) = normalize_numeric_candidate(trimmed) {
        if let Some(v) = serde_json::Number::from_f64(n).map(Value::Number) {
            debug!(
                input = %trimmed,
                parsed = %n,
                "numeric normalizer accepted formatted scalar (Session 53 Piece D)"
            );
            return v;
        }
    }

    Value::String(trimmed.to_string())
}

/// Bounded pre-coercion normalizer for human-readable numeric
/// strings. Returns `Some(f64)` when the candidate parses cleanly
/// after normalization; `None` when the candidate doesn't fit the
/// supported shapes (genuinely non-numeric, or an
/// ambiguous-locale shape we refuse to guess on).
///
/// Strip order (each step is independent and any can fire):
/// 1. **EU-locale gate** — if both `.` and `,` are present and
///    the last `,` appears after the last `.`, the string is
///    ambiguously EU-shaped (`1.234,56`). Return `None` and let
///    apply fail honestly. Adding heuristic locale detection
///    would silently mis-parse legitimate values; explicit is
///    better than guessing. (Documented in Session 53 handoff
///    "What's intentionally not in this patch".)
/// 2. **Estimate prefixes** — `est. `, `est `, `~`, `≈`, and a
///    careful `e `/`e` matcher that distinguishes from
///    scientific notation. Common in agency tables (`est. 1,200`).
/// 3. **Currency markers** — `$`, `€`, `£`, `¥` (single-char
///    prefixes/suffixes); ASCII codes `USD`, `EUR` (case-
///    insensitive). The strip is positional: a leading currency
///    marker is removed; a trailing one too.
/// 4. **Whitespace** inside the body (`"1 234.5"` → `"1234.5"`).
/// 5. **ASCII thousand-separator commas** — only when every
///    comma sits between digit triplets in the integer portion.
///    Malformed positions (`"1,23"`, `"abc,def"`) leave the
///    whole string alone; the parse fails honestly.
/// 6. **Trailing-unit fallback** — if the post-strip whole
///    string doesn't parse, take the leading numeric prefix
///    (digits, decimal point, sign) and try to parse that.
///    Preserves the pre-Session-53 contract for `"49000 t"`
///    and `"12.5%"`-shaped values where the recipe-author
///    selected a cell whose unit suffix tagged along.
fn normalize_numeric_candidate(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Step 1: strip estimate prefixes from the start.
    //
    // Done BEFORE the EU-locale gate (Step 2) because the prefix
    // `"est. "` contains a literal period; if the gate ran on the
    // un-stripped string, an input like `"est. 1,200"` would see
    // a `.` at index 3 and a `,` at index 6 — c > p — and the
    // gate would false-positive on the prefix's punctuation rather
    // than on the numeric body's locale, returning `None` for a
    // string the agency-table convention expects to normalise to
    // `1200`. Stripping first lets the gate see the numeric body
    // alone and reach the correct verdict (no `.` in `"1,200"`,
    // gate doesn't fire). Genuine EU-locale inputs such as
    // `"est. 1.234,56"` still decline cleanly: the prefix is
    // stripped to `"1.234,56"`, then the gate fires on the
    // numeric body's `c > p` signal.
    let mut working: String = trimmed.to_string();
    for prefix in ["est. ", "est ", "~", "≈"] {
        if let Some(rest) = working.strip_prefix(prefix) {
            working = rest.trim_start().to_string();
        }
    }
    // The bare-`e` estimate prefix is delicate: it must not eat
    // scientific notation mantissas. We never strip a leading
    // `e<digit>` because that's the start of an exponent in
    // shapes like `e9` (which `f64::parse` accepts as part of a
    // larger mantissa-bearing literal). The strip fires only on
    // `e ` (literal space before the digit, the agency-table
    // convention) and on `e<digit>` *when* the rest also
    // contains a comma — at which point it can't be scientific
    // notation anyway because Rust's f64 parser rejects commas
    // inside literals.
    if let Some(rest) = working.strip_prefix("e ") {
        if rest.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            working = rest.trim_start().to_string();
        }
    } else if let Some(rest) = working.strip_prefix('e') {
        if rest.contains(',')
            && rest.chars().next().map_or(false, |c| c.is_ascii_digit())
        {
            working = rest.to_string();
        }
    }

    // Step 2: EU-locale gate. `1.234,56`-shaped strings are
    // ambiguous: US-style would treat the period as decimal,
    // EU-style would treat the comma as decimal. We refuse to
    // guess. The signal: both `.` and `,` present, and the
    // LAST `,` is to the RIGHT of the LAST `.`. Run against the
    // post-prefix-strip `working`, not the original `trimmed`,
    // for the reason given in Step 1.
    let last_comma = working.rfind(',');
    let last_period = working.rfind('.');
    if let (Some(c), Some(p)) = (last_comma, last_period) {
        if c > p {
            return None;
        }
    }

    // Step 3: strip currency markers, leading or trailing.
    for sym in ['$', '€', '£', '¥'] {
        if working.starts_with(sym) {
            let rest = working[sym.len_utf8()..].trim_start();
            working = rest.to_string();
        }
        if working.ends_with(sym) {
            let cut = working.len() - sym.len_utf8();
            let rest = working[..cut].trim_end();
            working = rest.to_string();
        }
    }
    for code in ["USD", "EUR", "usd", "eur"] {
        if working.starts_with(code) {
            let rest = working[code.len()..].trim_start();
            working = rest.to_string();
        }
        if working.ends_with(code) {
            let cut = working.len() - code.len();
            let rest = working[..cut].trim_end();
            working = rest.to_string();
        }
    }

    // Step 4: collapse internal whitespace (e.g. `1 234.5` →
    // `1234.5`). Lossless for canonical numeric shapes.
    let no_internal_ws: String = working
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();

    // Step 5: validate-and-strip ASCII thousand-separator
    // commas. Failed validation returns the input unchanged
    // (the parser will reject it below); a malformed comma
    // position should never produce a silently normalised
    // value.
    let stripped = strip_thousand_separator_commas(&no_internal_ws)
        .unwrap_or_else(|| no_internal_ws.clone());

    // First parse attempt: the whole post-strip string.
    if let Ok(n) = stripped.parse::<f64>() {
        return Some(n);
    }

    // Step 6: trailing-unit fallback. Take the longest leading
    // numeric prefix (digits, decimal point, sign) and try to
    // parse that. Preserves the pre-Session-53 behaviour for
    // recipe-authors who selected a cell whose unit tagged
    // along (`"49000 t"`, `"12.5%"`).
    let prefix: String = stripped
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    if prefix.is_empty() {
        return None;
    }
    prefix.parse::<f64>().ok()
}

/// Return `Some(stripped)` when every comma in the candidate sits
/// between digit triplets (canonical thousands form), or there are
/// no commas. Return `None` otherwise — the caller treats this as
/// "leave the original string alone, let apply fail honestly."
///
/// Examples that pass: `"74,700"` → `Some("74700")`,
/// `"1,234,567"` → `Some("1234567")`, `"42"` → `Some("42")`,
/// `"1234.56"` → `Some("1234.56")`, `"-3,200.5"` → `Some("-3200.5")`.
///
/// Examples that fail (return `None`): `"1,23"` (decimal-mark
/// pattern, ambiguous locale), `"abc,def"` (non-numeric).
fn strip_thousand_separator_commas(s: &str) -> Option<String> {
    if !s.contains(',') {
        return Some(s.to_string());
    }
    // Split on the first non-digit-non-comma-non-period-non-minus-
    // non-plus character, treating that as the boundary between
    // the numeric body and a possible suffix. The body is what
    // we validate; the suffix (if any) is preserved as-is for
    // downstream (it'll fail to parse if it's there).
    let body_end = s
        .char_indices()
        .find(|(_, c)| {
            !(c.is_ascii_digit() || *c == ',' || *c == '.' || *c == '-' || *c == '+')
        })
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let body = &s[..body_end];
    let suffix = &s[body_end..];

    // Sign? Hold it aside.
    let (sign, body) = if let Some(rest) = body.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = body.strip_prefix('+') {
        ("", rest)
    } else {
        ("", body)
    };

    // Split off the decimal portion (everything after the LAST
    // period). Commas only matter in the integer part.
    let (int_part, frac_part) = match body.rfind('.') {
        Some(idx) => (&body[..idx], &body[idx..]),
        None => (body, ""),
    };

    // Validate the comma positions in `int_part`: must be
    // 1..=3 digits, then groups of `,DDD`.
    let has_comma = int_part.contains(',');
    let int_clean = if has_comma {
        let groups: Vec<&str> = int_part.split(',').collect();
        // First group: 1..=3 digits, all ascii_digit.
        if groups[0].is_empty() || groups[0].len() > 3 {
            return None;
        }
        if !groups[0].chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // Subsequent groups: exactly 3 digits.
        for g in &groups[1..] {
            if g.len() != 3 || !g.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
        }
        groups.concat()
    } else {
        int_part.to_string()
    };

    Some(format!("{sign}{int_clean}{frac_part}{suffix}"))
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
// Authoring-time validation against prefetched bytes (Session 41 items 4–6)
// ---------------------------------------------------------------------------
//
// Architectural rationale (ADR 0007 golden rule applied to the
// authoring loop). Pre-Session-41, the LLM authored a recipe, the
// validator (`build_validated_recipe`) checked structural shape, and
// the recipe was persisted. The runtime then attempted to apply it
// against fetched bytes on every fetch — and an apply failure
// recurred forever because the recipe was already on disk.
//
// Item 4 (css_select), item 5 (pdf_table), item 6 (json_path) close
// this gap by running the runtime's own extraction code against the
// *same bytes the LLM saw* immediately after authoring. By
// construction, no recipe gets persisted that the runtime would fail
// at apply, because the validator IS the runtime.
//
// What this is NOT:
// - A mode-specific validator. The dispatch goes through [`extract`]
//   (the same function the runtime calls at apply time) so adding a
//   new extraction mode under ADR 0007 does not require touching this
//   function — the closed enum's `match` is in `extract`, not here.
// - A re-fetch. Validation runs against the bytes the caller already
//   prefetched. No second network round-trip.
// - A best-effort heuristic. A failure here is a hard decline; the
//   recipe is not persisted.

/// Validate that a candidate [`FetchRecipe`] would succeed at apply
/// time against the given bytes — the same bytes the LLM saw when
/// authoring the recipe.
///
/// Scalar recipes: dispatch through [`extract`] and discard the
/// extracted string. Any [`ApplyError`] returned by the runtime
/// extractor is the authoring-time decline reason.
///
/// Iterator recipes: the runtime supports `css_select × css_select`
/// (ADR 0016 Phase 1) and `json_path × json_path` (ADR 0019 Phase 2A).
/// We mirror that contract here — any other pairing surfaces the same
/// `ApplyError::NotImplemented` the runtime would produce, but at
/// authoring time so the recipe is never persisted. For each
/// supported pair, the validator requires the outer iterator to match
/// ≥1 node AND the inner extraction to match ≥1 node within at least
/// one of those outer matches. This is the minimum "the runtime would
/// produce records" contract; a recipe that fails this preflight
/// would fail at apply.
///
/// Session 41 items 4–6 added the css branch; Session 67 added the
/// json_path branch (closing the authoring-time gate that prior
/// sessions left around the existing Phase-2A runtime).
pub(crate) fn validate_recipe_against_bytes(
    recipe: &FetchRecipe,
    bytes: &[u8],
) -> Result<(), ApplyError> {
    match &recipe.iterator {
        None => {
            // Scalar: same dispatch the runtime uses. Discard the
            // returned scalar — we only care that extraction
            // succeeded structurally.
            extract(&recipe.extraction, bytes).map(|_| ())
        }
        Some(iter_spec) => match (iter_spec, &recipe.extraction) {
            (
                ExtractionSpec::CssSelect {
                    selector: iter_selector,
                    attribute: _,
                },
                ExtractionSpec::CssSelect {
                    selector: inner_selector,
                    attribute: _,
                },
            ) => validate_css_iterator(bytes, iter_selector, inner_selector),
            (
                ExtractionSpec::JsonPath { path: iter_path },
                ExtractionSpec::JsonPath { path: inner_path },
            ) => validate_json_iterator(bytes, iter_path, inner_path),
            (iter_other, inner_other) => {
                // Mirror the runtime's NotImplemented; the validator
                // should never disagree with the runtime about which
                // pairings are supported. Keep this message aligned
                // with `apply_iterator`'s fallthrough — operators
                // reading either failure should see the same boundary.
                Err(ApplyError::NotImplemented {
                    mode: "iterator",
                    reason: format!(
                        "iterator runtime is wired for css_select × css_select \
                         (ADR 0016 Phase 1) and json_path × json_path (ADR 0019 \
                         Phase 2A). Got iter={}, inner={}. Other modes \
                         (csv_cell, pdf_table, regex_capture) are tracked for \
                         Phase 2B; until then, iterator-bearing recipes against \
                         listing-shaped sources of those modes should decline \
                         at authoring.",
                        mode_name(iter_other),
                        mode_name(inner_other),
                    ),
                })
            }
        },
    }
}

/// Iterator preflight for css_select × css_select. Mirrors the
/// runtime's [`apply_css_iterator`] structurally: parse once, run the
/// outer selector, ensure ≥1 match, then run the inner selector
/// against each outer match's sub-tree until one matches. Differences
/// from `apply_css_iterator`:
/// - No record building (we don't have an `ApplyContext` and don't
///   need normalized records).
/// - No cap check — Phase 1's `MAX_RECORDS_PER_RECIPE` cap is a
///   runtime concern; the validator only asks "would this produce ≥1
///   record." A recipe that yields too many records still gets
///   persisted; the runtime caps it on apply.
/// - Inner-empty / attribute-missing cases are NOT failures here:
///   those are per-record runtime errors, not "the recipe is
///   structurally wrong against this page" errors. The validator
///   defends authoring; the runtime defends apply.
fn validate_css_iterator(
    bytes: &[u8],
    iter_selector: &str,
    inner_selector: &str,
) -> Result<(), ApplyError> {
    let html_str = std::str::from_utf8(bytes).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("bytes were not UTF-8: {e}"),
    })?;
    let doc = Html::parse_document(html_str);

    let iter_sel = Selector::parse(iter_selector).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("iterator selector did not parse: {e}"),
    })?;
    let inner_sel = Selector::parse(inner_selector).map_err(|e| ApplyError::Extraction {
        mode: "css_select",
        reason: format!("inner selector did not parse: {e}"),
    })?;

    let outer_matches: Vec<scraper::ElementRef<'_>> = doc.select(&iter_sel).collect();
    if outer_matches.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: format!(
                "iterator selector {iter_selector:?} matched no elements"
            ),
        });
    }

    // Inner must match within at least one outer scope. We don't
    // require *every* outer match to host an inner match — the
    // runtime tolerates a per-card miss as a per-record extraction
    // error and continues. The minimum authoring contract is "this
    // selector pair would produce at least one record."
    let any_inner_hit = outer_matches
        .iter()
        .any(|m| m.select(&inner_sel).next().is_some());
    if !any_inner_hit {
        return Err(ApplyError::Extraction {
            mode: "css_select",
            reason: format!(
                "inner selector {inner_selector:?} matched no elements within \
                 any of the {} iterator matches — the iterator selector found \
                 cards but none of them contained the value the inner selector \
                 addresses. Likely cause: the inner selector is hallucinated \
                 against markup the prefetch did not return.",
                outer_matches.len()
            ),
        });
    }

    Ok(())
}

/// Iterator preflight for `json_path` × `json_path`. Mirror of
/// [`validate_css_iterator`] for the JSON case. ADR 0019 Phase 2A
/// declared this pairing supported at the runtime ([`apply_json_iterator`])
/// since Session 61; this function closes the corresponding gap in
/// the authoring-time validator so the structural validator no
/// longer disagrees with the runtime about which pairings persist.
///
/// Differences from [`apply_json_iterator`]:
/// - No record building (the validator has no `ApplyContext`).
/// - No cap check — Phase 1's `MAX_RECORDS_PER_RECIPE` cap is a
///   runtime concern; the validator only asks "would this produce
///   ≥1 record." A recipe that yields too many records still gets
///   persisted; the runtime caps it on apply.
/// - All-null inner matches count as misses because the runtime's
///   [`extract_json_within`] rejects them at apply time. Requiring
///   ≥1 non-null inner hit at authoring matches the runtime's
///   "would extraction succeed" contract.
///
/// Predicate strings deliberately re-use the runtime's
/// `apply_json_iterator` text ("iterator path … matched no
/// nodes", "inner path … matched no nodes within …") so the
/// `failure_message` column reads the same whether the recipe
/// failed at authoring or at apply. ADR 0012's strict-Class-B
/// gate matches on these strings; keeping them aligned across
/// validator and runtime preserves that contract.
fn validate_json_iterator(
    bytes: &[u8],
    iter_path: &str,
    inner_path: &str,
) -> Result<(), ApplyError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| ApplyError::Extraction {
        mode: "json_path",
        reason: format!("bytes did not parse as JSON: {e}"),
    })?;

    let outer_matches: Vec<&Value> =
        value.query(iter_path).map_err(|e| ApplyError::Extraction {
            mode: "json_path",
            reason: format!("iterator path query failed: {e}"),
        })?;

    if outer_matches.is_empty() {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!(
                "iterator path {iter_path:?} matched no nodes (the source \
                 may have changed shape, or the path is targeting the \
                 wrong array)"
            ),
        });
    }

    // Inner must produce ≥1 non-null node within at least one outer
    // scope. Same posture as [`validate_css_iterator`]: we don't
    // require every outer match to host a usable inner — the runtime
    // tolerates per-element misses. All-null inner matches count as
    // misses because [`extract_json_within`] rejects all-null at
    // apply time. A query error (e.g. malformed inner path syntax)
    // counts as a miss here; the failure surfaces on the next outer
    // match or, if none match, in the aggregate error below — which
    // names the path, the right diagnostic for the LLM to fix.
    let any_inner_hit = outer_matches.iter().any(|scope| match scope.query(inner_path) {
        Ok(nodes) => nodes.iter().any(|n| !matches!(n, Value::Null)),
        Err(_) => false,
    });
    if !any_inner_hit {
        return Err(ApplyError::Extraction {
            mode: "json_path",
            reason: format!(
                "inner path {inner_path:?} matched no nodes within any of the \
                 {} iterator matches — the iterator path found elements but \
                 none contained a non-null value the inner path addresses. \
                 Likely cause: the inner path is hallucinated against shape \
                 the prefetch did not return.",
                outer_matches.len()
            ),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Authoring-time SHAPE validation against prefetched bytes (Session 53 piece B)
// ---------------------------------------------------------------------------
//
// `validate_recipe_against_bytes` (Session 41 items 4–6) catches the
// case where the LLM authors a selector that does not match the page —
// the runtime's `Extraction` error surfaces at authoring. It does NOT
// catch the case where the selector matches *something* but the
// matched value's shape is wrong for the binding's content type. The
// 2026-05-09 18:12 lithium re-run hit this twice (`pubs.usgs.gov`
// authoring `string "Argentina"` into an `f64` value field;
// `www.worldbank.org` authoring a selector that resolved to no
// `value` field at all). Both apply-failed; both consumed a fetch
// attempt; both polluted the prior-attempts log only as "apply
// failed" rows the proposer doesn't see.
//
// Piece B closes this gap. After the structural validator accepts the
// recipe, we run the *full* apply path (extract → build_record) for
// each binding against the prefetched bytes. A `ContentAssembly`,
// `Binding`, or `FieldMapping` failure here is the same shape the
// runtime would surface at apply time — by construction, the
// validator and the runtime cannot disagree about whether the
// extracted bytes type-check into the binding's content type.
//
// Differences from `apply()`:
// - We do not run `normalize::finalize`. Normalization can reject
//   records on plan-relevance grounds (topic mismatch, geography out
//   of scope) which are real apply-stage failures but unrelated to
//   the wire shape the LLM picked. Surfacing normalization rejects as
//   author-time declines would teach the recipe-author the wrong
//   lesson; those rejects belong at apply time where the operator
//   sees them as "the recipe ran but normalization rejected the
//   record" rather than "the LLM's selector was wrong."
// - For iterator recipes we validate ONE record (the first inner
//   match in the first outer match). Phase 1's per-record shape is
//   uniform within a given page — if one record builds, all do; if
//   the first one's shape mismatches, every subsequent one would
//   too. Validating all N matches would multiply the cost without
//   adding coverage.

/// Validate that a candidate [`FetchRecipe`] would not just extract a
/// scalar but also assemble a record of the binding's declared content
/// type. The successor to [`validate_recipe_against_bytes`] used by
/// the recipe-author, which catches the shape-mismatch class
/// (`string in numeric slot`, `missing required field`) the
/// extraction-only validator cannot see.
///
/// Strict superset of [`validate_recipe_against_bytes`]: any error
/// the extraction-only validator returns is also returned here, plus
/// `ContentAssembly`, `Binding`, and `FieldMapping` errors that
/// require a `ResearchPlan` to surface.
///
/// Session 53 Piece B.
pub(crate) fn validate_recipe_shape_against_bytes(
    recipe: &FetchRecipe,
    bytes: &[u8],
    plan: &ResearchPlan,
) -> Result<(), ApplyError> {
    // Defer to the existing structural validator first. It catches
    // the no-extracted-bytes class (selector matches nothing, JSON
    // path resolves to null, pdf_table out of range) which is
    // independent of the binding's content type and produces clearer
    // error wording. If a recipe fails this preflight there's no
    // useful shape work to do — the bytes never reach build_record.
    validate_recipe_against_bytes(recipe, bytes)?;

    // The same ApplyContext build_record would see. `fetched_at` is
    // synthetic — shape validation does not depend on the timestamp;
    // we pass `Utc::now()` so the constructed Envelope is well-formed
    // for the path-resolution machinery (some plan-derived field
    // mappings reference `valid_at`-shaped values).
    let ctx = ApplyContext {
        recipe,
        plan,
        bytes,
        fetched_at: Utc::now(),
    };

    // The dedup_key on a shape-validation record is synthetic — the
    // shape check is content-type-only; the runtime stamps the real
    // dedup_key after the content type-checks.
    let dedup_key = match &recipe.iterator {
        None => None,
        Some(_) => Some(format!("shape-validator:{}", recipe.id)),
    };

    // Acquire one representative extracted scalar plus, for
    // ExtractedInner-using bindings, the real per-FieldMap inner
    // leaves the runtime would see at apply time. Scoping the work
    // inside one match arm per (iterator-mode, outer-mode) keeps the
    // borrow of the parsed document / JSON local — `scraper::ElementRef`
    // and `&serde_json::Value` borrow from the parse output, so we
    // build records inline within the borrow scope.
    //
    // ADR 0019 Phase 2A: for ExtractedInner-using bindings we run the
    // inner sub-specs against the same scope the runtime would use:
    //   - css_select iterator → the first iter match (ElementRef sub-tree).
    //   - css_select scalar    → the first outer-extraction match.
    //   - json_path iterator   → the first non-null iter match (JSON value).
    //   - json_path scalar     → the first non-null outer-extraction match.
    // The match-arm structure mirrors the runtime's apply dispatch so
    // shape-validator behaviour and apply-stage behaviour cannot drift.
    match (&recipe.iterator, &recipe.extraction) {
        (
            Some(ExtractionSpec::CssSelect {
                selector: iter_selector,
                attribute: _,
            }),
            ExtractionSpec::CssSelect {
                selector: inner_selector,
                attribute: inner_attribute,
            },
        ) => {
            let html_str =
                std::str::from_utf8(bytes).map_err(|e| ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!("bytes were not UTF-8: {e}"),
                })?;
            let doc = Html::parse_document(html_str);
            let iter_sel = Selector::parse(iter_selector).map_err(|e| {
                ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!("iterator selector did not parse: {e}"),
                }
            })?;
            let inner_sel = Selector::parse(inner_selector).map_err(|e| {
                ApplyError::Extraction {
                    mode: "css_select",
                    reason: format!("inner selector did not parse: {e}"),
                }
            })?;
            let scope = doc
                .select(&iter_sel)
                .find(|m| m.select(&inner_sel).next().is_some())
                .ok_or_else(|| ApplyError::Extraction {
                    mode: "css_select",
                    reason: "iterator validator agreed inner matches but \
                             shape validator could not re-locate the \
                             match — bytes mutated under us"
                        .into(),
                })?;
            let extracted =
                extract_css_within(scope, &inner_sel, inner_attribute.as_deref())?;
            for (idx, binding) in recipe.produces.iter().enumerate() {
                let inner_extractions = compute_inner_extractions_css(scope, binding)?;
                let _ = build_record(
                    binding,
                    idx,
                    &extracted,
                    inner_extractions.as_ref(),
                    &ctx,
                    dedup_key.clone(),
                )?;
            }
        }
        (
            Some(ExtractionSpec::JsonPath { path: iter_path }),
            ExtractionSpec::JsonPath { path: inner_path },
        ) => {
            let value: Value =
                serde_json::from_slice(bytes).map_err(|e| ApplyError::Extraction {
                    mode: "json_path",
                    reason: format!("bytes did not parse as JSON: {e}"),
                })?;
            let scope_refs: Vec<&Value> =
                value.query(iter_path).map_err(|e| ApplyError::Extraction {
                    mode: "json_path",
                    reason: format!("iterator path query failed: {e}"),
                })?;
            let scope = scope_refs
                .iter()
                .find(|n| !matches!(n, Value::Null))
                .copied()
                .ok_or_else(|| ApplyError::Extraction {
                    mode: "json_path",
                    reason: format!(
                        "iterator path {iter_path:?} matched no non-null nodes"
                    ),
                })?;
            let extracted = extract_json_within(scope, inner_path)?;
            for (idx, binding) in recipe.produces.iter().enumerate() {
                let inner_extractions = compute_inner_extractions_json(scope, binding)?;
                let _ = build_record(
                    binding,
                    idx,
                    &extracted,
                    inner_extractions.as_ref(),
                    &ctx,
                    dedup_key.clone(),
                )?;
            }
        }
        (Some(_), _) => {
            // Other iterator pairings: caught by the structural
            // validator above; reaching this branch means a Phase 2B
            // mode landed without updating this function.
            return Err(ApplyError::NotImplemented {
                mode: "iterator",
                reason: "shape validator handles css_select × css_select \
                         (ADR 0016 Phase 1) and json_path × json_path \
                         (ADR 0019 Phase 2A) iterator pairings; other \
                         modes (csv_cell, pdf_table, regex_capture) \
                         defer to Phase 2B."
                    .into(),
            });
        }
        (None, ExtractionSpec::CssSelect { selector, .. }) => {
            // Scalar CSS — for ExtractedInner FieldMaps, scope is the
            // first element matching the outer selector. The legacy
            // single-leaf `extracted` comes from the same outer
            // selector via `extract`.
            let extracted = extract(&recipe.extraction, bytes)?;
            let needs_inner_scope = recipe.produces.iter().any(|b| {
                b.field_mappings
                    .iter()
                    .any(|fm| matches!(fm.source, FieldValueSource::ExtractedInner { .. }))
            });
            if needs_inner_scope {
                let html_str =
                    std::str::from_utf8(bytes).map_err(|e| ApplyError::Extraction {
                        mode: "css_select",
                        reason: format!("bytes were not UTF-8: {e}"),
                    })?;
                let doc = Html::parse_document(html_str);
                let outer_sel = Selector::parse(selector).map_err(|e| {
                    ApplyError::Extraction {
                        mode: "css_select",
                        reason: format!("outer selector did not parse: {e}"),
                    }
                })?;
                let scope = doc.select(&outer_sel).next().ok_or_else(|| {
                    ApplyError::Extraction {
                        mode: "css_select",
                        reason: format!(
                            "outer selector {selector:?} matched no elements; \
                             scalar+multi-leaf bindings need an outer scope"
                        ),
                    }
                })?;
                for (idx, binding) in recipe.produces.iter().enumerate() {
                    let inner_extractions =
                        compute_inner_extractions_css(scope, binding)?;
                    let _ = build_record(
                        binding,
                        idx,
                        &extracted,
                        inner_extractions.as_ref(),
                        &ctx,
                        dedup_key.clone(),
                    )?;
                }
            } else {
                for (idx, binding) in recipe.produces.iter().enumerate() {
                    let _ = build_record(
                        binding,
                        idx,
                        &extracted,
                        None,
                        &ctx,
                        dedup_key.clone(),
                    )?;
                }
            }
        }
        (None, ExtractionSpec::JsonPath { path }) => {
            // Scalar JSON — analogue of the scalar CSS branch above.
            let extracted = extract(&recipe.extraction, bytes)?;
            let needs_inner_scope = recipe.produces.iter().any(|b| {
                b.field_mappings
                    .iter()
                    .any(|fm| matches!(fm.source, FieldValueSource::ExtractedInner { .. }))
            });
            if needs_inner_scope {
                let value: Value =
                    serde_json::from_slice(bytes).map_err(|e| ApplyError::Extraction {
                        mode: "json_path",
                        reason: format!("bytes did not parse as JSON: {e}"),
                    })?;
                let nodes: Vec<&Value> =
                    value.query(path).map_err(|e| ApplyError::Extraction {
                        mode: "json_path",
                        reason: format!("outer path query failed: {e}"),
                    })?;
                let scope = nodes
                    .iter()
                    .find(|n| !matches!(n, Value::Null))
                    .copied()
                    .ok_or_else(|| ApplyError::Extraction {
                        mode: "json_path",
                        reason: format!("outer path {path:?} matched no non-null nodes"),
                    })?;
                for (idx, binding) in recipe.produces.iter().enumerate() {
                    let inner_extractions =
                        compute_inner_extractions_json(scope, binding)?;
                    let _ = build_record(
                        binding,
                        idx,
                        &extracted,
                        inner_extractions.as_ref(),
                        &ctx,
                        dedup_key.clone(),
                    )?;
                }
            } else {
                for (idx, binding) in recipe.produces.iter().enumerate() {
                    let _ = build_record(
                        binding,
                        idx,
                        &extracted,
                        None,
                        &ctx,
                        dedup_key.clone(),
                    )?;
                }
            }
        }
        (None, _) => {
            // Scalar non-CSS, non-JSON extraction (csv_cell, pdf_table,
            // regex_capture). ExtractedInner is rejected by validator
            // rule (iv) for these modes, so the legacy single-scalar
            // path is the only path that reaches here.
            let extracted = extract(&recipe.extraction, bytes)?;
            for (idx, binding) in recipe.produces.iter().enumerate() {
                let _ = build_record(
                    binding,
                    idx,
                    &extracted,
                    None,
                    &ctx,
                    dedup_key.clone(),
                )?;
            }
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

    // ---- Session 87: render_selector_path + truncate_excerpt ----------

    #[test]
    fn render_selector_path_css_without_attribute() {
        let s = render_selector_path(&ExtractionSpec::CssSelect {
            selector: "#price".into(),
            attribute: None,
        });
        assert_eq!(s, "css:#price");
    }

    #[test]
    fn render_selector_path_css_with_attribute() {
        let s = render_selector_path(&ExtractionSpec::CssSelect {
            selector: ".quote".into(),
            attribute: Some("data-value".into()),
        });
        assert_eq!(s, "css:.quote[data-value]");
    }

    #[test]
    fn render_selector_path_json() {
        let s = render_selector_path(&ExtractionSpec::JsonPath {
            path: "$.prices[-1].close".into(),
        });
        assert_eq!(s, "json:$.prices[-1].close");
    }

    #[test]
    fn render_selector_path_csv_no_filter() {
        let s = render_selector_path(&ExtractionSpec::CsvCell {
            column: "close".into(),
            row_filter: None,
        });
        assert_eq!(s, "csv:close");
    }

    #[test]
    fn render_selector_path_csv_equals_filter() {
        let s = render_selector_path(&ExtractionSpec::CsvCell {
            column: "close".into(),
            row_filter: Some(RowFilter::Equals {
                column: "date".into(),
                value: "2026-05-15".into(),
            }),
        });
        assert_eq!(s, "csv:close@date=2026-05-15");
    }

    #[test]
    fn render_selector_path_csv_labeled_as() {
        let s = render_selector_path(&ExtractionSpec::CsvCell {
            column: "production".into(),
            row_filter: Some(RowFilter::LabeledAs {
                label_column: "country".into(),
                label: "Australia".into(),
            }),
        });
        assert_eq!(s, "csv:production@country=Australia");
    }

    #[test]
    fn render_selector_path_pdf_table() {
        let s = render_selector_path(&ExtractionSpec::PdfTable {
            page: 7,
            table_index: 0,
            row: 2,
            col: 3,
        });
        assert_eq!(s, "pdf:p7/t0/r2/c3");
    }

    #[test]
    fn render_selector_path_regex() {
        let s = render_selector_path(&ExtractionSpec::RegexCapture {
            pattern: "(\\d+)".into(),
            group: 1,
        });
        assert_eq!(s, "regex:group=1");
    }

    #[test]
    fn truncate_excerpt_short_string_passes_through() {
        let s = truncate_excerpt("613.99");
        assert_eq!(s, "613.99");
    }

    #[test]
    fn truncate_excerpt_long_string_gets_ellipsis() {
        let s = "x".repeat(500);
        let truncated = truncate_excerpt(&s);
        let cap = situation_room_core::schema::envelope::RAW_BYTES_EXCERPT_CAP;
        // 256 'x' codepoints + 1 ellipsis codepoint
        assert_eq!(truncated.chars().count(), cap + 1);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_excerpt_multibyte_codepoint_safe() {
        // Each character is a 3-byte codepoint in UTF-8; cap counts
        // codepoints not bytes so this should NOT split mid-character.
        let s = "日".repeat(500);
        let truncated = truncate_excerpt(&s);
        // round-trips through UTF-8 cleanly
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.ends_with('…'));
    }

    use crate::research::{
        DocumentSourceEntry, DocumentSourceNomination, EntityKindExpectation,
        EventTypeExpectation, GeoScope, MetricExpectation, PriorityTier, RecordExpectations,
        RelationKindExpectation,
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
                    attributes: vec![],
                    rationale: "Unit of supply".into(),
                }],
                relation_kinds: vec![RelationKindExpectation {
                    kind: "operator_of".into(),
                    exemplar_triples: vec![],
                    rationale: "Asset link".into(),
                }],
                document_sources: vec![DocumentSourceEntry::Nomination(
                    DocumentSourceNomination {
                        nomination_id: Uuid::now_v7(),
                        description:
                            "USGS Mineral Commodity Summaries — annual lithium chapter, \
                             mine production by country"
                                .into(),
                        priority_tier: PriorityTier::AuthoritativePrimary,
                    },
                )],
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
                // ADR 0016: scalar-recipe context (no dedup_key_field).
                dedup_key_field: None,
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
            // ADR 0016: scalar-recipe context (no iterator).
            iterator: None,
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

    /// Session 32b: the World Bank Open Data shape has nulls at the
    /// head of every per-country/indicator series for years where
    /// data hasn't been published yet. The path must skip the
    /// leading nulls and return the first real value, not stringify
    /// the leading null to `"null"` and fail downstream content
    /// assembly.
    ///
    /// Live evidence: hungarian-barley-production try-out on the
    /// Session 32a patch returned `invalid type: string "null",
    /// expected f64`. This test locks in the fix.
    #[test]
    fn json_path_skips_leading_nulls_and_returns_first_real_value() {
        // World Bank shape (simplified): two-element top-level array,
        // [paginationmeta, [datapoints]]. The datapoints array carries
        // null `value` for missing years.
        let bytes = br#"[
            {"page": 1, "pages": 1, "per_page": 5, "total": 5},
            [
                {"date": "2024", "value": null},
                {"date": "2023", "value": null},
                {"date": "2022", "value": 178832000000.0},
                {"date": "2021", "value": 181847000000.0},
                {"date": "2020", "value": 156800000000.0}
            ]
        ]"#;
        // `$[1][*].value` matches all five `value` fields; the first
        // two are null, the third is the first real number.
        let out = extract_json_path(bytes, "$[1][*].value").unwrap();
        assert_eq!(out, "178832000000.0");
    }

    /// When every matched node is null, the error message must name
    /// the pattern (so the operator reading the recipes panel and
    /// the response-bytes chip can write a useful flag note) and
    /// suggest the canonical fix (a filter expression).
    #[test]
    fn json_path_all_null_returns_actionable_error() {
        let bytes = br#"[
            {"date": "2024", "value": null},
            {"date": "2023", "value": null}
        ]"#;
        let err = extract_json_path(bytes, "$[*].value").unwrap_err();
        let reason = match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "json_path");
                reason
            }
            other => panic!("expected Extraction, got {other:?}"),
        };
        // The error must (a) name JSON null as the cause, (b)
        // suggest a filter-expression fix. Both signals show up in
        // the chip-and-bytes diagnostic flow Session 31/32 built.
        assert!(
            reason.to_lowercase().contains("null"),
            "error must name null as the cause; got: {reason}"
        );
        assert!(
            reason.contains("filter expression") || reason.contains("?("),
            "error must point at the filter-expression fix; got: {reason}"
        );
    }

    /// A single-node match on JSON null hits the same actionable
    /// error path as the all-null multi-match case. Without this,
    /// a path like `$[0].value` against `[{"value": null}]` would
    /// return the string `"null"` and reproduce the original
    /// `invalid type: string "null", expected f64` failure.
    #[test]
    fn json_path_single_null_match_returns_actionable_error() {
        let bytes = br#"{"value": null}"#;
        let err = extract_json_path(bytes, "$.value").unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "json_path");
                assert!(
                    reason.to_lowercase().contains("null"),
                    "error must name null; got: {reason}"
                );
            }
            other => panic!("expected Extraction, got {other:?}"),
        }
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
    // Session 33: scalar size bound + content-assembly error truncation
    //
    // These tests cover the runtime safeguard that catches recipes
    // whose extraction returns a multi-KB blob — typically because
    // the LLM authored a CSS selector against a container element
    // (body, div) on a non-data HTML page. Live evidence: a
    // hungarian-barley-production run against `usgs_mcs` produced an
    // apply error whose Display rendered ~12 KB of HTML/JSON page
    // bytes into the recipe outcome's `message`, polluting logs and
    // the desktop UI. The bound rejects oversized extractions at the
    // extractor layer with a small named error; the truncation helper
    // keeps content-assembly errors readable when they do reach that
    // stage with a legitimately-sized but type-wrong value.
    // -----------------------------------------------------------------------

    #[test]
    fn css_select_rejects_oversized_extraction() {
        // A wide selector (`body`) on a real-world-looking HTML page
        // returns the full rendered text. `bound_extracted` should
        // turn that into a small actionable error that names the
        // scalar contract, not a 12 KB blob.
        let huge = "x".repeat(EXTRACTED_SCALAR_MAX_BYTES + 100);
        let html = format!("<html><body>{huge}</body></html>");
        let err = extract_css_select(html.as_bytes(), "body", None).unwrap_err();
        let ApplyError::Extraction { mode, reason } = err else {
            panic!("expected Extraction error, got {err:?}");
        };
        assert_eq!(mode, "css_select");
        assert!(
            reason.contains("scalar"),
            "reason should explain the scalar contract, got {reason:?}"
        );
        assert!(
            reason.contains(&EXTRACTED_SCALAR_MAX_BYTES.to_string()),
            "reason should name the bound, got {reason:?}"
        );
        assert!(
            reason.len() < 1024,
            "reason itself must stay small (got {} bytes); the whole \
             point of the bound is to keep error messages short",
            reason.len()
        );
    }

    #[test]
    fn json_path_rejects_oversized_extraction() {
        // Mirror of the css_select case for the JSON mode: a path
        // that resolves to a large JSON-stringified object should
        // not flow into content assembly as an enormous scalar.
        let huge = "y".repeat(EXTRACTED_SCALAR_MAX_BYTES + 100);
        let bytes = format!(r#"{{"data":"{huge}"}}"#).into_bytes();
        let err = extract_json_path(&bytes, "$.data").unwrap_err();
        assert!(
            matches!(err, ApplyError::Extraction { mode: "json_path", .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn bound_extracted_passes_typical_field_sizes() {
        // Sanity: a legitimately-sized extraction (a scientific
        // notation number, a multi-paragraph event description, a
        // formatted price) must continue to pass through unchanged.
        for fixture in [
            "3.14",
            "1,234,567.89 USD",
            // ~600 chars: a verbose event description, well within
            // the 2 KB bound.
            &"This is a long event description ".repeat(18),
        ] {
            let out = bound_extracted(fixture.to_string(), "json_path")
                .expect("typical sizes must pass the bound");
            assert_eq!(out, fixture);
        }
    }

    #[test]
    fn truncate_content_assembly_reason_passes_short_messages_unchanged() {
        let short = "observation content: invalid type: string \"foo\", expected f64";
        assert_eq!(
            truncate_content_assembly_reason(short.to_string()),
            short
        );
    }

    #[test]
    fn truncate_content_assembly_reason_preserves_head_and_tail() {
        // Construct an error whose middle is a 4 KB value — the
        // shape serde_json produces when the offending string is
        // big. The truncated message must:
        //   1. be bounded (operator-readable in logs and UI)
        //   2. preserve the head (so the operator sees which content
        //      type and which type mismatch)
        //   3. preserve the tail (so the operator sees the expected
        //      type — `expected f64` etc.)
        //   4. carry an explicit length marker so the operator knows
        //      the truncation happened and how much was elided.
        let middle = "Z".repeat(4_000);
        let reason = format!(
            "observation content: invalid type: string \"{middle}\", expected f64"
        );
        let truncated = truncate_content_assembly_reason(reason.clone());

        assert!(
            truncated.chars().count()
                <= CONTENT_ASSEMBLY_REASON_MAX_CHARS + 32,
            "truncated message must stay near the cap, got {} chars",
            truncated.chars().count()
        );
        assert!(
            truncated.starts_with("observation content: invalid type: string"),
            "head must survive: {truncated}"
        );
        assert!(
            truncated.ends_with("expected f64"),
            "tail must survive: {truncated}"
        );
        assert!(
            truncated.contains("truncated"),
            "must signal truncation: {truncated}"
        );
        assert!(
            truncated.contains(&reason.chars().count().to_string()),
            "must name the original total length: {truncated}"
        );
    }

    // -----------------------------------------------------------------------
    // Schema-aware coercion at content assembly (Session 64)
    //
    // Reproduces the Fed-volatility incident in unit form: a same-plan
    // re-run after source-side leaf drift turning numeric-looking
    // ("22", "22.0") used to fail at content assembly because
    // parse_extracted_scalar promoted the leaf to f64 and serde then
    // rejected the Number where the schema declared String. The
    // coercion layer in build_record now lines those values up before
    // serde sees them.
    // -----------------------------------------------------------------------

    #[test]
    fn path_expects_string_covers_event_headline() {
        // The exact path that broke on federalreserve.gov.
        assert!(path_expects_string(RecordType::Event, "headline"));
    }

    #[test]
    fn path_expects_string_covers_observation_metric_unit_currency_period() {
        // Every JSON-string field on ObservationContent.
        for path in ["metric", "unit", "currency", "period"] {
            assert!(
                path_expects_string(RecordType::Observation, path),
                "Observation.{path} must be in the String set"
            );
        }
    }

    #[test]
    fn path_expects_string_rejects_observation_value() {
        // The canonical numeric field — must not be coerced or apply's
        // primary contract breaks.
        assert!(!path_expects_string(RecordType::Observation, "value"));
        assert!(
            !path_expects_string(RecordType::Observation, "value_uncertainty"),
            "value_uncertainty is Option<f64>, not String"
        );
    }

    #[test]
    fn path_expects_string_covers_relation_entity_endpoints() {
        // EntityId is a newtype around String; from/to are JSON
        // strings at the wire shape.
        assert!(path_expects_string(RecordType::Relation, "from"));
        assert!(path_expects_string(RecordType::Relation, "to"));
        assert!(path_expects_string(RecordType::Relation, "kind"));
    }

    #[test]
    fn path_expects_string_covers_nested_magnitude_paths() {
        // Event.magnitude and Relation.magnitude both embed an
        // ObservationContent — its String paths inherit through.
        for record_type in [RecordType::Event, RecordType::Relation] {
            for path in [
                "magnitude.metric",
                "magnitude.unit",
                "magnitude.currency",
                "magnitude.period",
            ] {
                assert!(
                    path_expects_string(record_type, path),
                    "{record_type:?}.{path} must be in the String set"
                );
            }
        }
    }

    #[test]
    fn path_expects_string_empty_for_non_recipe_record_types() {
        // Document / Assertion are not recipe-producible. The set
        // is empty so a future hand-rolled call site can't silently
        // rely on coercion that won't fire. Entity is recipe-producible
        // from Sn-97 Lever B onward — see its dedicated test below.
        for record_type in [RecordType::Document, RecordType::Assertion] {
            assert!(!path_expects_string(record_type, "any_path"));
            assert!(!path_expects_string(record_type, "headline"));
        }
    }

    #[test]
    fn path_expects_string_covers_entity_fields() {
        // Sn-97 Lever B — `entity` opened as a recipe-producible
        // record_type. The three Entity fields (entity_id, kind,
        // canonical_name) are all schema-typed as String / String-
        // newtype, so a numeric-looking leaf (entity_id `"9123456"`,
        // kind authored as `literal "driver"`, canonical_name
        // `"Adriano Moraes"`) must coerce to String at content
        // assembly rather than fail with `invalid type: floating
        // point '9123456', expected a string`.
        for path in ["entity_id", "kind", "canonical_name"] {
            assert!(
                path_expects_string(RecordType::Entity, path),
                "Entity.{path} must be in the String set"
            );
        }
        // Negative: a path not on EntityContent isn't in the set —
        // catches a future `geometry` addition that didn't get added
        // here when the binding shape grew.
        assert!(!path_expects_string(RecordType::Entity, "geometry"));
        assert!(!path_expects_string(RecordType::Entity, "value"));
    }

    #[test]
    fn coerce_for_string_path_stringifies_number_at_known_path() {
        // The exact transformation the Fed incident needed.
        let mut v = json!(22.0);
        coerce_for_string_path(&mut v, RecordType::Event, "headline");
        assert_eq!(v, json!("22.0"));

        // Integer-shaped Numbers come through as their integer
        // representation — serde_json::Number's Display impl picks
        // the canonical form.
        let mut v = json!(22);
        coerce_for_string_path(&mut v, RecordType::Event, "headline");
        assert_eq!(v, json!("22"));
    }

    #[test]
    fn coerce_for_string_path_leaves_value_field_alone() {
        // Negative case: the prime numeric field on Observation must
        // remain a Number. Stringifying it would break the f64
        // contract for every Observation recipe in production.
        let mut v = json!(22.0);
        coerce_for_string_path(&mut v, RecordType::Observation, "value");
        assert_eq!(v, json!(22.0));
    }

    #[test]
    fn coerce_for_string_path_leaves_already_string_alone() {
        // The common case — leaf was non-numeric, parse_extracted_scalar
        // returned a String, no coercion needed.
        let mut v = json!("Fed raised rates");
        coerce_for_string_path(&mut v, RecordType::Event, "headline");
        assert_eq!(v, json!("Fed raised rates"));
    }

    #[test]
    fn coerce_for_string_path_leaves_non_number_non_string_alone() {
        // Boolean / null / object / array land at content assembly
        // through other paths (Literal, FromPlan). The coercion is
        // narrow: only Number → String. Other shapes pass through and
        // serde produces its own error message at deserialization,
        // which is the right signal for those mismatches.
        for mut v in [json!(true), json!(null), json!({}), json!([])] {
            let before = v.clone();
            coerce_for_string_path(&mut v, RecordType::Event, "headline");
            assert_eq!(v, before, "non-Number/non-String must pass through");
        }
    }

    /// End-to-end: an Event recipe extracts `"22.0"` from CSS and
    /// binds it into `headline`. Pre-fix this failed at content
    /// assembly with `invalid type: floating point '22.0', expected
    /// a string`. Post-fix the record builds with `headline = "22.0"`
    /// — a degraded but honest representation that the operator can
    /// see, re-author against, or accept.
    #[test]
    fn end_to_end_event_recipe_with_numeric_leaf_stringifies_into_headline() {
        let html = br#"<html><body><h1>22.0</h1></body></html>"#;
        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "federalreserve.gov".into(),
            source_url: Url::parse("https://www.federalreserve.gov/x").unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: "h1".into(),
                attribute: None,
            },
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
                        source: FieldValueSource::Extracted,
                    },
                ],
                dedup_key_field: None,
            }],
            iterator: None,
            authored_at: Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let p = plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).expect(
            "post-fix: numeric-looking leaf into headline must coerce to \
             String and assemble cleanly",
        );
        assert_eq!(records.len(), 1);
        let ev = match &records[0] {
            Record::Event(e) => e,
            other => panic!("expected Event, got {other:?}"),
        };
        assert_eq!(ev.content.headline, "22.0");
    }

    /// Companion negative case: the canonical Observation recipe with
    /// a numeric leaf into `value` must still produce a numeric `value`
    /// (the f64 path is the apply contract for every observation
    /// recipe in production). Locks in that the coercion is path-
    /// specific and does not regress the numeric path.
    #[test]
    fn end_to_end_observation_recipe_keeps_numeric_value_unchanged() {
        let csv = b"country,production\nChile,49000\n";
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
        assert_eq!(obs.content.value, 49000.0);
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
    // Numeric-format normalizer — Session 53 Piece D
    //
    // The 2026-05-09 18:11 USGS MCS run authoring decline cited
    // "comma-formatted numbers (e.g. 74,700) and estimate prefixes
    // that prevent clean numeric extraction to f64 via pdf_table" —
    // a real apply-stage limitation the recipe-author refused to
    // work around. The normalizer broadens what apply will accept
    // from the formats the recipe-author reasonably authors against
    // human-readable tables (USGS production figures, IEA fact
    // sheets' `est. 1,200`-style estimates, FT/Bloomberg headline
    // `$1,234`).
    //
    // These tests pin the bounded contract: which shapes the
    // normalizer accepts, and which (specifically: ambiguous-locale
    // shapes) it deliberately leaves alone.
    // -----------------------------------------------------------------------

    #[test]
    fn normalizer_accepts_us_locale_thousand_separators() {
        // The USGS MCS class — `74,700` is the canonical lithium
        // production cell shape.
        assert_eq!(parse_extracted_scalar("74,700"), json!(74700.0));
        assert_eq!(parse_extracted_scalar("1,234,567"), json!(1234567.0));
        assert_eq!(parse_extracted_scalar("-3,200.5"), json!(-3200.5));
    }

    #[test]
    fn normalizer_accepts_currency_prefix_and_suffix() {
        // FT/Bloomberg headline shape — `$1,234` and `1,234 USD`
        // both occur in news prose. Currency is a positional marker
        // around a numeric body.
        assert_eq!(parse_extracted_scalar("$1,234.56"), json!(1234.56));
        assert_eq!(parse_extracted_scalar("€1,000"), json!(1000.0));
        assert_eq!(parse_extracted_scalar("£99"), json!(99.0));
        assert_eq!(parse_extracted_scalar("¥500"), json!(500.0));
        assert_eq!(parse_extracted_scalar("USD 1,234"), json!(1234.0));
        assert_eq!(parse_extracted_scalar("1,234 USD"), json!(1234.0));
        assert_eq!(parse_extracted_scalar("1,234 EUR"), json!(1234.0));
    }

    #[test]
    fn normalizer_accepts_estimate_prefixes() {
        // Agency tables routinely tag estimated cells with a
        // marker. `est. 1,200`, `~5000`, `≈10,000` all appear in
        // IEA / USGS / Australian RE Quarterly chapters.
        assert_eq!(parse_extracted_scalar("est. 1,200"), json!(1200.0));
        assert_eq!(parse_extracted_scalar("est 800"), json!(800.0));
        assert_eq!(parse_extracted_scalar("~5000"), json!(5000.0));
        assert_eq!(parse_extracted_scalar("≈10,000"), json!(10000.0));
    }

    #[test]
    fn normalizer_preserves_scientific_notation() {
        // Scientific notation must not be mangled by the estimate-
        // prefix rules — `1.5e9` is one billion and a half, not an
        // estimate of 1.5 followed by 9.
        assert_eq!(parse_extracted_scalar("1.5e9"), json!(1_500_000_000.0));
        assert_eq!(parse_extracted_scalar("2e3"), json!(2000.0));
        assert_eq!(parse_extracted_scalar("-1.5e9"), json!(-1_500_000_000.0));
    }

    #[test]
    fn normalizer_rejects_eu_locale_decimal_comma() {
        // `1.234,56` is ambiguous (US would mis-parse as 1234.56;
        // EU means 1234.56 written EU-style). The normalizer
        // refuses to guess: returns the original as Value::String
        // so apply fails honestly with the un-normalised string in
        // the error message. This is the explicit
        // "intentionally not in this patch" carve-out from the
        // Session 53 handoff.
        assert_eq!(parse_extracted_scalar("1.234,56"), json!("1.234,56"));
        assert_eq!(parse_extracted_scalar("88.000,0"), json!("88.000,0"));
    }

    #[test]
    fn normalizer_rejects_genuinely_non_numeric() {
        assert_eq!(parse_extracted_scalar("abc"), json!("abc"));
        assert_eq!(parse_extracted_scalar("Argentina"), json!("Argentina"));
        assert_eq!(parse_extracted_scalar(""), json!(""));
    }

    #[test]
    fn normalizer_rejects_malformed_comma_positions() {
        // `1,23` has a comma not at a thousand-separator position.
        // The conservative rule: don't strip the comma; let parse
        // fail. The fallback then takes the leading numeric prefix
        // `1`, which is the most defensible interpretation when the
        // overall string is structurally broken.
        // (Old code stripped all commas unconditionally, producing
        // `123` — see parse_extracted_scalar history. Session 53
        // narrows the strip to canonical positions only.)
        let v = parse_extracted_scalar("1,23");
        assert!(
            v == json!(1.0) || v == json!("1,23"),
            "ambiguous comma position should produce conservative \
             leading-prefix parse or string fallback, not silent \
             123; got {v:?}"
        );
    }

    #[test]
    fn normalizer_handles_internal_whitespace() {
        // `1 234.5` — Australian REQ tables sometimes use thin
        // spaces (which collapse to ASCII whitespace through
        // upstream extraction). Treat as canonical thousand
        // separator.
        assert_eq!(parse_extracted_scalar("1 234.5"), json!(1234.5));
    }

    #[test]
    fn normalizer_preserves_trailing_unit_via_prefix_fallback() {
        // Pre-Session-53: `"49,000 t"` extracted from a CSV cell
        // where the recipe-author selected the wrong column was
        // parsed as 49000 by stripping the unit. Session 53
        // preserves that fallback through the leading-numeric-
        // prefix path.
        assert_eq!(parse_extracted_scalar("49,000 t"), json!(49000.0));
        assert_eq!(parse_extracted_scalar("12.5%"), json!(12.5));
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

    // -----------------------------------------------------------------------
    // Session 38 — iterator runtime (ADR 0016 Phase 1)
    // -----------------------------------------------------------------------

    /// A plan whose first event_type expectation is `milestone_announced`,
    /// matching the listing-source case the iterator targets. Built from
    /// the lithium-shaped `plan()` so the rest of the apply path
    /// (provenance, topic tags) stays unchanged across scalar and
    /// iterator tests.
    fn iterator_plan() -> ResearchPlan {
        plan() // sample plan already has one event_type expectation
    }

    /// Helper: build an iterator-bearing recipe with css_select ×
    /// css_select. The iterator selects `.card` elements; the inner
    /// extraction reads each card's `h3` text. Each card produces
    /// one Event record with a literal event_type and an extracted
    /// headline; dedup_key_field references "headline".
    fn iterator_event_recipe(iter_selector: &str, inner_selector: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "listing_source".into(),
            source_url: Url::parse("https://example.com/listing").unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: inner_selector.into(),
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: iter_selector.into(),
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
                        source: FieldValueSource::Extracted,
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    /// Helper: build an iterator-bearing recipe with json_path ×
    /// json_path. Same Event-record shape as
    /// [`iterator_event_recipe`] (one literal event_type, one
    /// extracted headline, dedup_key_field on headline) so the
    /// validator tests can vary only the bytes side. ADR 0019
    /// Phase 2A case — the FEMA / FRED / World-Bank API shape.
    fn iterator_event_recipe_json(iter_path: &str, inner_path: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "listing_source".into(),
            source_url: Url::parse("https://example.com/listing.json").unwrap(),
            extraction: ExtractionSpec::JsonPath {
                path: inner_path.into(),
            },
            iterator: Some(ExtractionSpec::JsonPath {
                path: iter_path.into(),
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
                        source: FieldValueSource::Extracted,
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 13, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    /// Three-card listing: iterator selects three `.card`s, inner
    /// selector reads each card's `h3`, three Event records emerge
    /// with the three headlines verbatim. ADR 0016's Phase 1 happy
    /// path.
    #[test]
    fn css_select_iterator_produces_n_records() {
        let html = br#"
            <html><body>
              <div class="card"><h3>Quantum supremacy claim verified</h3></div>
              <div class="card"><h3>Photonic chip hits 1024 qubits</h3></div>
              <div class="card"><h3>Error correction crosses threshold</h3></div>
            </body></html>
        "#;
        let recipe = iterator_event_recipe(".card", "h3");
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).unwrap();
        assert_eq!(records.len(), 3, "expected one record per card");

        // Each record is an Event with the matching headline and the
        // literal event_type.
        let headlines: Vec<&str> = records
            .iter()
            .map(|r| match r {
                Record::Event(e) => e.content.headline.as_str(),
                other => panic!("expected Event, got {other:?}"),
            })
            .collect();
        assert_eq!(
            headlines,
            vec![
                "Quantum supremacy claim verified",
                "Photonic chip hits 1024 qubits",
                "Error correction crosses threshold",
            ]
        );

        // Each record carries a stamped dedup_key of the form
        // `{recipe.id}:{headline}` — the per-record natural key
        // ADR 0016 §"Per-match dedup becomes load-bearing" requires.
        for (rec, expected_headline) in records.iter().zip(headlines.iter()) {
            let key = match rec {
                Record::Event(e) => e.dedup_key.as_ref().expect("dedup_key set"),
                _ => unreachable!(),
            };
            let want = format!("{}:{}", recipe.id, expected_headline);
            assert_eq!(key, &want, "dedup_key mismatch");
        }
    }

    /// Cap enforcement: 600 cards in the document → ApplyError::Extraction
    /// with the cap message. No records are produced.
    #[test]
    fn iterator_caps_records_at_max() {
        // Build an HTML body with 600 cards. Keep the per-card payload
        // tiny so the test is fast; the cap check fires before any
        // per-match work happens.
        let mut body = String::with_capacity(60_000);
        body.push_str("<html><body>");
        for i in 0..600 {
            body.push_str(&format!("<div class=\"card\"><h3>card {i}</h3></div>"));
        }
        body.push_str("</body></html>");
        let recipe = iterator_event_recipe(".card", "h3");
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: body.as_bytes(),
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "css_select");
                assert!(
                    reason.contains("cap is")
                        && reason.contains(&MAX_RECORDS_PER_RECIPE.to_string()),
                    "expected cap message, got: {reason}"
                );
            }
            other => panic!("expected Extraction error, got {other:?}"),
        }
    }

    /// Iterator with zero matches reports the iterator selector by
    /// name (not the inner selector) so the operator's debug path
    /// points at the right level. The contrast with the scalar path
    /// (which reports the only selector's name) matters: iterator
    /// recipes have two selectors and the failure layer needs to be
    /// unambiguous.
    #[test]
    fn iterator_with_zero_matches_reports_iterator_selector() {
        let html = b"<html><body><p>nothing here</p></body></html>";
        let recipe = iterator_event_recipe(".card", "h3");
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "css_select");
                assert!(reason.contains("iterator"), "got: {reason}");
                assert!(reason.contains(".card"), "got: {reason}");
            }
            other => panic!("expected Extraction error, got {other:?}"),
        }
    }

    /// When the iterator matches but the inner selector misses inside
    /// a card, the error message points at the inner-selector layer,
    /// not the iterator. Tests the symmetry of layer-naming with
    /// `iterator_with_zero_matches_reports_iterator_selector` above.
    #[test]
    fn iterator_inner_selector_miss_reports_inner_layer() {
        let html = b"<html><body>
            <div class=\"card\"><p>no h3 in here</p></div>
        </body></html>";
        let recipe = iterator_event_recipe(".card", "h3");
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "css_select");
                assert!(
                    reason.contains("inner selector") && reason.contains("iterator"),
                    "got: {reason}"
                );
            }
            other => panic!("expected Extraction error, got {other:?}"),
        }
    }

    /// Cross-mode iterator/extraction pairs surface as
    /// `NotImplemented` at the apply boundary. The validator should
    /// reject these at authoring time; this test pins the runtime's
    /// defensive shape for hand-edits and Phase-2-shaped recipes.
    #[test]
    fn iterator_with_cross_mode_pair_is_not_implemented() {
        // Build a recipe with css_select iterator and json_path
        // extraction directly (bypassing the validator).
        let mut recipe = iterator_event_recipe(".card", "h3");
        recipe.extraction = ExtractionSpec::JsonPath {
            path: "$.title".into(),
        };
        let html = b"<html><body><div class=\"card\"><h3>x</h3></div></body></html>";
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        match err {
            ApplyError::NotImplemented { mode, reason } => {
                assert_eq!(mode, "iterator");
                assert!(reason.contains("css_select") && reason.contains("json_path"));
            }
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    /// Scalar-recipe records continue to carry `dedup_key: None` after
    /// Session 38. ADR 0016 §Carry-forward dependencies: the scalar
    /// path is unchanged; only iterator records get the per-record key.
    #[test]
    fn scalar_recipe_records_still_carry_no_dedup_key() {
        let csv = b"country,production\nChile,49000\n";
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
        match &records[0] {
            Record::Observation(o) => {
                assert!(
                    o.dedup_key.is_none(),
                    "scalar recipe should carry no dedup_key, got {:?}",
                    o.dedup_key
                );
            }
            other => panic!("expected Observation, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // ADR 0019 Phase 2A — ExtractedInner runtime (Session 61)
    //
    // Per-field extraction sub-specs let one record carry multiple
    // extracted leaves (an event with date + headline; a relation
    // with from + to). The runtime evaluates each inner sub-spec
    // against the same per-match scope the binding's outer
    // extraction operates on. These tests pin the css_select
    // iterator path (the NHC storm-row motivating case) and the
    // json_path iterator path (the array-of-objects API case).
    // -----------------------------------------------------------------------

    /// Build a css_select iterator recipe with multi-leaf bindings:
    /// `headline` and `direction` are extracted via inner sub-selectors;
    /// `event_type` is literal. Two extracted leaves per record exercise
    /// the ADR 0019 multi-leaf path. The outer extraction is required
    /// structurally but is benign — its scalar leaf is unused at the
    /// FieldMap level because no FieldMap has `FieldValueSource::Extracted`.
    fn multi_leaf_iterator_event_recipe() -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "nhc".into(),
            source_url: Url::parse("https://example.com/storms").unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: "td.storm-name".into(),
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: "tr.storm-row".into(),
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
                                selector: "td.storm-name".into(),
                                attribute: None,
                            },
                        },
                    },
                    FieldMap {
                        path: "direction".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::CssSelect {
                                selector: "td.storm-direction".into(),
                                attribute: None,
                            },
                        },
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    #[test]
    fn css_iterator_multi_leaf_produces_records_with_multiple_extracted_fields_adr_0019() {
        // Two storm rows; each row carries a name (`td.storm-name`)
        // and a direction tag (`td.storm-direction`). Under ADR 0019
        // the runtime produces two Event records, each with a
        // distinct headline *and* a distinct direction — both pulled
        // per-row via ExtractedInner sub-selectors. The direction
        // value deserialises into `EventDirection` because the
        // fixture's text matches the snake_case variant names.
        let html = br#"
            <html><body><table class="storms">
              <tr class="storm-row">
                <td class="storm-name">Hurricane Alpha</td>
                <td class="storm-direction">supply_negative</td>
              </tr>
              <tr class="storm-row">
                <td class="storm-name">Hurricane Beta</td>
                <td class="storm-direction">context</td>
              </tr>
            </table></body></html>
        "#;
        let recipe = multi_leaf_iterator_event_recipe();
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).expect("multi-leaf iterator must apply");
        assert_eq!(records.len(), 2, "one record per storm row");

        let mut by_headline: Vec<(&str, Option<&'static str>)> = records
            .iter()
            .map(|r| match r {
                Record::Event(e) => {
                    let dir_tag = e.content.direction.map(|d| match d {
                        situation_room_core::schema::content::EventDirection::SupplyNegative => {
                            "supply_negative"
                        }
                        situation_room_core::schema::content::EventDirection::Context => "context",
                        situation_room_core::schema::content::EventDirection::SupplyPositive => {
                            "supply_positive"
                        }
                        situation_room_core::schema::content::EventDirection::DemandPositive => {
                            "demand_positive"
                        }
                        situation_room_core::schema::content::EventDirection::DemandNegative => {
                            "demand_negative"
                        }
                    });
                    (e.content.headline.as_str(), dir_tag)
                }
                other => panic!("expected Event, got {other:?}"),
            })
            .collect();
        by_headline.sort_by_key(|(h, _)| *h);
        assert_eq!(by_headline[0].0, "Hurricane Alpha");
        assert_eq!(by_headline[0].1, Some("supply_negative"));
        assert_eq!(by_headline[1].0, "Hurricane Beta");
        assert_eq!(by_headline[1].1, Some("context"));

        // Dedup keys reference the per-row headline (the
        // dedup_key_field on the binding), resolved via the
        // ExtractedInner path — confirms compute_dedup_key threads
        // the inner-extractions map correctly.
        for rec in &records {
            let (key, headline) = match rec {
                Record::Event(e) => (
                    e.dedup_key.as_ref().expect("dedup_key set"),
                    e.content.headline.as_str(),
                ),
                _ => unreachable!(),
            };
            let want = format!("{}:{}", recipe.id, headline);
            assert_eq!(key, &want, "dedup_key should resolve to per-row headline");
        }
    }

    /// JSONPath iterator + ExtractedInner: ADR 0019's other Phase 2A
    /// case. The outer path resolves to an array of objects; per
    /// object, inner paths resolve to per-field leaves. Mirrors the
    /// FRED / FEMA-style API listing shape.
    #[test]
    fn json_iterator_multi_leaf_produces_records_with_multiple_extracted_fields_adr_0019() {
        let json_bytes = br#"{"storms":[
            {"name":"Hurricane Alpha","direction":"supply_negative"},
            {"name":"Hurricane Beta","direction":"context"}
        ]}"#;
        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "api".into(),
            source_url: Url::parse("https://example.com/api/storms.json").unwrap(),
            extraction: ExtractionSpec::JsonPath {
                path: "$.name".into(),
            },
            iterator: Some(ExtractionSpec::JsonPath {
                path: "$.storms[*]".into(),
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
                            spec: ExtractionSpec::JsonPath {
                                path: "$.name".into(),
                            },
                        },
                    },
                    FieldMap {
                        path: "direction".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::JsonPath {
                                path: "$.direction".into(),
                            },
                        },
                    },
                ],
                dedup_key_field: Some("headline".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: json_bytes,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).expect("json multi-leaf iterator must apply");
        assert_eq!(records.len(), 2);

        let mut headlines: Vec<&str> = records
            .iter()
            .map(|r| match r {
                Record::Event(e) => e.content.headline.as_str(),
                other => panic!("expected Event, got {other:?}"),
            })
            .collect();
        headlines.sort();
        assert_eq!(headlines, vec!["Hurricane Alpha", "Hurricane Beta"]);

        // Verify both records carry a per-row direction — confirms
        // each ExtractedInner sub-spec ran independently against its
        // per-match JSON scope.
        let directions: Vec<bool> = records
            .iter()
            .map(|r| match r {
                Record::Event(e) => e.content.direction.is_some(),
                _ => unreachable!(),
            })
            .collect();
        assert!(directions.iter().all(|d| *d), "every record carries direction");
    }

    // -----------------------------------------------------------------------
    // Session 97 Lever B — recipe-driven Entity production
    //
    // An iterator-bearing recipe against an entity_kind expectation
    // emits one Entity row per iterator row. Each row pulls
    // entity_id + canonical_name via ExtractedInner; kind is a
    // literal. The build_record Entity arm assembles the three
    // flat fields; the apply path returns Record::Entity values.
    // -----------------------------------------------------------------------

    #[test]
    fn css_iterator_entity_recipe_produces_one_entity_per_row_sn97_lever_b() {
        let html = br#"
            <html><body><table class="roster"><tbody>
              <tr><td>driver:adriano_moraes</td><td>Adriano Moraes</td></tr>
              <tr><td>driver:guilherme_marchi</td><td>Guilherme Marchi</td></tr>
              <tr><td>driver:silvano_alves</td><td>Silvano Alves</td></tr>
            </tbody></table></body></html>
        "#;
        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "roster".into(),
            source_url: Url::parse("https://example.com/roster").unwrap(),
            // Outer extraction is required structurally for
            // iterator-bearing recipes but unused at the FieldMap
            // level (every FieldMap is ExtractedInner or Literal).
            extraction: ExtractionSpec::CssSelect {
                selector: "table.roster tbody tr td:nth-child(1)".into(),
                attribute: None,
            },
            iterator: Some(ExtractionSpec::CssSelect {
                selector: "table.roster tbody tr".into(),
                attribute: None,
            }),
            produces: vec![ProductionBinding {
                record_type: RecordType::Entity,
                expectation: ExpectationRef::EntityKind { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "entity_id".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::CssSelect {
                                selector: "td:nth-child(1)".into(),
                                attribute: None,
                            },
                        },
                    },
                    FieldMap {
                        path: "kind".into(),
                        source: FieldValueSource::Literal {
                            value: json!("driver"),
                        },
                    },
                    FieldMap {
                        path: "canonical_name".into(),
                        source: FieldValueSource::ExtractedInner {
                            spec: ExtractionSpec::CssSelect {
                                selector: "td:nth-child(2)".into(),
                                attribute: None,
                            },
                        },
                    },
                ],
                dedup_key_field: Some("entity_id".into()),
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).expect("entity iterator recipe must apply");
        assert_eq!(records.len(), 3, "one Entity per iterator row");

        // Each record is an Entity with the row's id + literal kind +
        // row's display name. Sort by entity_id for deterministic
        // assertion (iterator order matches doc order but sorting
        // makes the test resilient to selector permutations).
        let mut by_id: Vec<(&str, &str, &str)> = records
            .iter()
            .map(|r| match r {
                Record::Entity(e) => (
                    e.entity_id.as_str(),
                    e.kind.as_str(),
                    e.canonical_name.as_str(),
                ),
                other => panic!("expected Entity, got {other:?}"),
            })
            .collect();
        by_id.sort_by_key(|t| t.0);
        assert_eq!(by_id[0], ("driver:adriano_moraes", "driver", "Adriano Moraes"));
        assert_eq!(by_id[1], ("driver:guilherme_marchi", "driver", "Guilherme Marchi"));
        assert_eq!(by_id[2], ("driver:silvano_alves", "driver", "Silvano Alves"));
    }

    #[test]
    fn entity_recipe_with_missing_required_field_fails_content_assembly_sn97_lever_b() {
        // Negative case: a binding that omits canonical_name should
        // surface as a ContentAssembly error pointing at the missing
        // field — same shape Observation's missing-field error takes.
        let html = b"<html><body><div id=\"row\"><span class=\"id\">driver:x</span></div></body></html>";
        let recipe = FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: None,
            plan_id: Uuid::now_v7(),
            source_id: "roster".into(),
            source_url: Url::parse("https://example.com/r").unwrap(),
            extraction: ExtractionSpec::CssSelect {
                selector: ".id".into(),
                attribute: None,
            },
            iterator: None,
            produces: vec![ProductionBinding {
                record_type: RecordType::Entity,
                expectation: ExpectationRef::EntityKind { index: 0 },
                field_mappings: vec![
                    FieldMap {
                        path: "entity_id".into(),
                        source: FieldValueSource::Extracted,
                    },
                    FieldMap {
                        path: "kind".into(),
                        source: FieldValueSource::Literal {
                            value: json!("driver"),
                        },
                    },
                    // canonical_name omitted on purpose.
                ],
                dedup_key_field: None,
            }],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap(),
            authored_by: "xai".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        };
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let err = apply(ctx).unwrap_err();
        match err {
            ApplyError::ContentAssembly { reason } => {
                assert!(
                    reason.contains("canonical_name"),
                    "missing-field error must name canonical_name; got: {reason}"
                );
            }
            other => panic!("expected ContentAssembly, got {other:?}"),
        }
    }

    /// Legacy iterator recipes (no ExtractedInner) still take the
    /// pre-ADR-0019 path: the inner-extractions map stays `None` and
    /// `resolve_field_value` walks the legacy three-variant arms.
    /// This pins backwards compatibility — the
    /// css_select_iterator_produces_n_records test already exercises
    /// the same path, but this name documents the contract
    /// explicitly.
    #[test]
    fn legacy_iterator_recipes_skip_inner_extractions_pass_adr_0019() {
        let html = br#"
            <html><body>
              <div class="card"><h3>Old-shape headline</h3></div>
            </body></html>
        "#;
        let recipe = iterator_event_recipe(".card", "h3");
        let p = iterator_plan();
        let ctx = ApplyContext {
            recipe: &recipe,
            plan: &p,
            bytes: html,
            fetched_at: fetched_at(),
        };
        let records = apply(ctx).expect("legacy iterator path must still apply");
        assert_eq!(records.len(), 1);
        match &records[0] {
            Record::Event(e) => assert_eq!(e.content.headline, "Old-shape headline"),
            other => panic!("got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // validate_recipe_against_bytes — Session 41 items 4–6
    //
    // These tests pin the contract that authoring-time validation
    // mirrors apply-time extraction. Each test pairs a recipe with
    // bytes and asserts the validator's verdict matches what the
    // runtime would return at apply time.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_recipe_pdf_table_against_lithium_fixture_accepts_in_range_coords() {
        // Coordinate the fixture supports (page 2 contains the
        // Country/Production table; row 2 col 1 is "49000" — Chile's
        // production figure). Validator must agree with the runtime
        // that this would extract.
        let recipe = recipe_with(ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 2,
            col: 1,
        });
        validate_recipe_against_bytes(&recipe, LITHIUM_PDF)
            .expect("in-range pdf_table coordinates must validate");
    }

    #[test]
    fn validate_recipe_pdf_table_against_lithium_fixture_rejects_out_of_range_row() {
        // The lithium MCS class of failure from Session 40: LLM
        // counted rows by visual inspection of the page text and
        // authored row=11 against a detected table that has 2 rows.
        // The validator catches this at authoring time so the recipe
        // is never persisted.
        let recipe = recipe_with(ExtractionSpec::PdfTable {
            page: 2,
            table_index: 0,
            row: 99,
            col: 0,
        });
        let err = validate_recipe_against_bytes(&recipe, LITHIUM_PDF).unwrap_err();
        match err {
            ApplyError::Extraction { mode: "pdf_table", reason } => {
                assert!(
                    reason.contains("row 99 out of range"),
                    "validator should surface the runtime's exact error \
                     so the operator sees the same message at authoring \
                     time as they would at apply time; got: {reason}"
                );
            }
            other => panic!(
                "expected Extraction error from pdf_table, got {other:?}"
            ),
        }
    }

    #[test]
    fn validate_recipe_css_select_rejects_selector_that_matches_nothing() {
        // The Session 40 Fed H.4.1 class of failure: LLM authors
        // `table#balance-sheet td.value` against markup that doesn't
        // contain a #balance-sheet table. The validator catches it
        // before the recipe is persisted.
        let html = b"<html><body><table id=\"data\"><tr><td>1</td></tr></table></body></html>";
        let recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: "table#balance-sheet td.value".into(),
            attribute: None,
        });
        match validate_recipe_against_bytes(&recipe, html).unwrap_err() {
            ApplyError::Extraction { mode: "css_select", reason } => {
                assert!(
                    reason.contains("matched no elements"),
                    "validator should report no-match in the runtime's wording; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from css_select, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_css_select_accepts_selector_that_matches() {
        let html = b"<html><body><div class=\"value\">42</div></body></html>";
        let recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: ".value".into(),
            attribute: None,
        });
        validate_recipe_against_bytes(&recipe, html)
            .expect("matching css_select selector must validate");
    }

    #[test]
    fn validate_recipe_css_iterator_accepts_when_outer_and_inner_match() {
        // Iterator path: outer selects cards, inner selects headlines.
        // Both match in the fixture — validator says ok.
        let html = b"\
            <html><body>\
            <div class=\"card\"><h3>First</h3></div>\
            <div class=\"card\"><h3>Second</h3></div>\
            </body></html>";
        let recipe = iterator_event_recipe(".card", "h3");
        validate_recipe_against_bytes(&recipe, html)
            .expect("matching iterator + inner selectors must validate");
    }

    #[test]
    fn validate_recipe_css_iterator_rejects_when_outer_matches_but_inner_does_not() {
        // Outer cards exist; inner targets markup not present inside
        // any card. Validator should reject — the recipe would
        // produce zero records at apply.
        let html = b"\
            <html><body>\
            <div class=\"card\"><span>First</span></div>\
            <div class=\"card\"><span>Second</span></div>\
            </body></html>";
        let recipe = iterator_event_recipe(".card", "h3.headline");
        match validate_recipe_against_bytes(&recipe, html).unwrap_err() {
            ApplyError::Extraction { mode: "css_select", reason } => {
                assert!(
                    reason.contains("inner selector"),
                    "validator should attribute the failure to the inner \
                     selector specifically; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from css_select, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_css_iterator_rejects_when_outer_matches_nothing() {
        let html = b"<html><body><p>no cards here</p></body></html>";
        let recipe = iterator_event_recipe(".card", "h3");
        match validate_recipe_against_bytes(&recipe, html).unwrap_err() {
            ApplyError::Extraction { mode: "css_select", reason } => {
                assert!(
                    reason.contains("iterator selector"),
                    "validator should attribute the failure to the iterator \
                     selector specifically; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from css_select, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // ADR 0019 Phase 2A — validator-side json_path × json_path (Session 67)
    //
    // The runtime's `apply_json_iterator` has supported this pair since
    // Session 61, but `validate_recipe_against_bytes` was missing the
    // matching match-arm — every json_path × json_path attempt was
    // intercepted at authoring with `NotImplemented`. These tests pin
    // the closed gap: the validator now agrees with the runtime, and
    // the runtime-side dispatch is exercised end-to-end by the existing
    // ADR 0019 runtime tests below.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_recipe_json_iterator_accepts_when_outer_and_inner_match() {
        // Array-of-objects API shape — the canonical FEMA / FRED /
        // World-Bank case. Outer path picks each object; inner path
        // picks the headline leaf. Both match in the fixture — the
        // recipe persists.
        let bytes = br#"{
            "items": [
                {"headline": "Storm declared", "id": 1},
                {"headline": "Amendment filed", "id": 2}
            ]
        }"#;
        let recipe = iterator_event_recipe_json("$.items[*]", "$.headline");
        validate_recipe_against_bytes(&recipe, bytes)
            .expect("matching iterator + inner json_paths must validate");
    }

    #[test]
    fn validate_recipe_json_iterator_rejects_when_outer_matches_but_inner_does_not() {
        // Outer items exist; inner targets a key not present in any
        // item. Validator should reject — the recipe would produce
        // zero records at apply, which is exactly the strict Class B
        // shape the FEMA-hunt JSONL surfaced (Session 67 hunt;
        // `$.femaDeclarationString` inner against
        // `/api/open/v2/DisasterDeclarationsSummaries`).
        let bytes = br#"{
            "items": [
                {"id": 1},
                {"id": 2}
            ]
        }"#;
        let recipe = iterator_event_recipe_json("$.items[*]", "$.headline");
        match validate_recipe_against_bytes(&recipe, bytes).unwrap_err() {
            ApplyError::Extraction { mode: "json_path", reason } => {
                assert!(
                    reason.contains("inner path"),
                    "validator should attribute the failure to the inner \
                     path specifically; got: {reason}"
                );
                assert!(
                    reason.contains("matched no nodes"),
                    "validator should re-use the runtime's predicate string \
                     for the strict-Class-B failure_message match; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from json_path, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_json_iterator_rejects_when_outer_matches_nothing() {
        // Outer path targets a key not present at the document root.
        // Validator should reject and attribute the failure to the
        // iterator path so the LLM knows which path to fix.
        let bytes = br#"{"other_root": []}"#;
        let recipe = iterator_event_recipe_json("$.items[*]", "$.headline");
        match validate_recipe_against_bytes(&recipe, bytes).unwrap_err() {
            ApplyError::Extraction { mode: "json_path", reason } => {
                assert!(
                    reason.contains("iterator path"),
                    "validator should attribute the failure to the iterator \
                     path specifically; got: {reason}"
                );
                assert!(
                    reason.contains("matched no nodes"),
                    "predicate string must match the runtime / ADR 0012 \
                     Class B contract; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from json_path, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_json_iterator_rejects_when_inner_matches_only_null() {
        // Outer hits N elements; inner path matches in each element
        // but every match is JSON null. The runtime's
        // `extract_json_within` rejects all-null at apply time
        // (Session 32b's null-skip contract); the validator mirrors
        // that contract so the LLM doesn't author a recipe whose
        // every apply would fail with "all JSON null".
        let bytes = br#"{
            "items": [
                {"headline": null, "id": 1},
                {"headline": null, "id": 2}
            ]
        }"#;
        let recipe = iterator_event_recipe_json("$.items[*]", "$.headline");
        match validate_recipe_against_bytes(&recipe, bytes).unwrap_err() {
            ApplyError::Extraction { mode: "json_path", reason } => {
                assert!(
                    reason.contains("inner path"),
                    "validator should attribute the failure to the inner \
                     path specifically; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from json_path, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_json_iterator_rejects_when_bytes_are_not_json() {
        // The structural validator's mode-pair dispatch is JSON; the
        // runtime parses bytes-as-JSON; bytes that don't parse must
        // produce the "bytes did not parse as JSON" predicate verbatim
        // (the recipe-author prompt teaches that exact text as the
        // category-error signal).
        let html = b"<html><body><p>not json</p></body></html>";
        let recipe = iterator_event_recipe_json("$.items[*]", "$.headline");
        match validate_recipe_against_bytes(&recipe, html).unwrap_err() {
            ApplyError::Extraction { mode: "json_path", reason } => {
                assert!(
                    reason.contains("bytes did not parse as JSON"),
                    "validator must surface the runtime's category-error \
                     predicate; got: {reason}"
                );
            }
            other => panic!("expected Extraction error from json_path, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_unsupported_iterator_pair_message_names_both_supported_pairs() {
        // The fallthrough NotImplemented message must name both
        // supported pairings (css × css and json × json) so the
        // operator and the recipe-author prompt see the same
        // boundary. Pre-Session-67 message only named css × css.
        let mut recipe = iterator_event_recipe(".card", "h3");
        recipe.iterator = Some(ExtractionSpec::JsonPath {
            path: "$.items[*]".into(),
        });
        let bytes = br#"{"items": []}"#;
        match validate_recipe_against_bytes(&recipe, bytes).unwrap_err() {
            ApplyError::NotImplemented { mode, reason } => {
                assert_eq!(mode, "iterator");
                assert!(
                    reason.contains("css_select × css_select"),
                    "message must name css × css; got: {reason}"
                );
                assert!(
                    reason.contains("json_path × json_path"),
                    "message must name json × json post-Session-67; got: {reason}"
                );
            }
            other => panic!("expected NotImplemented for cross-mode iterator pair, got {other:?}"),
        }
    }

    #[test]
    fn validate_recipe_json_path_inherits_runtime_null_skip_contract() {
        // Item 6 lands in the JSON patch (Session 41 patch 3); the
        // dispatch through `extract` already exercises
        // `extract_json_path`'s null-skip and no-match behaviour.
        // This pin documents that wiring up json_path validation will
        // not require new code in `validate_recipe_against_bytes` —
        // the dispatch is mode-agnostic by construction.
        let bytes = br#"{"data": [{"v": 1}, {"v": 2}]}"#;
        let recipe = recipe_with(ExtractionSpec::JsonPath {
            path: "$.data[*].v".into(),
        });
        validate_recipe_against_bytes(&recipe, bytes)
            .expect("matching json_path must validate via the same dispatch");

        let recipe_no_match = recipe_with(ExtractionSpec::JsonPath {
            path: "$.does_not_exist".into(),
        });
        let err = validate_recipe_against_bytes(&recipe_no_match, bytes).unwrap_err();
        assert!(
            matches!(err, ApplyError::Extraction { mode: "json_path", .. }),
            "no-match json_path should surface as Extraction; got {err:?}"
        );
    }

    #[test]
    fn validate_recipe_json_path_rejects_world_bank_leading_null_trap() {
        // Session 41 item 6: end-to-end pin against the World-Bank-
        // shaped null trap that motivated the JSON outline in item 3.
        // The World Bank Open Data API publishes time-series arrays
        // where the most-recent rows carry `"value": null` for
        // unpublished data — a positional index like `$[1][0].value`
        // hits the most recent row, which is null on every fetch
        // forever. The runtime's `extract_json_path` returns an error
        // when every matched node is null (Session 32b); the
        // validator inherits that error through the mode-agnostic
        // dispatch in `validate_recipe_against_bytes` (Session 41
        // patch 1) and converts it to `AuthoringError::Declined` at
        // the recipe-author layer rather than persisting a recipe
        // that would fail on every apply.
        //
        // This test pins the validator's contract for that specific
        // failure shape: a positional-index recipe against the
        // leading-null array must Decline at authoring, with the
        // runtime's null-skip error text intact (it tells the LLM
        // the fix verbatim — write a filter expression — so the
        // retry loop can re-author productively).
        //
        // **Architectural intent.** This test does NOT add any
        // dispatch logic to `validate_recipe_against_bytes`. It is
        // an integration test in the validator-level sense: it
        // exercises the same dispatch path the runtime uses at
        // apply time, against fixture bytes that exhibit the
        // canonical class. End-to-end through `author_recipe`
        // would add no coverage that this test doesn't already
        // provide; the conversion to `AuthoringError::Declined`
        // is structural (the `if let Err(apply_err) = ... return
        // Err(AuthoringError::Declined { reason: format!(...) })`
        // tail of `author_recipe`).
        let bytes = br#"[
            {"page": 1, "per_page": 4, "total": 4},
            [
                {"country": "AUS", "year": "2026", "value": null},
                {"country": "AUS", "year": "2025", "value": null},
                {"country": "AUS", "year": "2024", "value": 88000},
                {"country": "AUS", "year": "2023", "value": 86000}
            ]
        ]"#;

        // Recipe under test: positional index into the inner
        // array's first element. This is the exact authoring
        // mistake the leading-null trap punishes — `$[1][0].value`
        // is `null`, but `$[1][2].value` (the third element) is
        // `88000`.
        let trap_recipe = recipe_with(ExtractionSpec::JsonPath {
            path: "$[1][0].value".into(),
        });
        let err = validate_recipe_against_bytes(&trap_recipe, bytes)
            .expect_err(
                "positional indices into the leading-null array must \
                 fail validation, not silently produce a null record",
            );
        match err {
            ApplyError::Extraction { mode, reason } => {
                assert_eq!(mode, "json_path");
                // The runtime's reason names the failure shape
                // (`null`) and suggests the fix (filter expression).
                // Both are part of the contract the LLM relies on
                // when re-authoring after a Decline — surfacing
                // them through the validator is what closes the
                // loop. See `extract_json_path` lines ~685–690.
                assert!(
                    reason.to_lowercase().contains("null"),
                    "validator failure must name the null-only condition; \
                     got reason: {reason}"
                );
                assert!(
                    reason.contains("filter expression")
                        || reason.contains("?(@."),
                    "validator failure must suggest the filter-expression \
                     fix verbatim so the recipe-author retry loop can \
                     re-author productively; got reason: {reason}"
                );
            }
            other => panic!(
                "expected Extraction error from json_path; got {other:?}"
            ),
        }

        // Sanity check: the canonical fix (a filter-expression path
        // that skips null values) must validate cleanly against the
        // same bytes. Without this assertion the negative test above
        // could pass for the wrong reason — e.g., if the validator
        // were rejecting all json_path recipes — and the LLM's
        // re-authored recipe would still fail to land. Pinning the
        // positive case here keeps the contract two-sided.
        let fixed_recipe = recipe_with(ExtractionSpec::JsonPath {
            path: "$[1][?(@.value)].value".into(),
        });
        validate_recipe_against_bytes(&fixed_recipe, bytes).expect(
            "the filter-expression fix the runtime suggests must validate \
             cleanly; otherwise the retry loop would never land a recipe \
             for World-Bank-shaped sources",
        );
    }

    // -----------------------------------------------------------------------
    // validate_recipe_shape_against_bytes — Session 53 Piece B
    //
    // The 2026-05-09 18:12 lithium re-run hit the apply-stage shape
    // class twice: `pubs.usgs.gov` authored a CSS selector that
    // landed `string "Argentina"` into `ObservationContent.value`
    // (f64); `www.worldbank.org` authored a JSON path that yielded
    // no usable extraction at all, so build_record assembled a
    // content object missing the required `value` field. Both
    // would have been declined at authoring time by the shape
    // validator; both apply-failed forever in the live run.
    //
    // These tests pin the shape validator's contract for both
    // classes against a recipe that uses the test module's
    // `recipe_with` helper (Observation binding, value: f64
    // mapped from Extracted). A passing test is "the validator
    // declines this recipe at authoring time"; a failing test
    // would mean the runtime sees the same shape mismatch at
    // apply time on every fetch.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_shape_accepts_numeric_extraction_for_f64_value_field() {
        // The runtime's `parse_extracted_scalar` parses "49000" as a
        // Number; build_record's deserialization into ObservationContent
        // accepts it. The shape validator must agree — otherwise the
        // happy-path apply on numeric extractions would never author.
        let bytes = b"<html><body><div class=\"value\">49000</div></body></html>";
        let recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: ".value".into(),
            attribute: None,
        });
        validate_recipe_shape_against_bytes(&recipe, bytes, &plan())
            .expect("numeric extracted scalar must shape-validate into f64");
    }

    #[test]
    fn validate_shape_rejects_string_extraction_for_f64_value_field() {
        // The pubs.usgs.gov "Argentina" class. CSS selector matches
        // a country-name cell; parse_extracted_scalar keeps it as a
        // String; ObservationContent's `value: f64` rejects the
        // string. Shape validator must catch this at authoring so
        // the recipe is never persisted.
        let bytes = b"<html><body><div class=\"value\">Argentina</div></body></html>";
        let recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: ".value".into(),
            attribute: None,
        });
        let err = validate_recipe_shape_against_bytes(&recipe, bytes, &plan()).unwrap_err();
        match err {
            ApplyError::ContentAssembly { reason } => {
                assert!(
                    reason.contains("expected f64"),
                    "shape validator must surface the runtime's content-\
                     assembly error verbatim so the recipe-author retry \
                     loop sees the same wording it would at apply time; \
                     got: {reason}"
                );
                assert!(
                    reason.contains("Argentina") || reason.contains("string"),
                    "reason should name the offending value or its type \
                     so the operator's mental model of why the recipe was \
                     declined matches what the runtime would report at \
                     apply time; got: {reason}"
                );
            }
            other => panic!(
                "expected ContentAssembly error from shape validator; got {other:?}"
            ),
        }
    }

    #[test]
    fn validate_shape_rejects_recipe_missing_required_value_field() {
        // The www.worldbank.org "missing field 'value'" class. A
        // recipe whose `produces[0].field_mappings` doesn't include
        // a `value` mapping at all assembles content_json without
        // the key; ObservationContent deserialization rejects.
        // Shape validator catches it at authoring.
        let bytes = b"<html><body><div class=\"v\">42</div></body></html>";

        // Build a recipe whose Observation binding omits the `value`
        // field mapping. All other Observation fields (metric, unit,
        // period) are still mapped — the omission is specifically
        // the f64 slot.
        let mut recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: ".v".into(),
            attribute: None,
        });
        recipe.produces[0].field_mappings.retain(|fm| fm.path != "value");

        let err = validate_recipe_shape_against_bytes(&recipe, bytes, &plan()).unwrap_err();
        match err {
            ApplyError::ContentAssembly { reason } => {
                assert!(
                    reason.contains("missing field"),
                    "shape validator must surface the runtime's missing-\
                     field error so the recipe-author retry loop knows \
                     the binding is shape-incomplete; got: {reason}"
                );
                assert!(
                    reason.contains("value"),
                    "reason should name the specific field that's missing \
                     so the operator can act; got: {reason}"
                );
            }
            other => panic!(
                "expected ContentAssembly error from shape validator; got {other:?}"
            ),
        }
    }

    #[test]
    fn validate_shape_strict_superset_of_extraction_validator() {
        // A recipe that fails the structural validator (selector
        // matches nothing) must also fail the shape validator —
        // there are no bytes to type-check, so the shape validator
        // delegates to the structural validator and returns its
        // error verbatim. This pins the "strict superset" contract
        // documented on validate_recipe_shape_against_bytes.
        let bytes = b"<html><body><p>no value here</p></body></html>";
        let recipe = recipe_with(ExtractionSpec::CssSelect {
            selector: ".value".into(),
            attribute: None,
        });
        let shape_err = validate_recipe_shape_against_bytes(&recipe, bytes, &plan())
            .expect_err("shape validator must inherit structural failures");
        let struct_err = validate_recipe_against_bytes(&recipe, bytes)
            .expect_err("structural validator must reject too");
        assert!(
            matches!(shape_err, ApplyError::Extraction { mode: "css_select", .. }),
            "shape validator must inherit the structural validator's \
             extraction error type; got {shape_err:?}"
        );
        assert!(
            matches!(struct_err, ApplyError::Extraction { mode: "css_select", .. }),
            "sanity: structural validator's error shape unchanged; got {struct_err:?}"
        );
    }
}
