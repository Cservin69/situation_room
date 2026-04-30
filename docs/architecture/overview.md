# Situation_room architecture — overview

This document is the guided tour of the codebase. Read it before contributing.

## What Situation_room is

An open-source analyst workstation for critical-minerals intelligence —
generalizable, via the research function, to any topic with public-source
coverage. Type a topic, get a single-screen workstation populated from
open data, with every claim traceable to a source.

## The seven crates

```
core        the schema (zero workspace deps)
storage     DuckDB persistence
sources     data adapters (one folder per source)
llm         provider router, prompts, structured extraction
pipeline    ingest → normalize → extract → promote, plus research planner
analytics   anomaly detectors, aggregates, scoring
api         Tauri command surface and TS type export
```

Composition root lives in `apps/desktop/src-tauri/src/main.rs`.

## The lifecycle of a record

1. **Ingest** — a source's `fetch()` is called by the scheduler.
2. **Normalize** — units to UCUM, currencies to USD, entities to canonical IDs.
3. **Extract** — for documents, the LLM produces Assertions about their content.
4. **Promote** — Assertions agreed across N sources or from authoritative
   sources are elevated to Observations / Events / Relations.
5. **Detect** — anomaly detectors run over storage and emit `Anomaly` records.
6. **Render** — the frontend queries through `api`, renders panels.

## The research function

Free-text topic → LLM decomposition → `ResearchPlan` → matched against
source registry → triggered ingestion → `CoverageReport` shown alongside
panels so user knows what's missing.

## Key cross-cutting decisions

See `docs/adr/`. Eight ADRs documenting the load-bearing choices.
