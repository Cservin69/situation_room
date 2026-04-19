# ADR 0001 — Monorepo workspace layout

**Status**: Accepted
**Date**: 2026-04-19

## Decision

Single Cargo workspace, seven crates: core, storage, sources, llm, pipeline, analytics, api. Composition root lives in `apps/desktop/src-tauri/`, not in a separate `app` crate.

## Rationale

[To be written together with the human reviewer. Captures the conversation
about "Java monolith" mental model, why workspaces > micro-crates at this
stage, why we collapsed `app` and `cli` into the Tauri shim, and the
"structure follows code, not anticipates it" principle.]
