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

use crate::fetch_classes::FetchOutcomeClass;
use crate::research::{DocumentSourceNomination, PriorityTier, ResearchPlan};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// One prior attempt against this nomination. Used as input to the
/// next propose-URL call so the LLM can avoid repeating URLs that
/// already failed.
///
/// **Session 57 / ADR 0017 Piece B:** the `class` field is the
/// closed-vocabulary classification of what went wrong with this
/// attempt. The proposer prompt routes on the class (pivot host vs.
/// pivot URL on same host vs. wait for backoff) without needing
/// to parse the free-text `reason`. The `reason` survives so the
/// LLM has the human-readable detail when the class isn't
/// sufficient (e.g. distinguishing a 404 from a 410 within
/// `UrlShapeMismatch`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorAttempt {
    /// The URL that was tried.
    pub url: String,
    /// Closed-vocabulary class of the failure. Read by the
    /// propose-URL prompt's class-routing rules; see
    /// [`FetchOutcomeClass`] for the variant semantics and
    /// `crates/pipeline/src/fetch_classes.rs` for the host-class
    /// override map.
    pub class: FetchOutcomeClass,
    /// Short human-readable reason — the executor's classification of
    /// what went wrong: `"fetch failed: 404"`, `"recipe author
    /// declined: SPA"`, etc. Travels alongside `class` so the LLM
    /// can read both: the class drives routing, the reason
    /// disambiguates within a class.
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
    // Session 101 Lever 3 — target-aware proposer. The slice carries
    // the distinct record-type buckets that are still unfilled inside
    // the nomination's `author_for_nomination` loop. First attempt:
    // empty slice = no constraint (preserves pre-Sn-101 cache shape).
    // Subsequent attempts: the kinds whose slot(s) declined on prior
    // attempts. The proposer's prompt biases endpoint shape toward
    // serving those kinds — closed-vocab structural patterns only,
    // never host strings. Allowed values: "observation_metric",
    // "event_type", "entity_kind", "relation_kind".
    target_kinds_needed: &[&str],
) -> Result<ProposalOutcome, ProposalError> {
    let user = build_prompt(
        prompt_template,
        plan,
        nomination,
        prior_attempts,
        target_kinds_needed,
    )?;

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
        // Propose-URL shares the provider's default cache shard
        // (per-process `XAI_CONV_ID`). Session 80 reserves per-call
        // cache keys for the extraction path.
        prompt_cache_key: None,
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
    target_kinds_needed: &[&str],
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

    // Session 101 Lever 3 — render the still-unfilled record-type
    // bucket(s) for retry attempts. Empty slice (first attempt) →
    // "(no specific record-type focus)" so the proposer behaves
    // exactly like the pre-Sn-101 target-agnostic path. Non-empty →
    // bullet list naming the closed-vocab kind labels; the prompt
    // prose pairs each kind with structural endpoint shapes that
    // typically serve it.
    let target_kinds = render_target_kinds_needed(target_kinds_needed);

    let out = template
        .replace("{{PLAN_INTERPRETATION}}", interpretation)
        .replace("{{TOPIC_TAGS}}", &topic_tags)
        .replace("{{GEOGRAPHIC_SCOPE}}", &geo_scope)
        .replace("{{HISTORICAL_WINDOW}}", &window)
        .replace("{{NOMINATION_DESCRIPTION}}", description)
        .replace("{{PRIORITY_TIER}}", tier)
        .replace("{{PRIOR_ATTEMPTS}}", &prior)
        .replace("{{TARGET_KINDS_NEEDED}}", &target_kinds);

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

/// Session 101 Lever 3 — render the still-unfilled record-type bucket
/// list for the propose-URL prompt's `{{TARGET_KINDS_NEEDED}}` slot.
///
/// `kinds` carries the distinct closed-vocab record-type labels from
/// the set {"observation_metric", "event_type", "entity_kind",
/// "relation_kind"} that the calling nomination's
/// `author_for_nomination` loop still needs URLs for. The first
/// outer-attempt iteration passes an empty slice (no constraint;
/// target-agnostic shape preserved); subsequent attempts pass the
/// kinds whose slots declined on prior attempts.
///
/// Output shape:
/// - empty slice → `"(no specific record-type focus; propose any URL
///   that satisfies the nomination's description)"`.
/// - non-empty → newline-bulleted list of kind labels with a one-line
///   reminder that the prompt's per-record-type structural-pattern
///   section names endpoint shapes that typically serve each kind.
///
/// Closed-vocab discipline: never names a host; the prompt's
/// structural-pattern section teaches generic endpoint shapes
/// (`/stats/`, `/standings/`, `/roster/`, `/filings/`, etc.) per
/// record_type and lets the LLM bind them to the nomination's
/// description.
fn render_target_kinds_needed(kinds: &[&str]) -> String {
    if kinds.is_empty() {
        return "(no specific record-type focus; propose any URL \
                that satisfies the nomination's description)"
            .to_string();
    }
    // De-dup while preserving order via a small Vec — typical
    // cardinality is 1-4; a HashSet/BTreeSet would be overkill.
    let mut seen: Vec<&str> = Vec::new();
    for k in kinds {
        if !seen.iter().any(|s| s == k) {
            seen.push(*k);
        }
    }
    let bullets = seen
        .iter()
        .map(|k| format!("- `{}`", k))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The following record-type bucket(s) still need URLs after \
         prior attempts on this nomination:\n{bullets}\n\nBias your \
         next URL toward an endpoint shape that the structural-\
         patterns section above names as a typical fit for these \
         kinds. A URL that serves multiple still-unfilled kinds in \
         one fetch is fine; one that serves none of them is a poor \
         use of this attempt."
    )
}

fn render_prior_attempts(attempts: &[PriorAttempt]) -> String {
    if attempts.is_empty() {
        return "(none — this is the first attempt)".to_string();
    }
    // Session 57 / ADR 0017 Piece B: emit the class label on its own
    // line above the reason. The label is the closed-vocabulary key
    // the propose-URL prompt's class-routing rules read; the reason
    // is the free-text disambiguator. Putting class first means the
    // LLM's eye lands on the routing-relevant signal before any
    // detail that might encourage it to reason about the URL string.
    attempts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            format!(
                "{}. URL: `{}`\n   Class: {}\n   Reason: {}",
                i + 1,
                a.url,
                a.class.label(),
                a.reason
            )
        })
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
        let out = build_prompt(template, &plan, &nom, &[], &[]).unwrap();

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
                class: FetchOutcomeClass::UrlShapeMismatch,
                reason: "recipe author declined: JS-rendered SPA".into(),
            },
            PriorAttempt {
                url: "https://www.fao.org/topic/sustainable-fisheries".into(),
                class: FetchOutcomeClass::UrlShapeMismatch,
                reason: "fetch failed: 404".into(),
            },
        ]);
        assert!(s.contains("1."));
        assert!(s.contains("2."));
        assert!(s.contains("topic/166235"));
        assert!(s.contains("404"));
    }

    #[test]
    fn render_prior_attempts_includes_class_label_on_its_own_line() {
        // Session 57 / ADR 0017 Piece B: each attempt renders with a
        // dedicated `Class: <label>` line so the propose-URL prompt
        // can route on the closed-vocabulary class without parsing
        // the free-text reason.
        let s = render_prior_attempts(&[PriorAttempt {
            url: "https://www.example.com/auth-walled".into(),
            class: FetchOutcomeClass::HostRequiresAuth,
            reason: "fetch failed: 401".into(),
        }]);
        assert!(s.contains("Class: host_requires_auth"));
        assert!(s.contains("Reason: fetch failed: 401"));
        // Class line precedes Reason line — the prompt's routing
        // rules read the class first.
        let class_idx = s.find("Class: ").unwrap();
        let reason_idx = s.find("Reason: ").unwrap();
        assert!(class_idx < reason_idx);
    }

    #[test]
    fn render_target_kinds_needed_empty_yields_no_constraint_marker_sn101_lever3() {
        let s = render_target_kinds_needed(&[]);
        assert!(
            s.contains("no specific record-type focus"),
            "empty slice should produce the no-constraint marker; got: {s}"
        );
    }

    #[test]
    fn render_target_kinds_needed_single_kind_lists_it_as_bullet_sn101_lever3() {
        let s = render_target_kinds_needed(&["observation_metric"]);
        assert!(s.contains("- `observation_metric`"));
        assert!(s.contains("still need URLs"));
    }

    #[test]
    fn render_target_kinds_needed_dedupes_repeated_kinds_sn101_lever3() {
        let s = render_target_kinds_needed(&[
            "observation_metric",
            "observation_metric",
            "event_type",
        ]);
        let occ = s.matches("- `observation_metric`").count();
        assert_eq!(
            occ, 1,
            "repeated observation_metric kind should appear once after dedup"
        );
        assert!(s.contains("- `event_type`"));
    }

    #[test]
    fn render_target_kinds_needed_preserves_insertion_order_sn101_lever3() {
        let s = render_target_kinds_needed(&["entity_kind", "observation_metric"]);
        let i_entity = s.find("- `entity_kind`").unwrap();
        let i_obs = s.find("- `observation_metric`").unwrap();
        assert!(
            i_entity < i_obs,
            "insertion order should be preserved in bullet list"
        );
    }

    #[test]
    fn build_prompt_substitutes_target_kinds_needed_placeholder_sn101_lever3() {
        let template = "TARGET_KINDS: {{TARGET_KINDS_NEEDED}}\n\
                        PLAN: {{PLAN_INTERPRETATION}}\n\
                        TAGS: {{TOPIC_TAGS}}\n\
                        SCOPE: {{GEOGRAPHIC_SCOPE}}\n\
                        WINDOW: {{HISTORICAL_WINDOW}}\n\
                        DESC: {{NOMINATION_DESCRIPTION}}\n\
                        TIER: {{PRIORITY_TIER}}\n\
                        PRIOR: {{PRIOR_ATTEMPTS}}";
        let plan = sample_plan();
        let nom = sample_nomination();
        // Empty slice → no-constraint marker present.
        let empty = build_prompt(template, &plan, &nom, &[], &[]).unwrap();
        assert!(empty.contains("no specific record-type focus"));
        // Non-empty slice → kind bullets present.
        let with_kinds =
            build_prompt(template, &plan, &nom, &[], &["event_type", "entity_kind"]).unwrap();
        assert!(with_kinds.contains("- `event_type`"));
        assert!(with_kinds.contains("- `entity_kind`"));
        assert!(with_kinds.contains("still need URLs"));
    }

    #[test]
    fn render_prior_attempts_emits_distinct_class_labels_per_variant() {
        // Walks every variant the proposer could see in the wild and
        // confirms its label appears on the rendered line. Locks in
        // the label-to-prompt-routing-rule binding: a future rename
        // that breaks the binding will fail compilation here.
        let cases = [
            FetchOutcomeClass::HostUnreachable,
            FetchOutcomeClass::HostBlockedByWaf,
            FetchOutcomeClass::HostRequiresAuth,
            FetchOutcomeClass::HostRequiresUaPolicy,
            FetchOutcomeClass::UrlShapeMismatch,
            FetchOutcomeClass::RateLimited,
        ];
        for class in cases {
            let s = render_prior_attempts(&[PriorAttempt {
                url: "https://example.test/".into(),
                class,
                reason: "synthetic test row".into(),
            }]);
            assert!(
                s.contains(&format!("Class: {}", class.label())),
                "class {:?} did not render its label",
                class
            );
        }
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
        let out = build_prompt(template, &plan, &nom, &[], &[]).unwrap();
        assert!(out.contains("(none)"));
        assert!(out.contains("global"));
    }
}
