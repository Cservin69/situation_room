//! Recipe authoring — Level 2 of the research function (ADR 0007).
//!
//! This module asks an LLM to produce a [`FetchRecipe`] given:
//! - a [`ResearchPlan`] (Level 1 output) describing what to research,
//! - a brief source context (id, sample URL),
//! - a document excerpt so the LLM can see the source's current shape.
//!
//! The LLM runs **once**. The resulting recipe is then applied
//! deterministically by the runtime (Phase 3c.3, forthcoming) for
//! every subsequent fetch. This is the architectural commitment that
//! Session 2's deleted `parse.rs` tried to shortcut around. See the
//! "READ THIS FIRST" section of `STOCKPILE_HANDOFF_SESSION2.md`.
//!
//! ## What this module guarantees
//!
//! - The LLM is called through a `&dyn LlmProvider`, not a concrete
//!   provider. Swapping xAI → Anthropic requires no change here.
//! - The LLM's JSON output is constrained by a schema generated via
//!   `schemars` from [`RecipeAuthoringOutput`]. The LLM cannot return
//!   shapes the runtime wouldn't understand.
//! - The returned URL is validated through
//!   [`stockpile_secure::UrlGuard`] before the recipe is returned. An
//!   LLM hallucinating `file:///etc/passwd` doesn't leave this
//!   module.
//! - Structural sanity checks (≥1 binding, variant-specific bounds)
//!   catch obviously-broken outputs before they reach storage.
//!
//! ## What this module does NOT do
//!
//! - Validate that the chosen coordinates are *correct*. A wrong
//!   `pdf_table` row index produces plausible garbage; only human
//!   review catches that. This module catches *malformed* output,
//!   not *wrong* output.
//! - Test the recipe by applying it. That's the apply runtime's
//!   job; we return a recipe, and the caller decides whether to
//!   dry-run it before persisting.
//! - Store the recipe. Persistence happens in the caller (the demo
//!   binary in 3c.4; a dedicated recipes table lands there too).

use chrono::Utc;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stockpile_llm::{
    CompletionRequest, LlmError, LlmProvider, ModelTier,
};
use stockpile_secure::bounds::{check_string, Bounds};
use stockpile_secure::url_guard::{UrlGuard, UrlViolation};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::recipes::{
    ExpectationRef, ExtractionSpec, FetchRecipe, FieldMap, FieldValueSource,
    ProductionBinding, RowFilter,
};
use crate::research::ResearchPlan;
use stockpile_core::RecordType;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Context about the source being authored against.
///
/// The LLM reads this plus a document excerpt plus the research plan,
/// and produces a recipe. The excerpt is deliberately a snapshot of
/// the source's *current* shape — the LLM's job is to pick stable
/// coordinates, not to memorize values.
#[derive(Debug, Clone)]
pub struct AuthoringContext {
    /// Stable source identifier (e.g. `"usgs_mcs:2024:lithium"`).
    pub source_id: String,

    /// The URL the runtime will fetch. The LLM typically echoes this
    /// back as the recipe's `source_url`; we validate either way.
    pub sample_url: Url,

    /// Document content the LLM should reason over. Typically the
    /// extracted text of a recent fetch. Bounded by
    /// [`Bounds::LLM_PROMPT_BODY`] — callers must truncate before
    /// passing. An excerpt that blows the bound is rejected early
    /// rather than silently truncated.
    pub document_excerpt: String,
}

/// Errors that can arise during recipe authoring.
#[derive(Debug, Error)]
pub enum AuthoringError {
    #[error("llm call failed: {0}")]
    Llm(#[from] LlmError),

    #[error("llm returned no structured output (schema ignored?)")]
    NoStructuredOutput,

    #[error("llm output failed to deserialize: {0}")]
    OutputParse(String),

    #[error("recipe url rejected: {0}")]
    BadUrl(#[from] UrlViolation),

    #[error("recipe structural validation failed: {0}")]
    InvalidRecipe(String),

    #[error("prompt construction failed: {0}")]
    Prompt(String),
}

/// Assemble the user-message prompt from a template + runtime inputs.
///
/// The template string must contain `{{PLAN_JSON}}`, `{{SOURCE_ID}}`,
/// `{{SOURCE_URL}}`, and `{{DOCUMENT_EXCERPT}}` placeholders. Missing
/// placeholders are not errors — they're assumed to be intentional
/// omissions by the prompt author.
///
/// Pure function (no I/O, no LLM call) so tests can assert the
/// rendered prompt contains the expected markers without hitting a
/// network.
pub fn build_prompt(
    template: &str,
    plan: &ResearchPlan,
    ctx: &AuthoringContext,
) -> Result<String, AuthoringError> {
    check_string(
        "llm_prompt_user",
        &ctx.document_excerpt,
        Bounds::LLM_PROMPT_BODY,
    )
    .map_err(|e| AuthoringError::Prompt(e.to_string()))?;

    let plan_json = serde_json::to_string_pretty(plan)
        .map_err(|e| AuthoringError::Prompt(format!("plan serialization: {e}")))?;

    let out = template
        .replace("{{PLAN_JSON}}", &plan_json)
        .replace("{{SOURCE_ID}}", &ctx.source_id)
        .replace("{{SOURCE_URL}}", ctx.sample_url.as_str())
        .replace("{{DOCUMENT_EXCERPT}}", &ctx.document_excerpt);

    // The assembled prompt can be larger than the individual parts
    // (template text + inputs). Enforce the overall bound so we fail
    // fast rather than at the provider.
    check_string("llm_prompt_user", &out, Bounds::LLM_PROMPT_BODY)
        .map_err(|e| AuthoringError::Prompt(e.to_string()))?;

    Ok(out)
}

/// Author a [`FetchRecipe`] by calling the LLM once with the given
/// prompt template and context.
///
/// The prompt template is passed as a string so callers control how
/// they load it (from disk, embedded in the binary, a test literal).
/// The pipeline crate deliberately doesn't reach into the filesystem.
pub async fn author_recipe(
    provider: &dyn LlmProvider,
    tier: ModelTier,
    prompt_template: &str,
    plan: &ResearchPlan,
    ctx: &AuthoringContext,
) -> Result<FetchRecipe, AuthoringError> {
    let user = build_prompt(prompt_template, plan, ctx)?;

    // Schema derived from RecipeAuthoringOutput — the LLM cannot
    // return shapes the runtime wouldn't understand.
    let schema = schema_for!(RecipeAuthoringOutput);
    let schema_value = serde_json::to_value(&schema)
        .map_err(|e| AuthoringError::Prompt(format!("schema serialization: {e}")))?;

    let req = CompletionRequest {
        system: Some(
            "You are a recipe author for Stockpile. Output only JSON conforming \
             to the provided schema. No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(stockpile_llm::providers::StructuredOutputSchema {
            name: "RecipeAuthoringOutput".to_string(),
            schema: schema_value,
        }),
        max_tokens: 4096,
        // Zero temperature: recipe authoring is extraction, not generation.
        temperature: 0.0,
    };

    let fingerprint = provider.id().to_string(); // stable provider id; key fingerprint
                                                  // surfaces via the provider's own logging
    let resp = provider.complete(tier, req).await?;

    let raw = resp.structured.ok_or(AuthoringError::NoStructuredOutput)?;
    let output: RecipeAuthoringOutput = serde_json::from_value(raw)
        .map_err(|e| AuthoringError::OutputParse(e.to_string()))?;

    build_validated_recipe(output, plan, &fingerprint)
}

// ---------------------------------------------------------------------------
// Authoring output shape — what the LLM returns
// ---------------------------------------------------------------------------

/// Subset of [`FetchRecipe`] that the LLM is responsible for producing.
///
/// Server-assigned fields (`id`, `authored_at`, `authored_by`,
/// `version`, `plan_id`, `dedup_key`) are stamped by
/// [`build_validated_recipe`]. The LLM never sees these and never
/// invents them.
///
/// Serde representation matches the corresponding fields of
/// [`FetchRecipe`] exactly.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecipeAuthoringOutput {
    /// HTTPS URL the runtime will fetch. Parsed + URL-guarded
    /// server-side; the LLM just returns a string.
    pub source_url: String,

    /// Extraction coordinate set.
    pub extraction: AuthoredExtractionSpec,

    /// What records to produce. Must have length ≥ 1.
    pub produces: Vec<AuthoredProductionBinding>,
}

/// Mirror of [`ExtractionSpec`] with `JsonSchema` derived.
///
/// A parallel type exists because the real [`ExtractionSpec`] is used
/// elsewhere in the codebase without a `schemars` dep; duplicating
/// the shape here contains the derive to the authoring path. The two
/// must serde-match — the `extraction_spec_mirror_matches` test
/// guards that contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AuthoredExtractionSpec {
    JsonPath {
        path: String,
    },
    CssSelect {
        selector: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribute: Option<String>,
    },
    CsvCell {
        column: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_filter: Option<AuthoredRowFilter>,
    },
    PdfTable {
        page: u32,
        table_index: u32,
        row: u32,
        col: u32,
    },
    RegexCapture {
        pattern: String,
        group: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthoredRowFilter {
    Equals {
        column: String,
        value: String,
    },
    LabeledAs {
        label_column: String,
        label: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredProductionBinding {
    pub record_type: AuthoredRecordType,
    pub expectation: AuthoredExpectationRef,
    pub field_mappings: Vec<AuthoredFieldMap>,
}

/// The three record types a recipe may produce at authoring time.
///
/// `Document` and `Entity` are excluded because they come from other
/// paths (ingest and registry lookup respectively). `Assertion` is
/// excluded because it carries a `claimant` and `stance` that a
/// recipe's `field_mappings` don't populate — assertions are the
/// LLM extraction layer's job (ADR 0004, ADR 0007). If we later
/// want recipe-shaped assertions we'll need a separate binding
/// shape for them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredRecordType {
    Observation,
    Event,
    Relation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "list", rename_all = "snake_case")]
pub enum AuthoredExpectationRef {
    ObservationMetric { index: u32 },
    EventType { index: u32 },
    EntityKind { index: u32 },
    RelationKind { index: u32 },
    DocumentSource { index: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthoredFieldMap {
    pub path: String,
    pub source: AuthoredFieldValueSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthoredFieldValueSource {
    Extracted,
    Literal { value: Value },
    FromPlan { pointer: String },
}

// ---------------------------------------------------------------------------
// Validation + conversion: AuthoringOutput -> FetchRecipe
// ---------------------------------------------------------------------------

/// Maximum reasonable number of bindings per recipe. Anything beyond
/// this is a sign of a mis-scoped recipe; split into multiple.
const MAX_BINDINGS: usize = 20;
/// Maximum reasonable number of field mappings per binding.
const MAX_FIELD_MAPPINGS_PER_BINDING: usize = 50;

fn build_validated_recipe(
    output: RecipeAuthoringOutput,
    plan: &ResearchPlan,
    authored_by: &str,
) -> Result<FetchRecipe, AuthoringError> {
    // 1. URL: parse + URL-guard.
    let source_url = {
        let guard = UrlGuard::new();
        guard.check(&output.source_url)?
    };

    // 2. Extraction spec: structural bounds.
    let extraction = convert_extraction(output.extraction)?;

    // 3. Produces: non-empty, bounded.
    if output.produces.is_empty() {
        return Err(AuthoringError::InvalidRecipe(
            "recipe must contain at least one production binding".into(),
        ));
    }
    if output.produces.len() > MAX_BINDINGS {
        return Err(AuthoringError::InvalidRecipe(format!(
            "recipe has {} bindings, exceeds limit of {MAX_BINDINGS}",
            output.produces.len()
        )));
    }

    // 4. Validate each binding against the plan's expectations.
    let mut produces = Vec::with_capacity(output.produces.len());
    for binding in output.produces {
        produces.push(convert_binding(binding, plan)?);
    }

    // 5. Reject recipes that target the same expectation twice.
    // Two bindings against the same expectation is almost always a
    // mistake; split into separate recipes if truly intended.
    for (i, a) in produces.iter().enumerate() {
        for b in &produces[i + 1..] {
            if a.expectation == b.expectation {
                return Err(AuthoringError::InvalidRecipe(format!(
                    "two bindings target the same expectation {:?}",
                    a.expectation
                )));
            }
        }
    }

    Ok(FetchRecipe {
        id: Uuid::now_v7(),
        dedup_key: None, // caller sets this — convention is
        // `{plan_id}:{source_id}:{binding_tag}`.
        plan_id: plan.id,
        source_id: String::new(), // set by caller from registry
        source_url,
        extraction,
        produces,
        authored_at: Utc::now(),
        authored_by: authored_by.to_string(),
        version: 1,
    })
}

fn convert_extraction(
    spec: AuthoredExtractionSpec,
) -> Result<ExtractionSpec, AuthoringError> {
    Ok(match spec {
        AuthoredExtractionSpec::JsonPath { path } => {
            if path.is_empty() {
                return Err(AuthoringError::InvalidRecipe("empty jsonpath".into()));
            }
            ExtractionSpec::JsonPath { path }
        }
        AuthoredExtractionSpec::CssSelect {
            selector,
            attribute,
        } => {
            if selector.is_empty() {
                return Err(AuthoringError::InvalidRecipe("empty css selector".into()));
            }
            ExtractionSpec::CssSelect {
                selector,
                attribute,
            }
        }
        AuthoredExtractionSpec::CsvCell { column, row_filter } => {
            if column.is_empty() {
                return Err(AuthoringError::InvalidRecipe(
                    "empty csv column name".into(),
                ));
            }
            ExtractionSpec::CsvCell {
                column,
                row_filter: row_filter.map(convert_row_filter),
            }
        }
        AuthoredExtractionSpec::PdfTable {
            page,
            table_index,
            row,
            col,
        } => {
            if page == 0 {
                return Err(AuthoringError::InvalidRecipe(
                    "pdf_table page must be >= 1 (1-indexed)".into(),
                ));
            }
            ExtractionSpec::PdfTable {
                page,
                table_index,
                row,
                col,
            }
        }
        AuthoredExtractionSpec::RegexCapture { pattern, group } => {
            if pattern.is_empty() {
                return Err(AuthoringError::InvalidRecipe(
                    "empty regex pattern".into(),
                ));
            }
            if group == 0 {
                return Err(AuthoringError::InvalidRecipe(
                    "regex capture group must be >= 1 (1-indexed)".into(),
                ));
            }
            ExtractionSpec::RegexCapture { pattern, group }
        }
    })
}

fn convert_row_filter(rf: AuthoredRowFilter) -> RowFilter {
    match rf {
        AuthoredRowFilter::Equals { column, value } => RowFilter::Equals { column, value },
        AuthoredRowFilter::LabeledAs {
            label_column,
            label,
        } => RowFilter::LabeledAs {
            label_column,
            label,
        },
    }
}

fn convert_binding(
    b: AuthoredProductionBinding,
    plan: &ResearchPlan,
) -> Result<ProductionBinding, AuthoringError> {
    if b.field_mappings.is_empty() {
        return Err(AuthoringError::InvalidRecipe(
            "binding has no field mappings".into(),
        ));
    }
    if b.field_mappings.len() > MAX_FIELD_MAPPINGS_PER_BINDING {
        return Err(AuthoringError::InvalidRecipe(format!(
            "binding has {} field mappings, exceeds limit of {MAX_FIELD_MAPPINGS_PER_BINDING}",
            b.field_mappings.len()
        )));
    }

    let expectation = convert_expectation_ref(b.expectation, plan)?;

    let field_mappings = b
        .field_mappings
        .into_iter()
        .map(convert_field_map)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProductionBinding {
        record_type: match b.record_type {
            AuthoredRecordType::Observation => RecordType::Observation,
            AuthoredRecordType::Event => RecordType::Event,
            AuthoredRecordType::Relation => RecordType::Relation,
        },
        expectation,
        field_mappings,
    })
}

fn convert_expectation_ref(
    er: AuthoredExpectationRef,
    plan: &ResearchPlan,
) -> Result<ExpectationRef, AuthoringError> {
    // Bounds-check the index against the plan — catches hallucinated
    // references to expectations the plan doesn't have.
    let check_index = |list_len: usize, index: u32, list_name: &str| -> Result<(), AuthoringError> {
        if (index as usize) >= list_len {
            return Err(AuthoringError::InvalidRecipe(format!(
                "expectation reference {list_name}[{index}] but plan has only {list_len} entries"
            )));
        }
        Ok(())
    };

    match er {
        AuthoredExpectationRef::ObservationMetric { index } => {
            check_index(
                plan.expectations.observation_metrics.len(),
                index,
                "observation_metrics",
            )?;
            Ok(ExpectationRef::ObservationMetric { index })
        }
        AuthoredExpectationRef::EventType { index } => {
            check_index(plan.expectations.event_types.len(), index, "event_types")?;
            Ok(ExpectationRef::EventType { index })
        }
        AuthoredExpectationRef::EntityKind { index } => {
            check_index(plan.expectations.entity_kinds.len(), index, "entity_kinds")?;
            Ok(ExpectationRef::EntityKind { index })
        }
        AuthoredExpectationRef::RelationKind { index } => {
            check_index(
                plan.expectations.relation_kinds.len(),
                index,
                "relation_kinds",
            )?;
            Ok(ExpectationRef::RelationKind { index })
        }
        AuthoredExpectationRef::DocumentSource { index } => {
            check_index(
                plan.expectations.document_sources.len(),
                index,
                "document_sources",
            )?;
            Ok(ExpectationRef::DocumentSource { index })
        }
    }
}

fn convert_field_map(fm: AuthoredFieldMap) -> Result<FieldMap, AuthoringError> {
    if fm.path.is_empty() {
        return Err(AuthoringError::InvalidRecipe("empty field path".into()));
    }
    let source = match fm.source {
        AuthoredFieldValueSource::Extracted => FieldValueSource::Extracted,
        AuthoredFieldValueSource::Literal { value } => FieldValueSource::Literal { value },
        AuthoredFieldValueSource::FromPlan { pointer } => {
            if pointer.is_empty() {
                return Err(AuthoringError::InvalidRecipe(
                    "from_plan pointer must not be empty".into(),
                ));
            }
            FieldValueSource::FromPlan { pointer }
        }
    };
    Ok(FieldMap {
        path: fm.path,
        source,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{
        DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, GeoScope,
        MetricExpectation, RecordExpectations, RelationKindExpectation,
    };
    use chrono::{TimeZone, Utc};
    use stockpile_core::vocab::{EntityId, EventType, Topic, Unit};

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium production".into(),
            interpretation: "Research on global lithium production, reserves, and trade.".into(),
            topic_tags: vec![Topic::new("Li").unwrap()],
            geographic_scope: vec![GeoScope::code_only("AU"), GeoScope::code_only("CL")],
            historical_window_days: 730,
            expectations: RecordExpectations {
                observation_metrics: vec![
                    MetricExpectation {
                        name: "production".into(),
                        unit_hint: Some(Unit::new("t").unwrap()),
                        rationale: "Primary volume metric".into(),
                    },
                    MetricExpectation {
                        name: "reserves".into(),
                        unit_hint: Some(Unit::new("t").unwrap()),
                        rationale: "Stock metric".into(),
                    },
                ],
                event_types: vec![EventTypeExpectation {
                    event_type: EventType::new("mine_opened").unwrap(),
                    rationale: "Capacity expansion signal".into(),
                }],
                entity_kinds: vec![EntityKindExpectation {
                    kind: "mine".into(),
                    exemplars: vec![EntityId::new("mine:greenbushes").unwrap()],
                    rationale: "Atomic unit of supply".into(),
                }],
                relation_kinds: vec![RelationKindExpectation {
                    kind: "operator_of".into(),
                    rationale: "Operator-asset link".into(),
                }],
                document_sources: vec![DocumentSourceHint {
                    description: "USGS Mineral Commodity Summaries".into(),
                    preferred_source_ids: vec!["usgs_mcs".into()],
                }],
                assertion_guidance: None,
            },
            created_at: Utc.with_ymd_and_hms(2026, 4, 22, 0, 0, 0).unwrap(),
        }
    }

    fn sample_context() -> AuthoringContext {
        AuthoringContext {
            source_id: "usgs_mcs:2024:lithium".into(),
            sample_url: Url::parse(
                "https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf",
            )
            .unwrap(),
            document_excerpt: "Lithium\n\nProduction: Australia 88,000 tonnes, Chile 49,000 tonnes."
                .into(),
        }
    }

    fn good_output() -> RecipeAuthoringOutput {
        RecipeAuthoringOutput {
            source_url: "https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf"
                .into(),
            extraction: AuthoredExtractionSpec::PdfTable {
                page: 2,
                table_index: 0,
                row: 3,
                col: 1,
            },
            produces: vec![AuthoredProductionBinding {
                record_type: AuthoredRecordType::Observation,
                expectation: AuthoredExpectationRef::ObservationMetric { index: 0 },
                field_mappings: vec![
                    AuthoredFieldMap {
                        path: "value".into(),
                        source: AuthoredFieldValueSource::Extracted,
                    },
                    AuthoredFieldMap {
                        path: "unit".into(),
                        source: AuthoredFieldValueSource::Literal {
                            value: serde_json::json!("t"),
                        },
                    },
                    AuthoredFieldMap {
                        path: "metric".into(),
                        source: AuthoredFieldValueSource::FromPlan {
                            pointer: "expectations.observation_metrics.0.name".into(),
                        },
                    },
                ],
            }],
        }
    }

    // -----------------------------------------------------------------------
    // Prompt construction
    // -----------------------------------------------------------------------

    #[test]
    fn build_prompt_substitutes_all_placeholders() {
        let template = "\
            plan: {{PLAN_JSON}}\n\
            id: {{SOURCE_ID}}\n\
            url: {{SOURCE_URL}}\n\
            excerpt: {{DOCUMENT_EXCERPT}}\n\
        ";
        let out = build_prompt(template, &sample_plan(), &sample_context()).unwrap();

        assert!(!out.contains("{{PLAN_JSON}}"), "plan placeholder left");
        assert!(!out.contains("{{SOURCE_ID}}"), "source id placeholder left");
        assert!(!out.contains("{{SOURCE_URL}}"), "url placeholder left");
        assert!(!out.contains("{{DOCUMENT_EXCERPT}}"), "excerpt placeholder left");

        assert!(out.contains("usgs_mcs:2024:lithium"));
        assert!(out.contains("mcs2024-lithium.pdf"));
        assert!(out.contains("Australia 88,000 tonnes"));
        // plan is embedded as JSON — topic should appear
        assert!(out.contains("\"topic\""));
        assert!(out.contains("lithium production"));
    }

    #[test]
    fn build_prompt_rejects_oversized_excerpt() {
        let mut ctx = sample_context();
        ctx.document_excerpt = "x".repeat(Bounds::LLM_PROMPT_BODY + 1);
        let err = build_prompt("x{{DOCUMENT_EXCERPT}}y", &sample_plan(), &ctx).unwrap_err();
        assert!(matches!(err, AuthoringError::Prompt(_)), "got {err:?}");
    }

    // -----------------------------------------------------------------------
    // Mirror-shape contract: AuthoredX must serde-match X.
    //
    // If these fail after someone edits ExtractionSpec, the LLM schema has
    // drifted from the runtime's apply spec. The test is the canary.
    // -----------------------------------------------------------------------

    #[test]
    fn authored_extraction_spec_mirror_matches_runtime() {
        let cases = [
            (
                AuthoredExtractionSpec::JsonPath { path: "$.a".into() },
                ExtractionSpec::JsonPath { path: "$.a".into() },
            ),
            (
                AuthoredExtractionSpec::CssSelect {
                    selector: "td.v".into(),
                    attribute: None,
                },
                ExtractionSpec::CssSelect {
                    selector: "td.v".into(),
                    attribute: None,
                },
            ),
            (
                AuthoredExtractionSpec::PdfTable {
                    page: 1,
                    table_index: 0,
                    row: 2,
                    col: 3,
                },
                ExtractionSpec::PdfTable {
                    page: 1,
                    table_index: 0,
                    row: 2,
                    col: 3,
                },
            ),
        ];
        for (authored, runtime) in cases {
            let a = serde_json::to_value(&authored).unwrap();
            let r = serde_json::to_value(&runtime).unwrap();
            assert_eq!(
                a, r,
                "authored spec and runtime spec serialize differently: {authored:?} vs {runtime:?}"
            );
        }
    }

    #[test]
    fn authored_row_filter_mirror_matches_runtime() {
        let a = AuthoredRowFilter::Equals {
            column: "c".into(),
            value: "v".into(),
        };
        let r = RowFilter::Equals {
            column: "c".into(),
            value: "v".into(),
        };
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&r).unwrap()
        );
    }

    #[test]
    fn authored_expectation_ref_mirror_matches_runtime() {
        let a = AuthoredExpectationRef::ObservationMetric { index: 2 };
        let r = ExpectationRef::ObservationMetric { index: 2 };
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(r).unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // Validation: happy path
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_recipe_accepts_good_output() {
        let recipe = build_validated_recipe(good_output(), &sample_plan(), "xai").unwrap();
        assert_eq!(recipe.authored_by, "xai");
        assert_eq!(recipe.version, 1);
        assert_eq!(recipe.produces.len(), 1);
        assert!(matches!(
            recipe.extraction,
            ExtractionSpec::PdfTable { page: 2, .. }
        ));
        // UUIDv7 is the only identity form we accept.
        assert_eq!(recipe.id.get_version_num(), 7);
    }

    /// Regression: `FetchRecipe::plan_id` must equal `ResearchPlan::id`.
    /// Before Session 4 this was a placeholder (`Uuid::now_v7()`)
    /// because `ResearchPlan` carried no id; the consequence was that
    /// the same logical recipe authored twice produced different
    /// `dedup_key`s (`{plan_id}:{source_id}:{tag}`) and never deduped.
    #[test]
    fn build_validated_recipe_threads_plan_id() {
        let plan = sample_plan();
        let recipe = build_validated_recipe(good_output(), &plan, "xai").unwrap();
        assert_eq!(recipe.plan_id, plan.id);
    }

    // -----------------------------------------------------------------------
    // Validation: URL rejection
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_recipe_rejects_non_https_url() {
        let mut out = good_output();
        out.source_url = "file:///etc/passwd".into();
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::BadUrl(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_recipe_rejects_private_ip_url() {
        let mut out = good_output();
        out.source_url = "http://127.0.0.1/".into();
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::BadUrl(_)), "got {err:?}");
    }

    // -----------------------------------------------------------------------
    // Validation: structural
    // -----------------------------------------------------------------------

    #[test]
    fn build_validated_recipe_rejects_empty_produces() {
        let mut out = good_output();
        out.produces = vec![];
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::InvalidRecipe(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_recipe_rejects_binding_with_no_field_mappings() {
        let mut out = good_output();
        out.produces[0].field_mappings = vec![];
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no field mappings"), "got {msg}");
    }

    #[test]
    fn build_validated_recipe_rejects_duplicate_expectation_bindings() {
        let mut out = good_output();
        // Two bindings targeting observation_metrics[0].
        out.produces.push(out.produces[0].clone());
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("same expectation"), "got {msg}");
    }

    #[test]
    fn build_validated_recipe_rejects_expectation_index_out_of_range() {
        let mut out = good_output();
        out.produces[0].expectation = AuthoredExpectationRef::ObservationMetric { index: 99 };
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("but plan has only"), "got {msg}");
    }

    #[test]
    fn build_validated_recipe_rejects_pdf_table_page_zero() {
        let mut out = good_output();
        out.extraction = AuthoredExtractionSpec::PdfTable {
            page: 0,
            table_index: 0,
            row: 0,
            col: 0,
        };
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::InvalidRecipe(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_recipe_rejects_regex_group_zero() {
        let mut out = good_output();
        out.extraction = AuthoredExtractionSpec::RegexCapture {
            pattern: "x".into(),
            group: 0,
        };
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::InvalidRecipe(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_recipe_rejects_empty_css_selector() {
        let mut out = good_output();
        out.extraction = AuthoredExtractionSpec::CssSelect {
            selector: "".into(),
            attribute: None,
        };
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        assert!(matches!(err, AuthoringError::InvalidRecipe(_)), "got {err:?}");
    }

    #[test]
    fn build_validated_recipe_rejects_too_many_bindings() {
        let mut out = good_output();
        let one = out.produces[0].clone();
        for i in 1..=MAX_BINDINGS as u32 {
            // Use a different expectation index each time so the
            // duplicate-expectation rule isn't what fails.
            let mut b = one.clone();
            // Past the plan's 2 metrics, vary by event_types (only 1),
            // entity_kinds (only 1), relation_kinds (1), document_sources (1).
            // To keep this test focused on the binding-count rule, we
            // assert the error message rather than its variant.
            b.expectation = AuthoredExpectationRef::ObservationMetric { index: i };
            out.produces.push(b);
        }
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        let msg = format!("{err}");
        // Either "exceeds limit" (count rule fires first) or
        // "but plan has only" (index rule fires first) — both are
        // correct rejections. The test is that we reject, not which
        // message wins the race.
        assert!(
            msg.contains("exceeds limit") || msg.contains("but plan has only"),
            "got {msg}"
        );
    }

    #[test]
    fn build_validated_recipe_rejects_empty_field_path() {
        let mut out = good_output();
        out.produces[0].field_mappings[0].path = "".into();
        let err = build_validated_recipe(out, &sample_plan(), "xai").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("empty field path"), "got {msg}");
    }

    // -----------------------------------------------------------------------
    // Schema generation sanity
    // -----------------------------------------------------------------------

    #[test]
    fn schema_for_authoring_output_is_producible() {
        // schemars::schema_for! is compile-time, but the produced
        // Schema must also serialize to JSON. This is what gets sent
        // to the LLM as its structured-output constraint.
        let schema = schema_for!(RecipeAuthoringOutput);
        let json = serde_json::to_value(&schema).expect("schema must serialize");

        // Spot-check: the schema should mention the top-level fields.
        let s = json.to_string();
        assert!(s.contains("source_url"), "schema missing source_url");
        assert!(s.contains("extraction"), "schema missing extraction");
        assert!(s.contains("produces"), "schema missing produces");
        // And the closed set of extraction modes.
        assert!(s.contains("json_path"));
        assert!(s.contains("css_select"));
        assert!(s.contains("csv_cell"));
        assert!(s.contains("pdf_table"));
        assert!(s.contains("regex_capture"));
    }

    // -----------------------------------------------------------------------
    // Live LLM test — structural assertions only.
    //
    // The handoff is emphatic about this: LLM output is non-deterministic,
    // so test assertions must be structural (recipe parses, URL guard
    // passes, >=1 binding), not exact-match.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore]
    async fn live_author_recipe_against_xai_produces_valid_recipe() {
        use stockpile_llm::XaiProvider;
        use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};

        let _ = dotenvy::dotenv();
        let http = SecureHttpClient::new(SecureHttpConfig::default()).unwrap();
        let Some(provider) = XaiProvider::from_env(http) else {
            panic!("XAI_API_KEY not set in environment or .env — cannot run live test");
        };

        // A minimal prompt that matches the production template's
        // placeholders. This is test-scoped so it stays in sync with
        // the schema even if the real prompt file evolves.
        let template = "\
            You are a recipe author. Produce a FetchRecipe for:\n\
            PLAN: {{PLAN_JSON}}\n\
            SOURCE: {{SOURCE_ID}} at {{SOURCE_URL}}\n\
            EXCERPT:\n{{DOCUMENT_EXCERPT}}\n\
            Return JSON matching the schema. Use mode \"pdf_table\" if the \
            excerpt appears to be from a PDF. Target observation_metrics[0].\
        ";

        let plan = sample_plan();
        let ctx = sample_context();

        let recipe =
            author_recipe(&provider, ModelTier::Workhorse, template, &plan, &ctx)
                .await
                .expect("live recipe authoring should succeed");

        // Structural assertions only.
        assert!(!recipe.produces.is_empty(), "recipe must have >=1 binding");
        assert_eq!(recipe.version, 1);
        assert_eq!(recipe.authored_by, "xai");
        // The URL passed UrlGuard by virtue of reaching this point.
    }
}
