//! Provider router — selects which configured provider serves a given tier.
//!
//! Phase 1 stub. Phase 3 wires this up with config-driven preferences and
//! fallback chains (e.g. "prefer Anthropic for Frontier; fall back to OpenAI
//! if Anthropic is rate-limited").

use crate::providers::{LlmProvider, ModelTier};
use std::sync::Arc;

#[derive(Default)]
pub struct ProviderRouter {
    providers: Vec<Arc<dyn LlmProvider>>,
}

impl ProviderRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) {
        self.providers.push(provider);
    }

    /// Pick a provider for the given tier. Phase 3 will add preference logic.
    pub fn pick(&self, tier: ModelTier) -> Option<Arc<dyn LlmProvider>> {
        self.providers
            .iter()
            .find(|p| p.supported_tiers().contains(&tier))
            .cloned()
    }
}
