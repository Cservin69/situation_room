//! Source registry — discovery and lookup.
//!
//! Phase 2+ wires this up so the runtime can list all enabled sources
//! and dispatch fetches by id.

use crate::traits::Source;
use std::collections::HashMap;
use std::sync::Arc;

/// In-memory registry of sources keyed by their metadata id.
#[derive(Default)]
pub struct Registry {
    sources: HashMap<String, Arc<dyn Source>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, source: Arc<dyn Source>) {
        let id = source.metadata().id.clone();
        self.sources.insert(id, source);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Source>> {
        self.sources.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }
}
