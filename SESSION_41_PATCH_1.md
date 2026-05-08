# Session 41 — Patch 1 of N

Items 1 + 4 + 5 from the Session 41 handoff.

## Apply

From the repo root:

```
tar -xzf ~/Downloads/stockpile_session41_p1.tar.gz --strip-components=1 -C .
```

Then:

```
cargo build --workspace
cargo test --workspace
cd apps/desktop && npm run check
```

I cannot run cargo here, so the first thing you'll do on receipt is
share the output of those commands. If anything fails to compile,
expect a small follow-up patch.

## Files changed

- `crates/pipeline/src/recipe_apply.rs`
- `crates/pipeline/src/fetch_executor.rs`
- `crates/pipeline/src/recipe_author.rs`
- `config/prompts/recipe_author.md`

## What this patch does

### Item 1 — Framed-table PDF prefetch

`render_pdf_text` is replaced by `render_pdf_text_with_tables`. The
new function calls `recipe_apply::detect_pdf_tables` (made
`pub(crate)`) on each page's extracted text and frames each detected
table as

```
[PDF page 2, table 0] (3 rows × 2 cols)
  row 0 (col 0..1): "Country"  "Production"
  row 1 (col 0..1): "Australia"  "88000"
  row 2 (col 0..1): "Chile"  "49000"
```

Pages where the detector finds nothing tabular emit
`[PDF page N] (no table detected)` followed by the page's narrative
text (capped per-page at 4 KiB so a single text-heavy page can't
crowd out tables on later pages).

By construction, the row and column numbers the LLM reads off the
framed headers are the same numbers `extract_pdf_table` will index
into at apply time — same library, same detector, same coordinates.
The lithium MCS class of failure from Session 40 (LLM authored
`row=11` against a detected table that had 2 rows) is closed.

The kind annotation in the excerpt header changed from
`PDF (text extracted)` to `PDF (text + detected tables)` so an
operator scanning logs can tell the new format apart from the old.

### Item 5 — Authoring-time validation: pdf_table

After `build_validated_recipe` returns a candidate `pdf_table`
recipe, `author_recipe` runs the runtime's own
`extract_pdf_table` against the prefetched bytes (via the new
`recipe_apply::validate_recipe_against_bytes`). A recipe whose
coordinates exceed the detected table is converted to
`AuthoringError::Declined` rather than persisted — the recipe is
never written to disk and the operator sees a single Decline at
authoring instead of an apply failure on every fetch forever.

### Item 4 — Authoring-time validation: css_select

Same shape, applied to the css_select path. Scalar `css_select`
recipes run through `extract_css_select`; iterator-bearing recipes
(Phase 1 css_select × css_select) require the outer iterator to
match ≥1 element AND the inner extraction to match within at least
one outer scope. Non-Phase-1 iterator pairings surface the same
`NotImplemented` the runtime would, but at authoring time so the
recipe is never persisted. The Session 40 Fed H.4.1 class of
failure (LLM hallucinated `table#balance-sheet td.value` against
markup that didn't contain a `#balance-sheet` table) is closed.

### Plumbing

- `prefetch_excerpt` now returns `Option<(String, Vec<u8>)>` so the
  caller can hand the same bytes to validation that the LLM saw in
  the excerpt.
- `author_recipe` gains an `original_bytes: Option<&[u8]>`
  parameter. `None` skips validation (test paths, legacy callers).
- `reauthor_recipe` passes `Some(fetched_bytes)` through.
- The api crate's call to `reauthor_recipe_impl` is unchanged
  (it already passes the fetched bytes by reference).

### Prompt

`config/prompts/recipe_author.md` — the `pdf_table` mode docs and
the "Strategy for PDF sources" section are updated to describe the
framed-table format. **Principle-only language.** No source-specific
routing was added; the prompt teaches what to look for in the
framed excerpt, not which URLs go where.

## Tests

Old PDF tests in `fetch_executor.rs` decimated with comments
explaining why; replacements pin the new framed format against the
lithium fixture. The integration test
`prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author`
asserts the recipe-author prompt carries the new framed-table
header for the lithium fixture's data table on page 2.

New tests in `recipe_apply.rs` for the validator:

- `validate_recipe_pdf_table_against_lithium_fixture_accepts_in_range_coords`
- `validate_recipe_pdf_table_against_lithium_fixture_rejects_out_of_range_row`
- `validate_recipe_css_select_rejects_selector_that_matches_nothing`
- `validate_recipe_css_select_accepts_selector_that_matches`
- `validate_recipe_css_iterator_accepts_when_outer_and_inner_match`
- `validate_recipe_css_iterator_rejects_when_outer_matches_but_inner_does_not`
- `validate_recipe_css_iterator_rejects_when_outer_matches_nothing`
- `validate_recipe_json_path_inherits_runtime_null_skip_contract` —
  a forward pin documenting that wiring up json_path validation
  (item 6, patch 3) requires no new code in
  `validate_recipe_against_bytes` because the dispatch is mode-
  agnostic by construction.

## Out of scope (deliberately, per the handoff)

- Item 2 (HTML structural digest) — patch 2.
- Item 3 (JSON shape outline) — patch 3.
- Item 6 (json_path authoring-time validation) — lands with item 3
  in patch 3, since the validator dispatch already covers it.
- Item 7 (xAI tier discipline) — patch 4 (config-only, ships last
  so any compile drift from items 1–6 is settled).

## What to expect

Live runs of the lithium MCS source from Session 40 should now
either produce a successful pdf_table recipe (the LLM has the
runtime's row/col counts, not its own visual count) or an honest
authoring-time Decline naming the structural reason. The "fail at
apply forever" outcome from Session 40 should not recur.

The same applies to css_select recipes (Fed H.4.1 class) — the
authoring-time validator catches selectors that match nothing in
the prefetched HTML before the recipe is persisted.
