//! Document → Assertion extraction (Phase 3, Session 77).
//!
//! Thin orchestrator over `llm::extraction`. The fetch executor
//! calls [`extract_and_persist_assertions`] once per persisted
//! Document (Session 69 synth), gated to article-kind Documents
//! with non-empty body. The orchestrator:
//!
//!   1. Calls `llm::extraction::extract_assertions_from_document`
//!      with the Document's body, MIME, source URL, and the plan's
//!      topic for grounding.
//!   2. Wraps each returned [`AssertionDraft`] in an `Assertion`
//!      envelope provenanced to the recipe (same shape
//!      `recipe_apply` uses, so `records_for_plan`'s LIKE join
//!      routes the row into the originating plan's dashboard).
//!   3. Persists each `Assertion` via `Store::insert_assertion`.
//!
//! ## Scope (v1)
//!
//! - **Only relation-shaped output.** The LLM extractor emits a
//!   subject-predicate-object triple shape; v1 wraps each as
//!   `AssertedContent::Relation`. Observation / Event /
//!   EntityAttribute variants land in future sessions.
//! - **No retry on extraction failure.** A failed LLM call or
//!   parse error warn-logs and returns `Ok(0)`. Documents are
//!   numerous; one failed extraction doesn't justify retrying.
//! - **Persistence failures don't fail the recipe.** If
//!   `insert_assertion` fails on one item, the loop logs and
//!   continues — matches the posture of `record_apply_failure_attempt`
//!   and `insert_fetch_document` (auxiliary persistence is best-effort).
//!
//! ## What this module does NOT do
//!
//! - Decide which Documents to extract from. The fetch executor
//!   gates on MIME / kind before calling this module (article-kind
//!   with non-empty body, see [`should_extract_from`]). This keeps
//!   LLM-cost surface bounded.
//! - Dedup across re-fetches. Each fetch produces a fresh
//!   Document row (Session 69 design); each extraction produces a
//!   fresh batch of Assertion rows. The pipeline's promote stage
//!   (future work) will dedup at the cross-source consensus layer.

use chrono::{DateTime, Utc};
use situation_room_core::schema::content::{
    AssertedContent, EntityAttributeContent, EventContent, ObservationContent, RelationContent,
};
use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
use situation_room_core::schema::records::{Assertion, Event, Observation};
use situation_room_core::vocab::Confidence;
use situation_room_llm::{
    extract_assertions_from_document, extract_entity_attributes_from_document,
    extract_events_from_document, extract_observations_from_document, AssertionDraft,
    EntityAttributeDraft, EventDraft, ExtractionConfig, LlmProvider, ObservationDraft,
};
use situation_room_storage::Store;
use tracing::{info, warn};

use crate::document_synth;
use crate::recipes::FetchRecipe;
use crate::research::ResearchPlan;

/// Summary of one extraction pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionReport {
    /// Assertions emitted by the LLM and validated.
    pub extracted: u32,
    /// Of those, how many made it to storage.
    pub persisted: u32,
    /// Per-assertion insert failures (warn-logged in-band).
    pub insert_failures: u32,
    /// `Some(_)` when the LLM call or response parse failed
    /// outright. The runtime path warn-logs and continues; this
    /// field is for callers that want the failure visible without
    /// scraping logs.
    pub call_error: Option<String>,
}

/// Decide whether to run extraction on this Document. Gated to
/// article-kind Documents with non-empty body — JSON / CSV / PDF
/// don't carry the prose shape the v1 extractor expects, and an
/// empty body is the binary-MIME signal from `document_synth`
/// (PDF, images, octet-stream all produce empty bodies).
///
/// **Closed-vocabulary discipline.** The predicate keys off `mime`
/// (open vocabulary) and `body_len > 0` — no host strings, no
/// source-id heuristics. Document `kind` is downstream of MIME
/// (see `document_synth::document_kind_from_mime`); we match the
/// same boundary so a single MIME edit moves both routes in
/// lockstep.
pub fn should_extract_from(mime: &str, body_len: usize) -> bool {
    body_len > 0 && document_synth::is_html_mime(mime)
}

/// Per-Document extraction entry point. Called by each
/// `run_X_recipe` in `fetch_executor.rs` immediately after the
/// Session-69 `insert_fetch_document` call, with the same
/// `(plan, recipe, bytes, response_content_type, fetched_at)`
/// inputs.
///
/// Returns an [`ExtractionReport`] for observability; the caller
/// ignores it today (the operator-visible signal is the dashboard
/// Assertions panel lighting up). Errors are absorbed into the
/// report; this function never returns `Err` so it can't break
/// the runtime path even on LLM outage.
pub async fn extract_and_persist_assertions(
    store: &Store,
    provider: &dyn LlmProvider,
    extraction_prompt: &str,
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) -> ExtractionReport {
    let mut report = ExtractionReport::default();

    let mime = response_content_type.map(normalise_mime).unwrap_or_default();
    if !should_extract_from(&mime, bytes.len()) {
        // Not an article (or empty body). Silent skip — this is the
        // common case for JSON/CSV/PDF feeds, not a failure.
        return report;
    }

    // Build the body preview the same way document_synth does so
    // the extractor sees what the dashboard shows. `body_preview`
    // is pub-via-module — we route through `document_synth`'s
    // public surface.
    let body = document_synth::body_preview_for_mime(&mime, bytes);
    if body.trim().is_empty() {
        // HTML strip produced no usable prose (script-only page,
        // pure CSS, etc.). Skip.
        return report;
    }

    let cfg = ExtractionConfig::default();
    let topic = plan.topic.as_str();
    let source_url = recipe.source_url.as_str();

    // Session 80 — closed-vocab predicate gate. Walk the plan's
    // `relation_kinds[].kind` and hand the slice to the extractor. An
    // empty Vec means "the plan declared no relation kinds, accept
    // whatever the LLM emits" (Session 77's open-vocab behaviour). A
    // non-empty Vec turns the schema + validator into a closed-vocab
    // gate matching the event + observation extractor posture.
    let allowed_owned: Vec<String> = plan
        .expectations
        .relation_kinds
        .iter()
        .map(|r| r.kind.clone())
        .collect();
    let allowed_refs: Vec<&str> = allowed_owned.iter().map(|s| s.as_str()).collect();

    let drafts = match extract_assertions_from_document(
        provider,
        &cfg,
        extraction_prompt,
        topic,
        source_url,
        &mime,
        &body,
        &allowed_refs,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(
                recipe_id = %recipe.id,
                error = %e,
                "document extraction LLM call failed; skipping this Document's assertions"
            );
            report.call_error = Some(e.to_string());
            return report;
        }
    };

    report.extracted = drafts.len() as u32;
    if drafts.is_empty() {
        info!(
            recipe_id = %recipe.id,
            "document extraction returned no assertions (empty list is a legal outcome)"
        );
        return report;
    }

    for draft in drafts {
        let assertion = build_assertion(plan, recipe, &draft, fetched_at);
        match store.insert_assertion(&assertion) {
            Ok(()) => report.persisted += 1,
            Err(e) => {
                report.insert_failures += 1;
                warn!(
                    recipe_id = %recipe.id,
                    assertion_id = %assertion.id,
                    error = %e,
                    "failed to persist extracted Assertion; continuing with the rest of the batch"
                );
            }
        }
    }

    info!(
        recipe_id = %recipe.id,
        extracted = report.extracted,
        persisted = report.persisted,
        insert_failures = report.insert_failures,
        "document extraction complete"
    );

    report
}

/// Build one [`Assertion`] from a validated [`AssertionDraft`].
/// Pure function — no I/O — so tests can pin the envelope shape
/// without standing up a provider or DB.
///
/// `source_id` follows the same `{source}#recipe:{id}@v{ver}`
/// format `recipe_apply::build_record` and `document_synth`
/// produce, so `records_for_plan`'s LIKE join surfaces the
/// assertion under the originating plan.
pub fn build_assertion(
    plan: &ResearchPlan,
    recipe: &FetchRecipe,
    draft: &AssertionDraft,
    fetched_at: DateTime<Utc>,
) -> Assertion {
    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            recipe.source_id, recipe.id, recipe.version
        ),
        source_url: Some(recipe.source_url.to_string()),
        source_published_at: None,
        license: "extracted".into(),
        derived_from: vec![],
        selector_path: None,
        raw_bytes_excerpt: None,
    };

    let subjects = Subjects {
        // Both endpoints surface as subject entities so the
        // cross-record entity join surfaces the assertion alongside
        // any Entity rows for the same actors.
        entities: vec![draft.from.clone(), draft.to.clone()],
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: None,
        observed_at: fetched_at,
        // Confidence on the envelope is the orchestrator's combined
        // confidence; we use the draft's confidence directly so the
        // claimant's stated certainty survives.
        confidence: draft.confidence,
    };

    let content = AssertedContent::Relation(RelationContent {
        kind: draft.kind.clone(),
        from: draft.from.clone(),
        to: draft.to.clone(),
        magnitude: None,
        valid_until: None,
    });

    Assertion::new(draft.claimant.clone(), draft.stance, content, envelope)
}

/// Normalise a `Content-Type` header value the same way
/// `document_synth::normalise_mime` does — lowercase, strip
/// parameters. Kept local rather than re-using the (private)
/// helper in document_synth to avoid widening that module's
/// public surface for a one-line duplication.
fn normalise_mime(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return "application/octet-stream".to_string();
    }
    s.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Per-Document Event extraction orchestrator (Session 78)
// ---------------------------------------------------------------------------
//
// Sibling to `extract_and_persist_assertions`. The fetch executor
// calls this once per persisted Document, gated identically
// (article-kind + non-empty body). The orchestrator:
//
//   1. Reads the plan's declared `event_types[].event_type` list and
//      hands it to the LLM extractor as the closed-vocab gate.
//   2. Calls `llm::extraction::extract_events_from_document`. If the
//      plan declared no event kinds, the LLM call is skipped entirely
//      (the extractor short-circuits before touching the provider) —
//      so plans-without-events don't burn workhorse tokens.
//   3. Wraps each returned [`EventDraft`] in an `Event` envelope
//      provenanced to the recipe (same source_id shape `recipe_apply`
//      uses, so `records_for_plan`'s LIKE join surfaces the Event
//      under the originating plan's dashboard).
//   4. Persists each `Event` via `Store::insert_event`.
//
// ## Scope (v1)
//
// - **Strict closed-vocabulary.** The extractor only emits event
//   types in `plan.expectations.event_types[].event_type` — out-of-
//   vocab kinds are dropped at the LLM layer and counted via warn
//   logs there. The runtime path here trusts that filter and just
//   persists whatever the validator approved.
// - **No retry.** Same posture as the relation extractor: a failed
//   LLM call or parse error warn-logs and returns an empty report.
// - **Persistence failures don't fail the recipe.** Per-event insert
//   failures warn-log and the loop continues.
//
// ## What this orchestrator does NOT do
//
// - Dedup events across re-fetches. Each fetch produces a fresh
//   batch of Event rows; the promote stage (future work) will dedup
//   at the cross-source consensus layer the same way it will for
//   Assertion rows.
// - Fall through to the relation extractor or vice versa. The two
//   are independent LLM calls so a regression in one doesn't take
//   the other down.

/// Per-Document event extraction entry point. Called by each
/// `run_X_recipe` in `fetch_executor.rs` after the assertion
/// extraction call, with the same inputs.
///
/// Returns an [`ExtractionReport`] for observability; the caller
/// ignores it today (the operator-visible signal is the dashboard
/// Events panel ticking up). Errors are absorbed into the report;
/// this function never returns `Err`.
pub async fn extract_and_persist_events(
    store: &Store,
    provider: &dyn LlmProvider,
    extraction_prompt: &str,
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) -> ExtractionReport {
    let mut report = ExtractionReport::default();

    let mime = response_content_type.map(normalise_mime).unwrap_or_default();
    if !should_extract_from(&mime, bytes.len()) {
        return report;
    }

    // Collect the plan's declared event kinds. Empty list short-
    // circuits the LLM call inside `extract_events_from_document` so
    // we don't burn workhorse tokens on plans that don't track events.
    let allowed_owned: Vec<String> = plan
        .expectations
        .event_types
        .iter()
        .map(|e| e.event_type.as_str().to_string())
        .collect();
    if allowed_owned.is_empty() {
        return report;
    }
    let allowed_refs: Vec<&str> = allowed_owned.iter().map(|s| s.as_str()).collect();

    let body = document_synth::body_preview_for_mime(&mime, bytes);
    if body.trim().is_empty() {
        return report;
    }

    let cfg = ExtractionConfig::default();
    let topic = plan.topic.as_str();
    let source_url = recipe.source_url.as_str();

    let drafts = match extract_events_from_document(
        provider,
        &cfg,
        extraction_prompt,
        topic,
        source_url,
        &mime,
        &body,
        &allowed_refs,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(
                recipe_id = %recipe.id,
                error = %e,
                "document event extraction LLM call failed; skipping this Document's events"
            );
            report.call_error = Some(e.to_string());
            return report;
        }
    };

    report.extracted = drafts.len() as u32;
    if drafts.is_empty() {
        info!(
            recipe_id = %recipe.id,
            "document event extraction returned no events (empty list is a legal outcome)"
        );
        return report;
    }

    for draft in drafts {
        let event = build_event(plan, recipe, &draft, fetched_at);
        match store.insert_event(&event) {
            Ok(()) => report.persisted += 1,
            Err(e) => {
                report.insert_failures += 1;
                warn!(
                    recipe_id = %recipe.id,
                    event_id = %event.id,
                    error = %e,
                    "failed to persist extracted Event; continuing with the rest of the batch"
                );
            }
        }
    }

    info!(
        recipe_id = %recipe.id,
        extracted = report.extracted,
        persisted = report.persisted,
        insert_failures = report.insert_failures,
        "document event extraction complete"
    );

    report
}

/// Build one [`Event`] from a validated [`EventDraft`]. Pure
/// function — no I/O — so tests can pin the envelope shape.
///
/// `source_id` follows the same `{source}#recipe:{id}@v{ver}`
/// format as `build_assertion` / `recipe_apply::build_record`, so
/// `records_for_plan`'s LIKE join surfaces the Event under the
/// originating plan.
///
/// When the LLM extracted a `when` timestamp, it lands on
/// `envelope.valid_at` — the dashboard's event-feed timeline
/// renders against valid_at. When `when` is `None`, valid_at stays
/// `None` and the timeline falls back to `observed_at` for
/// ordering (the timestamp the document was fetched).
pub fn build_event(
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    draft: &EventDraft,
    fetched_at: DateTime<Utc>,
) -> Event {
    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            recipe.source_id, recipe.id, recipe.version
        ),
        source_url: Some(recipe.source_url.to_string()),
        source_published_at: None,
        license: "extracted".into(),
        derived_from: vec![],
        selector_path: None,
        raw_bytes_excerpt: None,
    };

    let subjects = Subjects {
        entities: draft.actors.clone(),
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: draft.when,
        observed_at: fetched_at,
        confidence: draft.confidence,
    };

    let content = EventContent {
        event_type: draft.event_type.clone(),
        headline: draft.headline.clone(),
        actors: draft.actors.clone(),
        direction: draft.direction,
        magnitude: None,
        geometry: None,
    };

    Event::new(envelope, content)
}

// ---------------------------------------------------------------------------
// Per-Document Observation extraction orchestrator (Session 79)
// ---------------------------------------------------------------------------
//
// Third sibling to `extract_and_persist_assertions` (Session 77) and
// `extract_and_persist_events` (Session 78). The fetch executor calls
// this once per persisted Document, gated identically to the other
// two (article-kind + non-empty body). The orchestrator:
//
//   1. Reads the plan's declared `observation_metrics[].name` list and
//      hands it to the LLM extractor as the closed-vocab gate.
//   2. Calls `llm::extraction::extract_observations_from_document`. If
//      the plan declared no metrics, the LLM call is skipped entirely
//      (the extractor short-circuits before touching the provider) —
//      so plans-without-observations don't burn workhorse tokens.
//   3. Wraps each returned [`ObservationDraft`] in an `Observation`
//      envelope provenanced to the recipe (same source_id shape
//      `recipe_apply` and `build_assertion` use, so
//      `records_for_plan`'s LIKE join surfaces the Observation under
//      the originating plan's dashboard).
//   4. Persists each `Observation` via `Store::insert_observation`.
//
// ## Scope (v1)
//
// - **Strict closed-vocabulary.** The extractor only emits metrics
//   in `plan.expectations.observation_metrics[].name` — out-of-vocab
//   names are dropped at the LLM layer and counted via warn logs
//   there. The runtime path here trusts that filter.
// - **No retry.** Same posture as the two earlier extractors: a
//   failed LLM call or parse error warn-logs and returns an empty
//   report.
// - **Persistence failures don't fail the recipe.** Per-observation
//   insert failures warn-log and the loop continues.
//
// ## What this orchestrator does NOT do
//
// - Dedup observations across re-fetches. Each fetch produces a
//   fresh batch of Observation rows; the promote stage (future work)
//   will dedup at the cross-source consensus layer.
// - Fall through to the assertion or event extractor or vice versa.
//   The three are independent LLM calls so a regression in one
//   doesn't take the others down.
// - Carry the `MetricExpectation.unit_hint` through to the
//   extractor. The hint is a classifier-time estimate; the LLM emits
//   whatever unit the document reports, and a future promote stage
//   will reconcile across hints. Today's surface keeps the closed-vocab
//   gate on `name` only — the same shape the dashboard's per-metric
//   tile keys off.

/// Per-Document observation extraction entry point. Called by each
/// `run_X_recipe` in `fetch_executor.rs` after the assertion + event
/// extraction calls, with the same inputs.
///
/// Returns an [`ExtractionReport`] for observability; the caller
/// ignores it today (the operator-visible signal is the dashboard
/// per-metric tiles ticking up). Errors are absorbed into the report;
/// this function never returns `Err`.
pub async fn extract_and_persist_observations(
    store: &Store,
    provider: &dyn LlmProvider,
    extraction_prompt: &str,
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) -> ExtractionReport {
    let mut report = ExtractionReport::default();

    let mime = response_content_type.map(normalise_mime).unwrap_or_default();
    if !should_extract_from(&mime, bytes.len()) {
        return report;
    }

    // Collect the plan's declared metric names. Empty list short-
    // circuits the LLM call inside `extract_observations_from_document`
    // so plans that don't track observations don't burn workhorse
    // tokens.
    let allowed_owned: Vec<String> = plan
        .expectations
        .observation_metrics
        .iter()
        .map(|m| m.name.clone())
        .collect();
    if allowed_owned.is_empty() {
        return report;
    }
    let allowed_refs: Vec<&str> = allowed_owned.iter().map(|s| s.as_str()).collect();

    let body = document_synth::body_preview_for_mime(&mime, bytes);
    if body.trim().is_empty() {
        return report;
    }

    let cfg = ExtractionConfig::default();
    let topic = plan.topic.as_str();
    let source_url = recipe.source_url.as_str();

    let drafts = match extract_observations_from_document(
        provider,
        &cfg,
        extraction_prompt,
        topic,
        source_url,
        &mime,
        &body,
        &allowed_refs,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(
                recipe_id = %recipe.id,
                error = %e,
                "document observation extraction LLM call failed; skipping this Document's observations"
            );
            report.call_error = Some(e.to_string());
            return report;
        }
    };

    report.extracted = drafts.len() as u32;
    if drafts.is_empty() {
        info!(
            recipe_id = %recipe.id,
            "document observation extraction returned no observations (empty list is a legal outcome)"
        );
        return report;
    }

    for draft in drafts {
        let observation = build_observation(plan, recipe, &draft, fetched_at);
        match store.insert_observation(&observation) {
            Ok(()) => report.persisted += 1,
            Err(e) => {
                report.insert_failures += 1;
                warn!(
                    recipe_id = %recipe.id,
                    observation_id = %observation.id,
                    error = %e,
                    "failed to persist extracted Observation; continuing with the rest of the batch"
                );
            }
        }
    }

    info!(
        recipe_id = %recipe.id,
        extracted = report.extracted,
        persisted = report.persisted,
        insert_failures = report.insert_failures,
        "document observation extraction complete"
    );

    report
}

/// Build one [`Observation`] from a validated [`ObservationDraft`].
/// Pure function — no I/O — so tests can pin the envelope shape.
///
/// `source_id` follows the same `{source}#recipe:{id}@v{ver}` format
/// as `build_assertion` / `build_event` / `recipe_apply::build_record`,
/// so `records_for_plan`'s LIKE join surfaces the Observation under
/// the originating plan.
///
/// When the LLM extracted a `when` timestamp, it lands on
/// `envelope.valid_at` — the dashboard's per-metric tile renders
/// against valid_at. When `when` is `None`, valid_at stays `None` and
/// downstream consumers fall back to `observed_at` for ordering (the
/// timestamp the document was fetched).
pub fn build_observation(
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    draft: &ObservationDraft,
    fetched_at: DateTime<Utc>,
) -> Observation {
    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            recipe.source_id, recipe.id, recipe.version
        ),
        source_url: Some(recipe.source_url.to_string()),
        source_published_at: None,
        license: "extracted".into(),
        derived_from: vec![],
        selector_path: None,
        raw_bytes_excerpt: None,
    };

    let subjects = Subjects {
        // Observations don't carry actors the way Events do; topics
        // alone are enough to route the row to the plan dashboard via
        // the topic_tags LIKE join. A future session may decide to
        // prompt the LLM for a single subject entity (e.g. the
        // company a `revenue` observation belongs to); today's v1
        // keeps the surface narrow.
        entities: vec![],
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: draft.when,
        observed_at: fetched_at,
        confidence: draft.confidence,
    };

    let content = ObservationContent {
        metric: draft.metric.clone(),
        value: draft.value,
        unit: draft.unit.clone(),
        value_uncertainty: draft.value_uncertainty,
        currency: draft.currency.clone(),
        period: draft.period.clone(),
        geometry: None,
    };

    Observation::new(envelope, content)
}

// ---------------------------------------------------------------------------
// Per-Document EntityAttribute extraction orchestrator (Session 80)
// ---------------------------------------------------------------------------
//
// Fourth sibling to the assertion (Session 77), event (Session 78), and
// observation (Session 79) orchestrators. The fetch executor calls this
// once per persisted Document, gated identically (article-kind +
// non-empty body). The orchestrator:
//
//   1. Calls `llm::extraction::extract_entity_attributes_from_document`
//      with the Document's body, MIME, source URL, and the plan's topic
//      for grounding. v1 has no closed-vocab gate on attribute names —
//      open-vocab matches the schema's `EntityAttributeContent.key`
//      shape. A future session can plumb `entity_kinds[].attributes[]`
//      through if the operator wants the gate.
//   2. Wraps each returned `EntityAttributeDraft` in an `Assertion`
//      envelope with `AssertedContent::EntityAttribute` — same posture
//      Session 77 uses for relation triples. Claimant defaults to
//      `agency:document` (the document is the source of the assertion);
//      stance defaults to `Stance::Asserted` (the document contains the
//      attribute by virtue of stating it). Future versions can have
//      the LLM emit claimant/stance per row.
//   3. Persists each `Assertion` via `Store::insert_assertion` — the
//      same destination Session 77 writes to. Records-for-plan's LIKE
//      join routes via the `{source}#recipe:{id}@v{ver}` provenance.
//
// ## Scope (v1/v2)
//
// - **Open- *or* closed-vocab `key`.** Session 81 added
//   `attributes: Vec<String>` to `EntityKindExpectation`. The
//   orchestrator collects the union of every kind's declared
//   attributes and hands the slice to the extractor. Empty slice =
//   open-vocab (Session 80 behaviour, preserved for plans that
//   didn't seed any kind with attributes); non-empty = closed-vocab
//   gate matching the relation / event / observation extractor
//   posture.
// - **Closed-vocab `value_kind`.** text / number / boolean only in v1
//   — the three shapes the most common attribute facts fit. Country /
//   Topic / Entity / EntityList / TopicList stay as future-session
//   work.
// - **Synthetic claimant + stance.** `agency:document` + `Asserted`.
//   No per-row LLM emission today.
// - **No retry, no dedup.** Same posture as the three earlier
//   extractors; failures warn-log and the loop continues.

/// Per-Document entity-attribute extraction entry point. Called by
/// each `run_X_recipe` in `fetch_executor.rs` after the assertion +
/// event + observation extraction calls, with the same inputs.
///
/// Returns an [`ExtractionReport`] for observability. Errors are
/// absorbed; this function never returns `Err` so the runtime path
/// can't break on LLM outage.
pub async fn extract_and_persist_entity_attributes(
    store: &situation_room_storage::Store,
    provider: &dyn LlmProvider,
    extraction_prompt: &str,
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    bytes: &[u8],
    response_content_type: Option<&str>,
    fetched_at: DateTime<Utc>,
) -> ExtractionReport {
    let mut report = ExtractionReport::default();

    let mime = response_content_type.map(normalise_mime).unwrap_or_default();
    if !should_extract_from(&mime, bytes.len()) {
        return report;
    }

    let body = document_synth::body_preview_for_mime(&mime, bytes);
    if body.trim().is_empty() {
        return report;
    }

    let cfg = ExtractionConfig::default();
    let topic = plan.topic.as_str();
    let source_url = recipe.source_url.as_str();

    // Session 81 — closed-vocab attribute-key gate. Collect the union
    // of every `EntityKindExpectation`'s declared `attributes`. Empty
    // Vec preserves Session 80 open-vocab behaviour; non-empty turns
    // the schema + validator into a closed-vocab gate. We deliberately
    // don't dedup or sort — small N (typically < 30 keys total) and
    // membership tests work on the literal slice either way.
    let allowed_keys_owned: Vec<String> = plan
        .expectations
        .entity_kinds
        .iter()
        .flat_map(|k| k.attributes.iter().cloned())
        .collect();
    let allowed_keys_refs: Vec<&str> =
        allowed_keys_owned.iter().map(|s| s.as_str()).collect();

    let drafts = match extract_entity_attributes_from_document(
        provider,
        &cfg,
        extraction_prompt,
        topic,
        source_url,
        &mime,
        &body,
        &allowed_keys_refs,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(
                recipe_id = %recipe.id,
                error = %e,
                "document entity-attribute extraction LLM call failed; \
                 skipping this Document's attributes"
            );
            report.call_error = Some(e.to_string());
            return report;
        }
    };

    report.extracted = drafts.len() as u32;
    if drafts.is_empty() {
        info!(
            recipe_id = %recipe.id,
            "document entity-attribute extraction returned no attributes \
             (empty list is a legal outcome)"
        );
        return report;
    }

    for draft in drafts {
        let assertion = build_entity_attribute_assertion(plan, recipe, &draft, fetched_at);
        match store.insert_assertion(&assertion) {
            Ok(()) => report.persisted += 1,
            Err(e) => {
                report.insert_failures += 1;
                warn!(
                    recipe_id = %recipe.id,
                    assertion_id = %assertion.id,
                    error = %e,
                    "failed to persist extracted entity attribute; \
                     continuing with the rest of the batch"
                );
            }
        }
    }

    info!(
        recipe_id = %recipe.id,
        extracted = report.extracted,
        persisted = report.persisted,
        insert_failures = report.insert_failures,
        "document entity-attribute extraction complete"
    );

    report
}

/// Build one `Assertion` from a validated [`EntityAttributeDraft`].
/// Pure function — no I/O — so tests can pin the envelope shape.
///
/// **Claimant + stance (Session 81).** The draft carries the LLM-
/// emitted per-row claimant + stance (already resolved by
/// `validate_entity_attribute_one` — bad / missing values fall back
/// to `agency:document` + `Stance::Asserted` so the row stays
/// emitable). This unifies the entity-attribute path with the
/// relation extractor's per-row attribution shape.
///
/// **Provenance.** Same `{source}#recipe:{id}@v{ver}` shape the three
/// earlier orchestrators use, so `records_for_plan`'s LIKE join
/// routes the Assertion under the originating plan.
pub fn build_entity_attribute_assertion(
    plan: &crate::research::ResearchPlan,
    recipe: &FetchRecipe,
    draft: &EntityAttributeDraft,
    fetched_at: DateTime<Utc>,
) -> Assertion {
    let provenance = Provenance {
        source_id: format!(
            "{}#recipe:{}@v{}",
            recipe.source_id, recipe.id, recipe.version
        ),
        source_url: Some(recipe.source_url.to_string()),
        source_published_at: None,
        license: "extracted".into(),
        derived_from: vec![],
        selector_path: None,
        raw_bytes_excerpt: None,
    };

    let subjects = Subjects {
        // The attribute's subject entity surfaces as a subject so the
        // cross-record entity join lights up the Assertion alongside
        // any Entity rows for the same actor.
        entities: vec![draft.entity_id.clone()],
        places: vec![],
        time: None,
        topics: plan.topic_tags.clone(),
    };

    let envelope = Envelope {
        provenance,
        subjects,
        tags: vec![],
        valid_at: None,
        observed_at: fetched_at,
        confidence: draft.confidence,
    };

    let content = AssertedContent::EntityAttribute(EntityAttributeContent {
        entity_id: draft.entity_id.clone(),
        key: draft.key.clone(),
        value: draft.value.clone(),
    });

    // Session 81 — claimant + stance lift from the draft. The
    // validator resolved both: `agency:document` + `Asserted`
    // defaults when the LLM didn't emit per-row values.
    Assertion::new(draft.claimant.clone(), draft.stance, content, envelope)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{ExtractionSpec, FetchRecipe};
    use crate::research::{RecordExpectations, ResearchPlan};
    use chrono::TimeZone;
    use situation_room_core::vocab::{Confidence, EntityId, Stance, Topic};
    use url::Url;
    use uuid::Uuid;

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            id: Uuid::now_v7(),
            topic: "tesla battery supply chain".into(),
            interpretation: "test".into(),
            topic_tags: vec![Topic::new("tesla").unwrap()],
            geographic_scope: vec![],
            historical_window_days: 30,
            expectations: RecordExpectations::default(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap(),
        }
    }

    fn sample_recipe(plan: &ResearchPlan, url: &str, source_id: &str) -> FetchRecipe {
        FetchRecipe {
            id: Uuid::now_v7(),
            dedup_key: Some(format!("{}:{}:demo", plan.id, source_id)),
            plan_id: plan.id,
            source_id: source_id.into(),
            source_url: Url::parse(url).unwrap(),
            extraction: ExtractionSpec::CsvCell {
                column: "value".into(),
                row_filter: None,
            },
            iterator: None,
            produces: vec![],
            authored_at: Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap(),
            authored_by: "session-77-test".into(),
            version: 1,
            static_payload: None,
            authored_from: situation_room_storage::AuthoredFrom::FetchedBytes,
            prior_recipe_id: None,
            reauthor_reason: None,
        }
    }

    fn sample_draft() -> AssertionDraft {
        AssertionDraft {
            claimant: EntityId::new("agency:reuters").unwrap(),
            stance: Stance::Reported,
            kind: "supplies_to".into(),
            from: EntityId::new("company:panasonic").unwrap(),
            to: EntityId::new("company:tsla").unwrap(),
            confidence: Confidence::new(0.85).unwrap(),
        }
    }

    #[test]
    fn should_extract_from_accepts_html_with_body() {
        assert!(should_extract_from("text/html", 1024));
        assert!(should_extract_from("text/html; charset=utf-8", 1024));
        assert!(should_extract_from("application/xhtml+xml", 1024));
    }

    #[test]
    fn should_extract_from_rejects_non_html() {
        // Closed-vocabulary discipline: structured-text MIMEs and
        // binary MIMEs are both ineligible. PDF would need an OCR
        // pass we don't have today; JSON / CSV carry no prose.
        assert!(!should_extract_from("application/json", 1024));
        assert!(!should_extract_from("text/csv", 1024));
        assert!(!should_extract_from("application/pdf", 1024));
        assert!(!should_extract_from("text/plain", 1024));
        assert!(!should_extract_from("application/octet-stream", 1024));
        assert!(!should_extract_from("image/png", 1024));
    }

    #[test]
    fn should_extract_from_rejects_empty_body() {
        // Empty body is the binary-MIME signal from
        // `document_synth::body_preview`. Skip extraction even when
        // the MIME claims HTML — there's nothing to read.
        assert!(!should_extract_from("text/html", 0));
    }

    #[test]
    fn build_assertion_carries_recipe_provenance() {
        let plan = sample_plan();
        let recipe = sample_recipe(&plan, "https://example.test/article", "example_news");
        let draft = sample_draft();
        let fetched_at = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

        let assertion = build_assertion(&plan, &recipe, &draft, fetched_at);

        // Provenance: same shape `recipe_apply` produces — so
        // records_for_plan's LIKE join routes it.
        assert_eq!(
            assertion.envelope.provenance.source_id,
            format!("example_news#recipe:{}@v1", recipe.id)
        );
        assert_eq!(
            assertion.envelope.provenance.source_url.as_deref(),
            Some("https://example.test/article")
        );
        assert_eq!(assertion.envelope.observed_at, fetched_at);

        // Claimant + stance + content.
        assert_eq!(assertion.claimant.as_str(), "agency:reuters");
        assert!(matches!(assertion.stance, Stance::Reported));
        match &assertion.content {
            AssertedContent::Relation(r) => {
                assert_eq!(r.kind, "supplies_to");
                assert_eq!(r.from.as_str(), "company:panasonic");
                assert_eq!(r.to.as_str(), "company:tsla");
            }
            other => panic!("expected Relation content, got: {other:?}"),
        }

        // Subject entities include both endpoints so the
        // cross-record entity join surfaces the assertion alongside
        // the Entity rows for those actors.
        assert_eq!(assertion.envelope.subjects.entities.len(), 2);
        assert_eq!(assertion.envelope.subjects.entities[0].as_str(), "company:panasonic");
        assert_eq!(assertion.envelope.subjects.entities[1].as_str(), "company:tsla");

        // Topics propagate from the plan.
        assert_eq!(assertion.envelope.subjects.topics.len(), 1);
    }

    #[test]
    fn normalise_mime_handles_parameters_and_whitespace() {
        assert_eq!(normalise_mime("Text/HTML; charset=UTF-8"), "text/html");
        assert_eq!(normalise_mime(" application/json "), "application/json");
        assert_eq!(normalise_mime(""), "application/octet-stream");
    }

    #[test]
    fn extraction_report_default_is_zeroed() {
        let r = ExtractionReport::default();
        assert_eq!(r.extracted, 0);
        assert_eq!(r.persisted, 0);
        assert_eq!(r.insert_failures, 0);
        assert!(r.call_error.is_none());
    }

    // -------------------------------------------------------------------
    // Per-Document Event extraction tests (Session 78)
    // -------------------------------------------------------------------

    fn sample_event_draft() -> EventDraft {
        use situation_room_core::schema::EventDirection;
        use situation_room_core::vocab::EventType;
        EventDraft {
            event_type: EventType::new("earnings_release").unwrap(),
            headline: "NVIDIA reports record Q4 revenue.".into(),
            actors: vec![EntityId::new("company:nvda").unwrap()],
            direction: Some(EventDirection::DemandPositive),
            when: Some(Utc.with_ymd_and_hms(2026, 2, 21, 21, 0, 0).unwrap()),
            confidence: Confidence::new(0.9).unwrap(),
        }
    }

    #[test]
    fn build_event_carries_recipe_provenance_and_envelope_metadata() {
        let plan = sample_plan();
        let recipe = sample_recipe(
            &plan,
            "https://example.test/article",
            "example_news",
        );
        let draft = sample_event_draft();
        let fetched_at = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

        let event = build_event(&plan, &recipe, &draft, fetched_at);

        // Provenance — same `{source}#recipe:{id}@v{ver}` shape the
        // assertion path uses, so records_for_plan's LIKE join routes
        // it under the plan dashboard.
        assert_eq!(
            event.envelope.provenance.source_id,
            format!("example_news#recipe:{}@v1", recipe.id)
        );
        assert_eq!(
            event.envelope.provenance.source_url.as_deref(),
            Some("https://example.test/article")
        );
        // `valid_at` carries the extracted `when`; `observed_at` is
        // the fetched_at timestamp the executor passed in.
        assert_eq!(
            event.envelope.valid_at,
            Some(Utc.with_ymd_and_hms(2026, 2, 21, 21, 0, 0).unwrap())
        );
        assert_eq!(event.envelope.observed_at, fetched_at);
        // Content shape — event_type / headline / actors / direction.
        assert_eq!(event.content.event_type.as_str(), "earnings_release");
        assert_eq!(event.content.headline, "NVIDIA reports record Q4 revenue.");
        assert_eq!(event.content.actors.len(), 1);
        assert_eq!(event.content.actors[0].as_str(), "company:nvda");
        assert!(event.content.direction.is_some());
        // Topics propagate from the plan, mirroring build_assertion.
        assert_eq!(event.envelope.subjects.topics.len(), 1);
    }

    #[test]
    fn build_event_with_no_when_leaves_valid_at_none() {
        // `when = None` → envelope.valid_at stays None; downstream
        // event-feed UI falls back to observed_at for ordering.
        let plan = sample_plan();
        let recipe = sample_recipe(&plan, "https://example.test/p", "ex");
        let mut draft = sample_event_draft();
        draft.when = None;
        let fetched_at = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

        let event = build_event(&plan, &recipe, &draft, fetched_at);

        assert!(event.envelope.valid_at.is_none());
        assert_eq!(event.envelope.observed_at, fetched_at);
    }

    // -------------------------------------------------------------------
    // Session 80 — EntityAttribute orchestrator
    // -------------------------------------------------------------------

    #[test]
    fn build_entity_attribute_assertion_carries_recipe_provenance() {
        use situation_room_core::schema::content::AttributeValue;
        let plan = sample_plan();
        let recipe = sample_recipe(
            &plan,
            "https://example.test/article",
            "example_news",
        );
        let draft = EntityAttributeDraft {
            entity_id: EntityId::new("company:tsla").unwrap(),
            key: "employee_count".into(),
            value: AttributeValue::Number {
                value: 140_473.0,
                unit: Some(situation_room_core::vocab::Unit::new("persons").unwrap()),
            },
            confidence: Confidence::new(0.85).unwrap(),
            claimant: EntityId::new("agency:document").unwrap(),
            stance: Stance::Asserted,
        };
        let fetched_at = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

        let assertion = build_entity_attribute_assertion(&plan, &recipe, &draft, fetched_at);

        // Provenance — same `{source}#recipe:{id}@v{ver}` shape as the
        // assertion / event / observation paths.
        assert_eq!(
            assertion.envelope.provenance.source_id,
            format!("example_news#recipe:{}@v1", recipe.id)
        );
        // Synthetic claimant + stance for v1 (the document is the
        // source by construction; future versions can have the LLM
        // emit these per row).
        assert_eq!(assertion.claimant.as_str(), "agency:document");
        assert!(matches!(assertion.stance, Stance::Asserted));
        // Content shape — EntityAttribute carries the typed value.
        match &assertion.content {
            AssertedContent::EntityAttribute(a) => {
                assert_eq!(a.entity_id.as_str(), "company:tsla");
                assert_eq!(a.key, "employee_count");
                match &a.value {
                    AttributeValue::Number { value, unit } => {
                        assert!((value - 140_473.0).abs() < 1e-3);
                        assert_eq!(unit.as_ref().map(|u| u.as_str()), Some("persons"));
                    }
                    other => panic!("expected Number, got: {other:?}"),
                }
            }
            other => panic!("expected EntityAttribute content, got: {other:?}"),
        }
        // Subject entities include the attribute's owner so the
        // cross-record entity join surfaces the assertion alongside
        // Entity rows for that actor.
        assert_eq!(assertion.envelope.subjects.entities.len(), 1);
        assert_eq!(assertion.envelope.subjects.entities[0].as_str(), "company:tsla");
        // Topics propagate from the plan.
        assert_eq!(assertion.envelope.subjects.topics.len(), 1);
        assert_eq!(assertion.envelope.observed_at, fetched_at);
    }
}
