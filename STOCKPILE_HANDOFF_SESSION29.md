# Stockpile — Session 30 handoff

**Status at the end of Session 29:** Track C (pdf_table extraction)
is **complete as a code drop** and ready for `cargo check` +
`cargo test --workspace` + `npm run check`. ADR 0007 amendment 5 is
appended. With pdf_table wired, every variant of the closed
extraction-mode enum (CsvCell, JsonPath, CssSelect, RegexCapture,
PdfTable) is a first-class wired runtime path. Adding a sixth mode
remains an ADR-level decision per ADR 0007's runtime-path section.

The patch is at `~/Downloads/stockpile_session29_track_c.tar.gz`.
Apply with the standard incantation from the repo root:

```
tar -xzf ~/Downloads/stockpile_session29_track_c.tar.gz --strip-components=1 -C .
```

Then verify with `cargo check`, `cargo test --workspace`. There are
no frontend changes in this patch (TS DTOs unchanged; PdfTable
recipes were already authorable, the wire shape was already in
place — what changed is the runtime arm). `npm run check` should
be a no-op.

Expected baseline pre-Track-C was ~471 tests green (Session 28
close). This session adds ~17 new tests:

- `recipe_apply::tests` — 7 unit tests for
  `split_on_runs_of_whitespace` and `detect_pdf_tables` (4 + 3
  respectively); 6 fixture-backed tests for `extract_pdf_table`
  (happy path, header row, zero page, page out of range, table
  not found, row out of range, col out of range, invalid PDF —
  that's actually 8); minus 1 test retired (the
  `pdf_table_returns_not_implemented_with_clear_reason` test).
- `recipe_apply::tests` — 1 end-to-end happy-path test
  (`end_to_end_pdf_recipe_produces_observation`); 1 end-to-end
  failure test (`end_to_end_pdf_recipe_fails_cleanly_when_address_is_out_of_range`);
  minus 1 retired (the `end_to_end_pdf_recipe_fails_cleanly_with_not_implemented`
  test).
- `fetch_executor::tests` — 1 happy-path PDF executor test
  (`run_fetch_for_plan_succeeds_against_pdf_recipe_without_calling_llm`);
  1 PDF apply-failure test
  (`run_fetch_for_plan_reports_apply_failure_on_pdf_with_out_of_range_address`);
  minus 1 retired (the
  `run_fetch_for_plan_skips_unwired_extraction_modes` canary).

Net: +17 tests added, –3 tests retired (their coverage role is met
by individual mode tests). New baseline: **~485 tests green**.

If anything fails, the lesson from Sessions 27/28 applies: read
what failed before doing anything else.

---

## STEP 0 — APPLY AND VERIFY (5 minutes)

```
cd /Users/aben/RustroverProjects/stockpile
tar -xzf ~/Downloads/stockpile_session29_track_c.tar.gz --strip-components=1 -C .
cargo check --workspace
cargo test --workspace
cd apps/desktop && npm run check
```

**Expected:** zero compile errors, ~485 tests green, zero TS errors.

Two things to specifically verify:

1. `pdf-extract` resolves and the function `extract_text_from_mem_by_pages`
   is in 0.7.x. The workspace pins `pdf-extract = "0.7"` (since
   Session 1, never consumed until now). The function is present in
   the latest pdf-extract (0.10.0); it should also be in 0.7.10 since
   the public API has been stable across the 0.7.x → 0.10.x window.
   **Fallback if it's not:** bump the workspace `Cargo.toml`'s
   `pdf-extract = "0.7"` to `pdf-extract = "0.10"` (one line, no
   other code changes needed — same function names, same
   `Result<Vec<String>, OutputError>` return type). The lockfile
   will resolve cleanly.

2. `cargo test -p situation_room-pipeline recipe_apply`. The PDF
   tests use `include_bytes!` against the fixture at
   `crates/pipeline/tests/fixtures/pdf/lithium_production.pdf`. If
   the fixture didn't extract from the tarball cleanly, every PDF
   test fails with a compile-time `include_bytes!` error rather
   than a runtime test failure — distinctive enough to be obvious.

---

## STEP 1 — HUMAN-LOOP TEST WITH A REAL USGS PDF (15 minutes)

This is the actual evidence step. The fixture is synthetic; a real
USGS Mineral Commodity Summaries chapter PDF is the natural human-
loop verification.

1. Pick a USGS MCS chapter with a clear single-table layout — most
   commodity chapters have a "World mine production and reserves"
   table on page 2 or 3 (e.g.
   `https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf`).
2. In the situation room, classify a topic that nominates `usgs_mcs`
   ("global lithium production" works).
3. Accept the plan. Run fetch.
4. Inspect the report:
   - Does the LLM author against the HTML companion (`mcs2024-lithium.html`)
     per the prompt's "HTML first" guidance? **This is the expected
     happy path** — the new pdf_table runtime is a *fallback*, not
     the default.
   - If the LLM authors against the PDF directly (which can happen
     when the source descriptor's `endpoint_hint` is the PDF URL),
     verify the recipe runs through `run_pdf_recipe` and produces
     records.
5. **Document the outcome** under `docs/failure_cases/` if anything
   surprising surfaces. Specifically of interest:
   - PDFs where pdf-extract's text-extraction order diverges from
     reading order, breaking the row-positional addressing.
   - PDFs where the table has merged cells that terminate the
     heuristic's table detection partway through.
   - PDFs where the LLM authored a `pdf_table` recipe but the
     addressed cell was empty.

These are Class C ("structurally PDF-table-table-but-wrong-shape")
patterns. They feed into ADR 0007's "what does and doesn't fit the
closed vocabulary" thinking.

---

## STEP 2 — Track C follow-ups (½ session each, in priority order)

### C.1 — Live test of pdf_table through xAI

The patch ships unit tests against the synthetic fixture. It does
**not** ship an `#[ignore]` live test that classifies a real plan,
authors a real PDF recipe, fetches a real USGS MCS PDF, and
asserts ≥1 observation. Such a test is worth adding once the
human-loop verification (Step 1) has shaken out edge cases. Mirror
`live_classify_topic_against_xai_produces_valid_plan`'s shape with
a fixture URL pointing at a known-stable USGS chapter.

### C.2 — Recipe-feedback channel for `pdf_table` failures

When `pdf_table` fails at apply time on a real PDF (most likely
due to merged cells or extraction-order drift), the operator's
remediation today is "drop the recipe and re-author against HTML
or static_payload." The recipe-feedback channel from ADR 0013 is
the natural way to surface this: the operator types "the row count
on page 2 is 5 not 4 because the header is two-line; address row
3 not row 2" and re-authoring picks up the note. The plumbing
already exists; the UI hook is small.

### C.3 — Multi-cell PDF extraction

The current `pdf_table` returns a single cell per recipe — same
as every other extraction mode. For PDFs that table out an entire
column (e.g. all countries × production for a year), the recipe
author has to write N recipes addressing N rows. This is not a
defect (it mirrors CSV's single-cell-per-recipe shape) but it does
mean PDFs are more recipe-heavy than they could be. A future
session might consider a `pdf_table_column` or `pdf_table_row`
extraction mode (ADR-level, sixth-of-the-closed-enum decision)
that returns a `Vec<String>` and produces multiple records.
Defer until empirical evidence shows the per-cell shape is a
real friction point.

### C.4 — Approach (b) for PDFs the heuristic can't address

If a meaningful number of real PDFs surface that approach (a)
(layout heuristic) can't address but approach (b) (positional
glyph clustering via pdf-extract's `OutputDev`) could, that's the
trigger to extend `extract_pdf_table` with a glyph-clusterer
fallback. Today's amendment 5 explicitly defers this; the gate is
empirical (count failures, look for the pattern that distinguishes
"would have worked with (b)" from "neither works").

### C.5 — Carried forward from Session 28

- Live xAI test of the decline path (Step 3, Track B follow-up).
- Schema regeneration check in CI.
- Recipe-feedback channel for declines.
- Anthropic provider and others are stubs.
- Apply-runtime strict deserialization is permissive.
- Authoring latency is 30–60s (xAI gateway, not us).
- Crate-level `#![allow(...)]` lint suppressions still need a
  sweep across crates that aren't `api`.

---

## What landed this session — file inventory

### Pipeline (`crates/pipeline`)

- `Cargo.toml` — `pdf-extract = { workspace = true }` added with
  comment block referencing ADR 0007 amendment 5.
- `src/recipe_apply.rs`:
  - Module docstring updated to reflect "all five modes wired."
  - `ExtractionSpec::PdfTable` arm in `extract` now dispatches to
    `extract_pdf_table` instead of returning
    `ApplyError::NotImplemented`.
  - `ApplyError::NotImplemented` retained on the public enum
    with a docstring noting it's currently unused (kept for
    backward-compat; removing it would be a breaking change).
  - New `extract_pdf_table(bytes, page, table_index, row, col)`
    function. Page is 1-indexed per the schema; zero is rejected
    explicitly. Each addressing failure (bad PDF bytes, page out
    of range, table not found, row/col out of range, empty cell)
    surfaces as `ApplyError::Extraction { mode: "pdf_table" }`
    with a coordinate-specific message.
  - New `detect_pdf_tables(page_text)` helper. Algorithm: split
    into lines, tokenize each via `split_on_runs_of_whitespace`,
    a *table* is a maximal contiguous run of lines whose token
    counts are all equal and ≥ 2. Empty lines and single-token
    lines terminate the current table. Single-row clusters do
    not become tables.
  - New `split_on_runs_of_whitespace(s)` helper. Runs of 2+
    whitespace separate cells; single spaces stay inside a cell
    so `"United States   1234"` → `["United States", "1234"]`.
  - 7 new unit tests on the helpers + 8 new fixture-backed tests
    on `extract_pdf_table` + 1 new end-to-end happy-path test +
    1 new end-to-end failure test. The
    `pdf_table_returns_not_implemented_with_clear_reason` test
    is retired (its coverage role is met by the new tests).
- `src/fetch_executor.rs`:
  - Module docstring updated to reflect "all five modes wired,"
    drift from the old "remaining mode is PdfTable" prose
    cleaned up.
  - `RecipeOutcome::Declined` docstring updated to remove the
    stale "pdf_table not yet wired" example.
  - `run_one_recipe`'s `PdfTable` arm now dispatches to
    `run_pdf_recipe` instead of returning
    `RecipeOutcome::Skipped`.
  - New `run_pdf_recipe` function, structurally identical to
    `run_regex_recipe` (and the other wired arms). Track A
    apply-failure capture path included.
  - `run_fetch_for_plan_skips_unwired_extraction_modes` canary
    test retired. Replaced with
    `run_fetch_for_plan_succeeds_against_pdf_recipe_without_calling_llm`
    (happy path) and
    `run_fetch_for_plan_reports_apply_failure_on_pdf_with_out_of_range_address`
    (apply failure + Track A attempt-row capture verified).
  - New `working_pdf_recipe` test helper.
- `tests/fixtures/pdf/lithium_production.pdf` — synthetic 2-page
  PDF fixture. Page 1 is filler prose; page 2 is a clean
  Country/Production table with 4 rows × 2 cols. 2,115 bytes.
- `tests/fixtures/pdf/README.md` — fixture provenance,
  regeneration script (reportlab), instructions for swapping in
  a real USGS PDF.

### Documentation

- `docs/adr/0007-research-function.md` — Amendment 5 appended
  with full rationale: what changed, the layout-heuristic vs
  glyph-clustering choice, what the amendment does NOT do, the
  failure modes accepted, and the code references.

---

## Known gaps and follow-ups

- **The synthetic fixture is not a real USGS PDF.** It exercises
  the same code path (multi-space-separated columns in a Courier-
  rendered PDF) but a real USGS PDF is the natural human-loop
  verification. Step 1 above is the path to converting the
  synthetic fixture into a real one if desired; the tests are
  shape-agnostic so swapping the fixture only requires updating
  the addressing in the assertions.
- **No live xAI test of the pdf_table runtime.** Mirrors the
  Track B follow-up shape from Session 28.
- **pdf-extract's text-extraction order is not guaranteed to
  match reading order on every PDF.** This is a known limitation
  of the underlying library, not a defect in this amendment.
  When it bites, the recipe sees rows out of order; the operator
  remediates by adjusting `row` or switching modes.
- **The `ApplyError::NotImplemented` variant is currently dead
  code.** Removing it is a breaking API change; the docstring
  notes it's reserved for a future hypothetical extraction mode
  that ships its enum variant before its runtime. ADR 0007's
  closed-enum invariant means this should remain hypothetical.
- **Carried forward from Session 28:**
  - Live xAI test of the decline path.
  - Schema regeneration check in CI.
  - Recipe-feedback channel for declines.
  - Anthropic provider and others are stubs.
  - Apply-runtime strict deserialization is permissive.
  - Authoring latency is 30–60s (xAI gateway, not us).
  - Crate-level `#![allow(...)]` lint suppressions sweep.

---

## What this session did not change

- The 6 record types stay 6.
- The two-level LLM architecture (classifier → recipe author) is
  unchanged.
- The closed extraction-mode enum stays at 5. **Wiring `pdf_table`
  in the runtime did not add a sixth mode** — it wired an existing
  fifth-of-five to an actual extractor. The closed-enum invariant
  is intact.
- ADR 0009's security posture is unchanged. PDF bytes flow
  through the existing `SecureHttpClient` fetch path; the
  layout-heuristic extractor takes those bytes by reference and
  returns a string. No new HTTP path, no new secret surface, no
  new external network dependency (pdf-extract is pure Rust + lopdf,
  no system libs, no Ghostscript, no Tabula/JVM).
- The recipe-author prompt's "HTML first" guidance is unchanged.
  PDFs are still the fallback when HTML doesn't exist; with
  `pdf_table` wired, the fallback is now functional, not
  aspirational. v1.9 of the prompt remains the production prompt.
- The xAI provider integration is unchanged.
- ADR 0012's gate (10 documented Class B failures) is unchanged.
  This amendment does not move it. PDF-shape failures that the
  layout heuristic can't handle are Class B (recipe ran and
  failed at apply); they count toward the gate. Class C
  (structurally undoable, e.g. scanned PDFs) remains for the
  decline channel.

---

## One thing to look at before any new code

The layout-heuristic algorithm in `detect_pdf_tables` is
deliberately simple: a table is a maximal contiguous run of
non-empty lines whose token-counts match and are ≥ 2. There is
**no** column-position alignment check, **no** font-size analysis,
**no** per-cell trimming beyond `trim()`. If you find yourself
about to add any of these, **first ask whether the failure shape
that motivates the addition is rare enough to remediate with a
different recipe (a different `table_index`, a regex_capture
fallback) instead of growing the heuristic.** The closed-enum
posture means the heuristic should stay narrow; complexity
belongs at the recipe-author layer, where the LLM picks the right
mode for the source.

End of handoff.
