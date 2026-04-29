//! Document → Assertion extraction. Thin orchestrator over `llm::extraction`.
//! Loads documents that haven't been extracted yet, batches them, dispatches
//! to the LLM layer, persists assertions. Phase 3.
