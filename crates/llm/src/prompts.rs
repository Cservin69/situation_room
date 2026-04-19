//! Prompt loader.
//!
//! Prompts live as versioned markdown files in `config/prompts/` so they
//! can be edited without recompiling and reviewed in PRs as plain text.
//!
//! Phase 3 will implement the templating engine. Phase 1 declares the path.

use serde::{Deserialize, Serialize};

/// Identifier for a versioned prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptId(pub String);

/// A loaded prompt template before variable substitution.
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub id: PromptId,
    pub body: String,
}
