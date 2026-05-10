//! Level-2 propose-URL step (Session 39).
//!
//! Post-Session-39, the Level-1 classifier no longer emits URLs. The
//! plan describes sources by `description` + `priority_tier` only;
//! URL discovery moves to a dedicated propose-URL step that the
//! fetch executor invokes inside its retry loop.
//!
//! The retry loop in [`crate::fetch_executor::author_one`] calls
//! [`propose_source_url`] up to 3 times per nomination, each time
//! passing the prior attempts (URLs already tried + the reason each
//! one failed). The LLM either commits to a fresh URL or declines
//! with an empty `url` and a rationale; the executor surfaces the
//! decline as `RecipeOutcome::Declined`.
//!
//! ## Why a separate prompt
//!
//! The recipe-author prompt asks the LLM to write a deterministic
//! recipe given fetched bytes. That's a heavier, schema-dense task
//! that benefits from the Workhorse tier. Picking a URL given a
//! description is a simpler task that the Cheap tier handles fine —
//! and decoupling the steps means the recipe author never sees URL
//! discovery as part of its job, preserving the clean "given bytes,
//! write recipe" contract.
//!
//! ## Decline path
//!
//! An empty `url` field in the response is the decline signal. The
//! `rationale` becomes the user-visible decline reason on the
//! exhausted-attempt path. Mirrors the recipe-author Track B decline
//! shape (Session 28, ADR 0007 amendment 4) — same idiom, same wire
//! convention.
//!
//! ## Security
//!
//! Every proposed URL is validated through [`UrlGuard::check`] before
//! being returned: scheme allowlist, private-IP rejection, embedded-
//! credentials rejection, port allowlist, and the 2048-byte length
//! cap. An LLM proposing `file:///etc/passwd` or
//! `http://169.254.169.254/...` is treated as a hard error, not
//! propagated to the caller as a valid URL.

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use situation_room_llm::{
    CompletionRequest, LlmError, LlmProvider, ModelTier, ReasoningEffort,
};
use situation_room_secure::bounds::{check_string, Bounds};
use situation_room_secure::url_guard::{UrlGuard, UrlViolation};
use thiserror::Error;
use url::Url;

use crate::research::{DocumentSourceNomination, PriorityTier, ResearchPlan};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// One prior attempt against this nomination. Used as input to the
/// next propose-URL call so the LLM can avoid repeating URLs that
/// already failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorAttempt {
    /// The URL that was tried.
    pub url: String,
    /// Short human-readable reason — the executor's classification of
    /// what went wrong: `"fetch failed: 404"`, `"recipe author
    /// declined: SPA"`, etc.
    pub reason: String,
}

/// Outcome of one [`propose_source_url`] call.
#[derive(Debug, Clone)]
pub enum ProposalOutcome {
    /// LLM committed to a URL. URL has already passed `UrlGuard`.
    Url {
        url: Url,
        /// LLM's rationale for the choice. Surfaces in fetch-run
        /// logs and can be threaded to the recipe author for context.
        rationale: String,
    },
    /// LLM declined to propose another URL. The retry loop surfaces
    /// this as `RecipeOutcome::Declined` with the rationale as the
    /// decline reason.
    Declined {
        /// LLM's verbatim decline reason — what was tried, what's
        /// exhausted, why none of it would work.
        reason: String,
    },
}

/// Errors that can arise during URL proposal. Mirrors the shape of
/// [`crate::recipe_author::AuthoringError`] for consistency at the
/// call site, minus the variants that don't apply (no recipe
/// validation here).
#[derive(Debug, Error)]
pub enum ProposalError {
    #[error("llm call failed: {0}")]
    Llm(#[from] LlmError),

    #[error("llm returned no structured output (schema ignored?)")]
    NoStructuredOutput,

    #[error("llm output failed to deserialize: {0}")]
    OutputParse(String),

    #[error("proposed url rejected: {0}")]
    BadUrl(#[from] UrlViolation),

    #[error("prompt construction failed: {0}")]
    Prompt(String),
}

/// Run one propose-URL LLM call.
///
/// Returns either a validated [`Url`] the executor can fetch, or a
/// `Declined` outcome the executor should surface to the operator.
/// Hard errors (network failures, schema misses, validation rejects)
/// bubble up as [`ProposalError`].
///
/// `prior_attempts` is the per-nomination history accumulated across
/// earlier propose → fetch → author cycles in the same retry loop.
/// Empty on the first attempt; populated on subsequent attempts.
///
/// `effort_override` is the Session 53 Piece F escalation knob.
/// `None` (the normal path) means "use the provider's per-tier
/// mapping for `tier`" — Low on xAI's default cheap mapping.
/// `Some(e)` pins the request body's effort to `e` for this call
/// only, regardless of tier. The fetch executor passes
/// `Some(Medium)` when a nomination has been declined ≥3 times
/// across the plan's runs; the escalation is per-nomination, not
/// per-source, and feeds runtime observations back into the
/// reasoning budget. See `ReasoningEffort`'s doc-comment for the
/// principle (per-tier and per-runtime-feedback are fine; per-
/// source name routing is forbidden).
pub async fn propose_source_url(
    provider: &dyn LlmProvider,
    tier: ModelTier,
    prompt_template: &str,
    plan: &ResearchPlan,
    nomination: &DocumentSourceNomination,
    prior_attempts: &[PriorAttempt],
    effort_override: Option<ReasoningEffort>,
) -> Result<ProposalOutcome, ProposalError> {
    let user = build_prompt(prompt_template, plan, nomination, prior_attempts)?;

    let schema = schema_for!(ProposedUrl);
    let schema_value = serde_json::to_value(&schema)
        .map_err(|e| ProposalError::Prompt(format!("schema serialization: {e}")))?;

    let req = CompletionRequest {
        system: Some(
            "You are a URL proposer for situation_room. Output only JSON conforming \
             to the provided schema. No prose outside the JSON."
                .to_string(),
        ),
        user,
        schema: Some(situation_room_llm::providers::StructuredOutputSchema {
            name: "ProposedUrl".to_string(),
            schema: schema_value,
        }),
        max_tokens: 1024,
        // Zero temperature: URL proposal is selection from training
        // knowledge, not generation. Same discipline as recipe
        // authoring.
        temperature: 0.0,
        // Session 53 Piece F: per-call effort override for stuck
        // nominations. `None` falls through to the provider's per-
        // tier mapping (cheap → Low on xAI by default — propose-URL
        // is the canonical cheap-tier call). `Some(e)` pins the
        // body field for this call. The escalation decision is
        // per-nomination and observation-driven (decline count from
        // fetch_run_outcomes); it is NOT per-host, per-publisher,
        // or per-URL-string — the closed-vocabulary discipline
        // forbids those, and ReasoningEffort's doc-comment names
        // the failure mode.
        reasoning_effort: effort_override,
    };

    let resp = provider.complete(tier, req).await?;
    let raw = resp.structured.ok_or(ProposalError::NoStructuredOutput)?;
    let output: ProposedUrl = serde_json::from_value(raw)
        .map_err(|e| ProposalError::OutputParse(e.to_string()))?;

    interpret_proposed_url(output)
}

// ---------------------------------------------------------------------------
// LLM-facing schema
// ---------------------------------------------------------------------------

/// What the LLM returns from one propose-URL call.
///
/// Empty `url` is the decline signal. The wire shape uses
/// empty-string-as-absent rather than `Option<String>` because xAI's
/// structured-output schema rejects top-level `Option<T>` for some
/// shapes — same idiom as `RecipeAuthoringOutput::decline_reason`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProposedUrl {
    /// The URL the LLM commits to as the next attempt. Empty string
    /// means "decline — I have nothing more to propose."
    pub url: String,

    /// Why this URL fits the description (commit case), or what's
    /// exhausted (decline case).
    pub rationale: String,
}

// ---------------------------------------------------------------------------
// Prompt building
// ---------------------------------------------------------------------------

/// Substitute the propose-URL template's placeholders with values
/// from the plan, the nomination, and the prior-attempts history.
///
/// Pure string formatting; no IO. The `{{PLACEHOLDERS}}`:
/// - `{{PLAN_INTERPRETATION}}` — plan's interpretation paragraph
/// - `{{TOPIC_TAGS}}` — newline-bullet list of topic tags
/// - `{{GEOGRAPHIC_SCOPE}}` — newline-bullet list of `code (display)`
/// - `{{HISTORICAL_WINDOW}}` — `<N> days`
/// - `{{NOMINATION_DESCRIPTION}}` — nomination's description verbatim
/// - `{{PRIORITY_TIER}}` — snake_case tier name
/// - `{{PRIOR_ATTEMPTS}}` — newline-bullet list, or `(none — this is
///   the first attempt)` when empty
pub fn build_prompt(
    template: &str,
    plan: &ResearchPlan,
    nomination: &DocumentSourceNomination,
    prior_attempts: &[PriorAttempt],
) -> Result<String, ProposalError> {
    let interpretation = plan.interpretation.as_str();

    let topic_tags = if plan.topic_tags.is_empty() {
        "(none)".to_string()
    } else {
        plan.topic_tags
            .iter()
            .map(|t| format!("- `{}`", t.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let geo_scope = if plan.geographic_scope.is_empty() {
        "(global / no specific scope)".to_string()
    } else {
        plan.geographic_scope
            .iter()
            .map(|g| {
                if g.display.is_empty() {
                    format!("- `{}`", g.code)
                } else {
                    format!("- `{}` ({})", g.code, g.display)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let window = format!("{} days", plan.historical_window_days);

    let description = nomination.description.as_str();
    let tier = priority_tier_to_str(nomination.priority_tier);

    let prior = render_prior_attempts(prior_attempts);

    let out = template
        .replace("{{PLAN_INTERPRETATION}}", interpretation)
        .replace("{{TOPIC_TAGS}}", &topic_tags)
        .replace("{{GEOGRAPHIC_SCOPE}}", &geo_scope)
        .replace("{{HISTORICAL_WINDOW}}", &window)
        .replace("{{NOMINATION_DESCRIPTION}}", description)
        .replace("{{PRIORITY_TIER}}", tier)
        .replace("{{PRIOR_ATTEMPTS}}", &prior);

    // Bound the assembled prompt so an unusually long description
    // can't blow LLM_PROMPT_BODY downstream.
    check_string(
        "propose_source_url prompt body",
        &out,
        Bounds::LLM_PROMPT_BODY,
    )
    .map_err(|e| ProposalError::Prompt(format!("{e}")))?;

    Ok(out)
}

fn priority_tier_to_str(t: PriorityTier) -> &'static str {
    match t {
        PriorityTier::AuthoritativePrimary => "authoritative_primary",
        PriorityTier::AuthoritativeSecondary => "authoritative_secondary",
        PriorityTier::IndustryTradePress => "industry_trade_press",
        PriorityTier::GeneralNews => "general_news",
    }
}

fn render_prior_attempts(attempts: &[PriorAttempt]) -> String {
    if attempts.is_empty() {
        return "(none — this is the first attempt)".to_string();
    }
    attempts
        .iter()
        .enumerate()
        .map(|(i, a)| format!("{}. URL: `{}`\n   Reason: {}", i + 1, a.url, a.reason))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Response interpretation + validation
// ---------------------------------------------------------------------------

fn interpret_proposed_url(output: ProposedUrl) -> Result<ProposalOutcome, ProposalError> {
    let trimmed = output.url.trim();

    // Empty URL = decline. The rationale is the decline reason.
    if trimmed.is_empty() {
        let reason = output.rationale.trim().to_string();
        let reason = if reason.is_empty() {
            "url proposer declined without a reason".to_string()
        } else {
            reason
        };
        return Ok(ProposalOutcome::Declined { reason });
    }

    // URL discipline. UrlGuard handles scheme, private-IP,
    // embedded-credentials, port allowlist, length cap. Single-point
    // discipline — the same UrlGuard the (now-removed) classifier URL
    // validation used.
    let guard = UrlGuard::new();
    guard.check(trimmed)?;

    // Parse after guard accepts — guard validates the byte string;
    // url::Url::parse builds the typed value the executor uses.
    let url = Url::parse(trimmed).map_err(|e| {
        // UrlGuard accepted this string but url::Url::parse rejected
        // it. Should not happen — UrlGuard parses internally as part
        // of its checks — but surface defensively as a parse violation.
        ProposalError::BadUrl(UrlViolation::Parse(format!(
            "guard accepted but url::Url::parse rejected: {e}"
        )))
    })?;

    Ok(ProposalOutcome::Url {
        url,
        rationale: output.rationale,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{
        DocumentSourceNomination, GeoScope, PriorityTier, RecordExpectations, ResearchPlan,
    };
    use chrono::Utc;
    use situation_room_core::vocab::Topic;
    use uuid::Uuid;

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "lithium supply chain".into(),
            interpretation:
                "Lithium supply chain — global production, reserves, and refining capacity."
                    .into(),
            topic_tags: vec![Topic::new("lithium").unwrap()],
            geographic_scope: vec![GeoScope {
                code: "AU".into(),
                display: "Australia".into(),
            }],
            historical_window_days: 730,
            expectations: RecordExpectations::default(),
            created_at: Utc::now(),
        }
    }

    fn sample_nomination() -> DocumentSourceNomination {
        DocumentSourceNomination {
            nomination_id: Uuid::now_v7(),
            description: "USGS Mineral Commodity Summaries — annual lithium chapter".into(),
            priority_tier: PriorityTier::AuthoritativePrimary,
        }
    }

    #[test]
    fn build_prompt_substitutes_all_placeholders() {
        let template = "\
            INTERPRETATION: {{PLAN_INTERPRETATION}}\n\
            TAGS:\n{{TOPIC_TAGS}}\n\
            SCOPE:\n{{GEOGRAPHIC_SCOPE}}\n\
            WINDOW: {{HISTORICAL_WINDOW}}\n\
            DESC: {{NOMINATION_DESCRIPTION}}\n\
            TIER: {{PRIORITY_TIER}}\n\
            PRIOR:\n{{PRIOR_ATTEMPTS}}\n\
        ";
        let plan = sample_plan();
        let nom = sample_nomination();
        let out = build_prompt(template, &plan, &nom, &[]).unwrap();

        for ph in [
            "{{PLAN_INTERPRETATION}}",
            "{{TOPIC_TAGS}}",
            "{{GEOGRAPHIC_SCOPE}}",
            "{{HISTORICAL_WINDOW}}",
            "{{NOMINATION_DESCRIPTION}}",
            "{{PRIORITY_TIER}}",
            "{{PRIOR_ATTEMPTS}}",
        ] {
            assert!(!out.contains(ph), "placeholder {ph} not substituted");
        }
        assert!(out.contains("USGS"));
        assert!(out.contains("authoritative_primary"));
        assert!(out.contains("730 days"));
        assert!(out.contains("first attempt"));
    }

    #[test]
    fn render_prior_attempts_empty_signals_first_attempt() {
        let s = render_prior_attempts(&[]);
        assert!(s.contains("first attempt"));
    }

    #[test]
    fn render_prior_attempts_non_empty_lists_url_and_reason() {
        let s = render_prior_attempts(&[
            PriorAttempt {
                url: "https://www.fao.org/fishery/en/topic/166235".into(),
                reason: "recipe author declined: JS-rendered SPA".into(),
            },
            PriorAttempt {
                url: "https://www.fao.org/topic/sustainable-fisheries".into(),
                reason: "fetch failed: 404".into(),
            },
        ]);
        assert!(s.contains("1."));
        assert!(s.contains("2."));
        assert!(s.contains("topic/166235"));
        assert!(s.contains("404"));
    }

    #[test]
    fn interpret_proposed_url_empty_string_is_decline() {
        let out = ProposedUrl {
            url: "".into(),
            rationale: "exhausted FAO endpoints I know".into(),
        };
        match interpret_proposed_url(out).unwrap() {
            ProposalOutcome::Declined { reason } => {
                assert!(reason.contains("FAO"));
            }
            _ => panic!("empty url must produce Declined"),
        }
    }

    #[test]
    fn interpret_proposed_url_whitespace_is_decline() {
        let out = ProposedUrl {
            url: "   ".into(),
            rationale: "nothing else".into(),
        };
        assert!(matches!(
            interpret_proposed_url(out).unwrap(),
            ProposalOutcome::Declined { .. }
        ));
    }

    #[test]
    fn interpret_proposed_url_decline_without_reason_gets_default() {
        let out = ProposedUrl {
            url: "".into(),
            rationale: "".into(),
        };
        match interpret_proposed_url(out).unwrap() {
            ProposalOutcome::Declined { reason } => {
                assert!(reason.contains("without a reason"));
            }
            _ => panic!("empty url must produce Declined"),
        }
    }

    #[test]
    fn interpret_proposed_url_valid_https_passes() {
        let out = ProposedUrl {
            url: "https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf".into(),
            rationale: "USGS lithium chapter PDF".into(),
        };
        match interpret_proposed_url(out).unwrap() {
            ProposalOutcome::Url { url, .. } => {
                assert_eq!(url.scheme(), "https");
                assert_eq!(url.host_str(), Some("pubs.usgs.gov"));
            }
            _ => panic!("expected Url"),
        }
    }

    #[test]
    fn interpret_proposed_url_file_scheme_rejected_by_guard() {
        let out = ProposedUrl {
            url: "file:///etc/passwd".into(),
            rationale: "should be blocked".into(),
        };
        let err = interpret_proposed_url(out).unwrap_err();
        assert!(matches!(err, ProposalError::BadUrl(_)));
    }

    #[test]
    fn interpret_proposed_url_metadata_ip_rejected_by_guard() {
        let out = ProposedUrl {
            url: "http://169.254.169.254/latest/meta-data/".into(),
            rationale: "should be blocked".into(),
        };
        let err = interpret_proposed_url(out).unwrap_err();
        assert!(matches!(err, ProposalError::BadUrl(_)));
    }

    #[test]
    fn priority_tier_to_str_covers_all_variants() {
        assert_eq!(
            priority_tier_to_str(PriorityTier::AuthoritativePrimary),
            "authoritative_primary"
        );
        assert_eq!(
            priority_tier_to_str(PriorityTier::AuthoritativeSecondary),
            "authoritative_secondary"
        );
        assert_eq!(
            priority_tier_to_str(PriorityTier::IndustryTradePress),
            "industry_trade_press"
        );
        assert_eq!(
            priority_tier_to_str(PriorityTier::GeneralNews),
            "general_news"
        );
    }

    #[test]
    fn build_prompt_handles_empty_topic_tags_and_scope() {
        let template = "TAGS: {{TOPIC_TAGS}}\nSCOPE: {{GEOGRAPHIC_SCOPE}}\n";
        let mut plan = sample_plan();
        plan.topic_tags = vec![];
        plan.geographic_scope = vec![];
        let nom = sample_nomination();
        let out = build_prompt(template, &plan, &nom, &[]).unwrap();
        assert!(out.contains("(none)"));
        assert!(out.contains("global"));
    }
}
