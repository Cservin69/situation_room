# Session 3 — Part 2 patch (3c.3 recipe apply runtime)

This patch is a **delta** on top of the earlier Session 3 patch. It
applies the same way:

```bash
cd /Users/aben/RustroverProjects/situation_room
tar -xvf ~/Downloads/situation_room_session3_part2_patch.tar.gz
```

## What this patch contains

### ADR revision

- `docs/adr/0007-research-function.md`
  Appended a 2026-04-22 review note that re-affirms the LLM-free-
  runtime property. Records the deliberate rejection of "LLM on every
  refresh" and names the `PdfTable` deferral explicitly. Old content
  preserved; the review note is additive so the archaeology is
  visible.

### New workspace deps (runtime extractors)

- `Cargo.toml` — added `regex`, `csv`, `scraper`, `serde_json_path`
  at workspace level. All MIT / Apache, all pure-Rust, nothing new
  that `cargo-deny` would flag.
- `crates/pipeline/Cargo.toml` — declared them as real deps.

### Recipe apply runtime (new)

- `crates/pipeline/src/recipe_apply.rs` (~1050 lines)
  The deterministic Level-2 runtime. Public API:
  ```rust
  pub fn apply(ctx: ApplyContext<'_>) -> Result<Vec<Record>, ApplyError>;
  ```
  Four of five extraction modes wired with real fixture tests:
  - `JsonPath` via `serde_json_path`
  - `CssSelect` via `scraper`
  - `CsvCell` via `csv` (supports both `Equals` and `LabeledAs`
    row filters; rejects ambiguous multi-row extractions)
  - `RegexCapture` via `regex`
  `PdfTable` returns `ApplyError::NotImplemented` with a clear
  reason — honest unavailable, not a silent fallback.

  Per-field resolution: `Extracted` values parse to JSON scalars
  (numbers preferred, falls back to string), `Literal` passes
  through, `FromPlan` walks a dotted pointer into the plan's JSON.
  Content types stay authoritative about their own shape — we
  assemble JSON and `serde_json::from_value` into
  `ObservationContent` / `EventContent` / etc. Type mismatches
  surface as `ApplyError::ContentAssembly`.

  Provenance stamping follows the ADR: `source_id` becomes
  `{source_id}#recipe:{recipe_id}@v{version}`.

  ~30 tests: per-extractor unit tests, scalar-parsing edge cases,
  pointer walker, path inserter, end-to-end CSV → Observation,
  end-to-end PdfTable → NotImplemented, end-to-end non-numeric
  extraction → ContentAssembly.

### Normalize module (fleshed out)

- `crates/pipeline/src/normalize.rs`
  Was a one-line stub. Now has `finalize(record, plan, recipe)` that
  attaches the session's `topic_tags` to `subjects.topics`
  (de-duped, order-preserving). Nothing more — per ADR, no guessing,
  no coercion. Unit/date normalization deferred until we have a
  second source motivating them. 3 tests.

### Pipeline module registration

- `crates/pipeline/src/lib.rs` — registered `recipe_apply`.

## PdfTable and the demo

Because `PdfTable` is intentionally `NotImplemented`, the Phase 3c.4
demo should target a non-PDF source. A CSV API (World Bank, FAO,
EIA) or a JSON API is the natural choice. USGS / PDF sources unblock
when positional PDF table extraction is built as its own session.

## To run on your side

```bash
cargo check --workspace
cargo test -p situation_room-pipeline     # ~35 tests including recipe_apply
```

No new live (`#[ignore]`) tests in this patch — the apply runtime is
LLM-free by design, so there's nothing to hit xAI for.
