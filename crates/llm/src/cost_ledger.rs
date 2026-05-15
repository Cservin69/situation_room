//! Cost-by-tier ledger — Session 75 piece 1.
//!
//! ## What this module is
//!
//! A process-wide tally of LLM-completion accounting, keyed by
//! `(provider_id, ModelTier)`. Every `complete()` call's
//! [`CompletionResponse`] gets recorded here; the totals are read by
//! the Tauri command `llm_cost_ledger` and rendered as a dashboard
//! panel.
//!
//! The ledger is the operator's way of seeing the v1.22 prompt-cache
//! lever (Session 74) actually working without grepping INFO logs.
//! Specifically — `cached_input_tokens / input_tokens` per
//! (provider, tier) should rise from ~0.03 (pre-v1.22) to ~0.92
//! (v1.22) on warm authoring calls; the dashboard tile is the
//! glanceable signal.
//!
//! ## Why a wrapping `LlmProvider`, not an extraction-layer hook
//!
//! Three reasons.
//!
//! 1. **One source of truth.** Every `LlmProvider::complete` call
//!    flows through the trait. Wrapping at the trait boundary catches
//!    every callsite without each call-site having to remember to
//!    report.
//! 2. **Tier-aware.** The ledger keys are (provider_id, tier); the
//!    trait method receives the tier as a typed argument. An
//!    extraction-layer hook would have to thread the tier through
//!    every call shape (classifier, recipe-author, propose-URL, …),
//!    duplicating what the trait already carries.
//! 3. **No source-routing language.** The decorator doesn't inspect
//!    the request body, model name, host, or any payload field. It
//!    only sees (provider_id, tier, response.usage). Stays inside
//!    the closed-vocabulary discipline (memory
//!    `project_sr_no_source_routing`).
//!
//! ## What the ledger does NOT do
//!
//! - **No persistence.** The ledger is in-memory only — the totals
//!   reset on every binary restart. A persistent rollup would be a
//!   later session; today the operator-visible value is intra-
//!   session ("does the cache lever work right now?"), and a
//!   process-restart-clean ledger is honest about that scope.
//! - **No cost in dollars.** Pricing tables drift per provider and
//!   tier; baking them in would either go stale or grow into a
//!   provider-aware config surface. Tokens (with cache split) is
//!   the most-stable, provider-portable unit; the operator can
//!   multiply by the current $/1k-tokens themselves.
//! - **No per-call timeline.** The ledger is cumulative; a per-call
//!   stream would belong on tracing, not on the dashboard. The
//!   xAI `cached_tokens=Some(N)` INFO log line (Session 72) already
//!   covers the per-call surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::providers::trait_def::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ModelTier,
};

// ---------------------------------------------------------------------------
// Tally — one bucket's running totals
// ---------------------------------------------------------------------------

/// Cumulative totals for one `(provider_id, tier)` bucket.
///
/// `calls_with_cache_data` is the denominator for the cache-hit ratio:
/// only count calls where the provider actually reported
/// `cached_input_tokens`. A `None` (provider didn't report) collapses
/// into a zero-denominator surface that the wire DTO renders as "—"
/// rather than 0%, so the dashboard doesn't falsely claim a cold cache
/// for providers that simply don't expose the field.
#[derive(Debug, Default, Clone)]
pub struct Tally {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    /// How many of the `calls` carried a `Some(_)` value for
    /// `cached_input_tokens`. Distinct from `calls` because
    /// `cached_input_tokens` being `None` (unknown) is not the same
    /// as `Some(0)` (cold prefix). See the field doc on
    /// [`CompletionResponse::cached_input_tokens`].
    pub calls_with_cache_data: u64,
}

// ---------------------------------------------------------------------------
// Ledger — the shared store of tallies
// ---------------------------------------------------------------------------

/// Process-wide ledger. Clone is cheap (an `Arc` clone); every
/// [`MeteredProvider`] holds a clone, and the Tauri command-handler
/// reads via another clone.
#[derive(Debug, Clone, Default)]
pub struct CostLedger {
    inner: Arc<Mutex<HashMap<(String, ModelTier), Tally>>>,
}

impl CostLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one completion into the appropriate bucket.
    ///
    /// Lock posture: short critical section (a `HashMap::entry` and a
    /// handful of u64 adds). The lock is `std::sync::Mutex`, not a
    /// `tokio::sync::Mutex`, because the work doesn't await; holding a
    /// std mutex across an `.await` is a deadlock risk we don't take.
    pub fn record(
        &self,
        provider_id: &str,
        tier: ModelTier,
        response: &CompletionResponse,
    ) {
        let key = (provider_id.to_string(), tier);
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                // Cost telemetry is not load-bearing; a poisoned mutex
                // means a previous panic happened mid-record. Recover
                // by taking the inner guard and continuing — losing one
                // call's accounting is better than panicking the
                // request path that's trying to record it.
                tracing::warn!(
                    "cost_ledger mutex poisoned; recovering and continuing"
                );
                poisoned.into_inner()
            }
        };
        let entry = guard.entry(key).or_default();
        entry.calls = entry.calls.saturating_add(1);
        if let Some(n) = response.input_tokens {
            entry.input_tokens = entry.input_tokens.saturating_add(n as u64);
        }
        if let Some(n) = response.output_tokens {
            entry.output_tokens = entry.output_tokens.saturating_add(n as u64);
        }
        if let Some(n) = response.cached_input_tokens {
            entry.cached_input_tokens =
                entry.cached_input_tokens.saturating_add(n as u64);
            entry.calls_with_cache_data =
                entry.calls_with_cache_data.saturating_add(1);
        }
    }

    /// Snapshot of every bucket, sorted (provider asc, tier asc) for
    /// stable wire ordering. The dashboard caller renders rows in the
    /// order returned; a stable order means the panel doesn't jiggle
    /// between refreshes.
    pub fn snapshot(&self) -> Vec<LedgerEntry> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut out: Vec<LedgerEntry> = guard
            .iter()
            .map(|((provider, tier), tally)| LedgerEntry {
                provider: provider.clone(),
                tier: *tier,
                tally: tally.clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| tier_sort_key(a.tier).cmp(&tier_sort_key(b.tier)))
        });
        out
    }
}

/// Tier ordering for stable wire output. Frontier → Workhorse → Cheap
/// is the same priority order the codebase uses when reasoning about
/// quality vs. cost. Keeps the dashboard rows reading top-down from
/// "most expensive" to "least expensive" within each provider.
fn tier_sort_key(t: ModelTier) -> u8 {
    match t {
        ModelTier::Frontier => 0,
        ModelTier::Workhorse => 1,
        ModelTier::Cheap => 2,
    }
}

/// One bucket's contents as it leaves the ledger. The API crate's
/// wire DTO lifts this into ts-rs-exported form.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub provider: String,
    pub tier: ModelTier,
    pub tally: Tally,
}

// ---------------------------------------------------------------------------
// MeteredProvider — the decorator that records every completion
// ---------------------------------------------------------------------------

/// Wrap any `Arc<dyn LlmProvider + Send + Sync>` in a ledger-aware
/// decorator. Forwards trait methods to the inner provider and
/// records the response into the shared `CostLedger`.
///
/// The decorator is itself `LlmProvider`, so wrapping is transparent
/// to every call site: `AppState::provider` keeps the same trait
/// shape regardless of whether the wrap is in place.
pub struct MeteredProvider {
    inner: Arc<dyn LlmProvider + Send + Sync>,
    ledger: Arc<CostLedger>,
}

impl MeteredProvider {
    pub fn new(
        inner: Arc<dyn LlmProvider + Send + Sync>,
        ledger: Arc<CostLedger>,
    ) -> Self {
        Self { inner, ledger }
    }

    /// Borrow the inner provider — useful for tests that need to
    /// assert against the un-wrapped surface (the decorator itself
    /// doesn't observe per-request state).
    pub fn inner(&self) -> &Arc<dyn LlmProvider + Send + Sync> {
        &self.inner
    }

    /// Borrow the ledger handle (an `Arc` clone, cheap).
    pub fn ledger(&self) -> Arc<CostLedger> {
        Arc::clone(&self.ledger)
    }
}

#[async_trait]
impl LlmProvider for MeteredProvider {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn supported_tiers(&self) -> &[ModelTier] {
        self.inner.supported_tiers()
    }

    async fn complete(
        &self,
        tier: ModelTier,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        // Capture provider id before the inner call so we don't have to
        // re-borrow `self.inner` after the move into `complete`.
        let provider_id = self.inner.id();
        let response = self.inner.complete(tier, request).await?;
        self.ledger.record(provider_id, tier, &response);
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::trait_def::CompletionResponse;
    use async_trait::async_trait;

    fn fake_response(
        input: Option<u32>,
        output: Option<u32>,
        cached: Option<u32>,
    ) -> CompletionResponse {
        CompletionResponse {
            text: String::new(),
            structured: None,
            provider: "test".into(),
            model: "test-model".into(),
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: cached,
        }
    }

    #[test]
    fn record_accumulates_into_the_right_bucket() {
        let ledger = CostLedger::new();
        ledger.record(
            "xai",
            ModelTier::Workhorse,
            &fake_response(Some(1000), Some(200), Some(900)),
        );
        ledger.record(
            "xai",
            ModelTier::Workhorse,
            &fake_response(Some(1100), Some(150), Some(1000)),
        );
        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 1, "one bucket: (xai, Workhorse)");
        let e = &snap[0];
        assert_eq!(e.provider, "xai");
        assert_eq!(e.tier, ModelTier::Workhorse);
        assert_eq!(e.tally.calls, 2);
        assert_eq!(e.tally.input_tokens, 2100);
        assert_eq!(e.tally.output_tokens, 350);
        assert_eq!(e.tally.cached_input_tokens, 1900);
        assert_eq!(e.tally.calls_with_cache_data, 2);
    }

    #[test]
    fn separate_buckets_for_distinct_tiers() {
        let ledger = CostLedger::new();
        ledger.record(
            "anthropic",
            ModelTier::Frontier,
            &fake_response(Some(100), Some(50), None),
        );
        ledger.record(
            "anthropic",
            ModelTier::Cheap,
            &fake_response(Some(200), Some(10), Some(0)),
        );
        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 2);
        // Stable order: Frontier (0) < Cheap (2)
        assert_eq!(snap[0].tier, ModelTier::Frontier);
        assert_eq!(snap[1].tier, ModelTier::Cheap);
    }

    #[test]
    fn snapshot_sorts_by_provider_then_tier() {
        let ledger = CostLedger::new();
        ledger.record(
            "xai",
            ModelTier::Cheap,
            &fake_response(Some(1), Some(1), None),
        );
        ledger.record(
            "anthropic",
            ModelTier::Workhorse,
            &fake_response(Some(1), Some(1), None),
        );
        ledger.record(
            "anthropic",
            ModelTier::Frontier,
            &fake_response(Some(1), Some(1), None),
        );
        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 3);
        // anthropic comes first alphabetically; within anthropic,
        // Frontier (0) before Workhorse (1); then xai.
        assert_eq!(snap[0].provider, "anthropic");
        assert_eq!(snap[0].tier, ModelTier::Frontier);
        assert_eq!(snap[1].provider, "anthropic");
        assert_eq!(snap[1].tier, ModelTier::Workhorse);
        assert_eq!(snap[2].provider, "xai");
        assert_eq!(snap[2].tier, ModelTier::Cheap);
    }

    #[test]
    fn none_cached_input_tokens_does_not_bump_cache_denominator() {
        // Anthropic without cache_control blocks: input_tokens is
        // present but cached_input_tokens is None. The denominator
        // (`calls_with_cache_data`) must stay 0 so the dashboard
        // doesn't claim a 0% hit ratio over zero observations.
        let ledger = CostLedger::new();
        ledger.record(
            "anthropic",
            ModelTier::Workhorse,
            &fake_response(Some(500), Some(50), None),
        );
        let snap = ledger.snapshot();
        let e = &snap[0];
        assert_eq!(e.tally.calls, 1);
        assert_eq!(e.tally.calls_with_cache_data, 0);
        assert_eq!(e.tally.cached_input_tokens, 0);
    }

    #[test]
    fn some_zero_cached_input_tokens_bumps_denominator() {
        // Cold-prefix call: provider reported the field as 0. The
        // denominator must bump so the ratio is well-defined as 0/n,
        // distinct from the None case above.
        let ledger = CostLedger::new();
        ledger.record(
            "xai",
            ModelTier::Workhorse,
            &fake_response(Some(500), Some(50), Some(0)),
        );
        let snap = ledger.snapshot();
        let e = &snap[0];
        assert_eq!(e.tally.calls_with_cache_data, 1);
        assert_eq!(e.tally.cached_input_tokens, 0);
    }

    #[test]
    fn missing_input_tokens_does_not_panic_or_corrupt() {
        // Some stub providers don't report usage at all. Ledger must
        // record the call without crashing or skewing totals.
        let ledger = CostLedger::new();
        ledger.record(
            "stub",
            ModelTier::Cheap,
            &fake_response(None, None, None),
        );
        let snap = ledger.snapshot();
        let e = &snap[0];
        assert_eq!(e.tally.calls, 1);
        assert_eq!(e.tally.input_tokens, 0);
        assert_eq!(e.tally.output_tokens, 0);
    }

    // ---- MeteredProvider decorator path ----

    struct StubProvider {
        next: CompletionResponse,
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn id(&self) -> &'static str {
            "stubprov"
        }
        fn supported_tiers(&self) -> &[ModelTier] {
            &[ModelTier::Cheap, ModelTier::Workhorse, ModelTier::Frontier]
        }
        async fn complete(
            &self,
            _tier: ModelTier,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            Ok(self.next.clone())
        }
    }

    #[tokio::test]
    async fn metered_provider_records_completion_into_shared_ledger() {
        let inner: Arc<dyn LlmProvider + Send + Sync> =
            Arc::new(StubProvider {
                next: fake_response(Some(2000), Some(80), Some(1800)),
            });
        let ledger = Arc::new(CostLedger::new());
        let metered = MeteredProvider::new(inner, Arc::clone(&ledger));
        let req = CompletionRequest {
            system: None,
            user: "hi".into(),
            schema: None,
            max_tokens: 8,
            temperature: 0.0,
            reasoning_effort: None,
        };
        let _ = metered.complete(ModelTier::Workhorse, req).await.unwrap();
        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 1);
        let e = &snap[0];
        assert_eq!(e.provider, "stubprov");
        assert_eq!(e.tier, ModelTier::Workhorse);
        assert_eq!(e.tally.calls, 1);
        assert_eq!(e.tally.cached_input_tokens, 1800);
    }

    #[tokio::test]
    async fn metered_provider_preserves_id_and_supported_tiers() {
        let inner: Arc<dyn LlmProvider + Send + Sync> =
            Arc::new(StubProvider {
                next: fake_response(None, None, None),
            });
        let ledger = Arc::new(CostLedger::new());
        let metered = MeteredProvider::new(inner, ledger);
        assert_eq!(metered.id(), "stubprov");
        assert_eq!(metered.supported_tiers().len(), 3);
    }
}
