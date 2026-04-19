# ADR 0007 — Topic research as a first-class capability

**Status**: Accepted
**Date**: 2026-04-19

## Decision

The research function — "type a topic, populate the workstation" — is a
first-class v0 feature, not a v2 addition. Lives in `pipeline::research`.
Decomposes free-text topics into structured `ResearchPlan` objects via the
LLM frontier tier, matches the plan against the source registry, and
surfaces a `CoverageReport` to the UI alongside whatever data was retrieved.

## Rationale

[To be written together. This is what makes Stockpile general-purpose
rather than commodity-only, and what differentiates it from Bloomberg
(structured-data-Bloomberg-decided-to-cover) and Palantir (whatever the
forward-deployed engineers built).]
