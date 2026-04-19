# ADR 0008 — Offline mode and cache architecture

**Status**: Accepted
**Date**: 2026-04-19

## Decision

DuckDB is the source of truth for the UI. The network layer writes through
into DuckDB; the UI never blocks on network. Feed panels keep a ring
buffer of recent items; archive items (explicitly opened/pinned) are never
auto-evicted. Offline toggle pauses the scheduler and surfaces a "last
updated" timestamp on each panel.

## Rationale

[To be written together. Captures the cache-first-UI pattern, why panels
are fast as a result, and the article-archiving / on-demand-fetch model
the human reviewer specified.]
