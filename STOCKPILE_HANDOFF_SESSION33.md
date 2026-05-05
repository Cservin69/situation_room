# situation_room — Session 33 followup

**Trigger:** Session 32b shipped the JSONPath null-skipping fix and
predicted "the next layer will be visible at messy errors." Today's
live run (hungarian barley production, replayed against a fresh
classify-and-fetch chain) produced exactly that.

The xAI gateway responded as expected; both recipes were authored,
both ran, both failed — but the **failure shapes** are new:

- `usgs_mcs`: `Failed @ Apply` with a ~12 KB error message. The LLM
  authored against real fetched bytes (`authored_from="fetched_bytes"`
  in the log), so the *endpoint* is fine. But the page is the
  USGS Mineral Commodity Summaries landing page — a navigation page,
  not data. The recipe's `css_select` extraction pulled the entire
  rendered body — including a `<noscript>` block whose text content
  is a literal `<iframe ...>` HTML fragment plus a Drupal page-state
  JSON embedded near the bottom — and content assembly tried to
  coerce 12 000 characters of HTML into the observation's
  `value: f64` field. The full string ended up in
  `RecipeOutcome::Failed.message`, which then propagated to the run
  log line, the `recipe_fetch_attempts.error_summary` column, and
  the desktop UI's recipe panel — rendering each illegible.

- `sec_edgar`: `Failed @ Fetch` with `403 (with response headers)`.
  This is **a different problem** — see "What this patch is NOT"
  below. It needs its own session.

Session 33 fixes the first failure mode at the runtime layer. The
underlying *upstream* issue — the LLM picked a navigation page
instead of a data endpoint — is a prompt/source-descriptor concern
that 32b's discipline ("no prompt changes per session unless
explicitly the focus") carries forward. The runtime catches what
the LLM gets wrong; ADR 0007 calls that out explicitly. The catch
just needs to be readable.

## What this patch does

A single-file, ~150-line change to `crates/pipeline/src/recipe_apply.rs`,
adding two surgical safeguards plus their tests:

### 1. `EXTRACTED_SCALAR_MAX_BYTES` bound at every extractor exit

A new module-level constant (`2048`) caps the byte size of any
extracted scalar. After each of the five extractors
(`extract_json_path`, `extract_css_select`, `extract_csv_cell`,
`extract_pdf_table`, `extract_regex`) computes its result, a tiny
helper `bound_extracted(out, mode)` checks the size and either
passes the value through or returns a small named
`ApplyError::Extraction` with a 120-char preview, the size, the
bound, and the structural diagnosis ("the selector matches a
container element, or the JSON path resolves to an object/array
instead of a scalar").

The bound is generous: an event title plus a multi-paragraph
description fits well under 2 KB; a formatted price, a numeric
observation, a regex capture all fit trivially. A 12 KB HTML body
or a multi-page JSON document does not.

Why 2 KB rather than 1 KB or 512 B: the next-bigger thing recipes
legitimately produce is a verbose Event description. Empirically
those land in the few-hundred-byte range, but the bound is a
contract not a budget — leave headroom so a recipe authored for
a future Event type with a longer narrative field doesn't get
tripped by a runtime decision that was conservative-by-default.

Document records aren't producible from recipes (`build_record`
already rejects them per ADR 0007), so no recipe-extracted field
legitimately needs the large-body affordance. The bound applies
uniformly across all five modes.

### 2. `truncate_content_assembly_reason` for residual oversized errors

The bound at extraction is the primary defense, but it doesn't
cover the case where `parse_extracted_scalar` keeps a moderately-
sized non-numeric string (say 300 chars) and content assembly
then fails because the field type is `f64`. `serde_json` quotes
the offending value back into its error message; even at 300
chars that's enough to bury the actionable `expected f64` suffix
in log readers that wrap.

A second helper `truncate_content_assembly_reason(reason)` caps
the wrapped error message at 600 chars while preserving both ends:

- Head: ~368 chars — keeps `observation content: invalid type:
  string "<head>` so the operator sees the type mismatch and
  the start of the offending value.
- Tail: ~184 chars — keeps the `..., expected f64` suffix so the
  operator sees what was expected.
- Middle: a marker `… [value truncated, total N chars] …`
  signalling the elision and naming the original length.

Applied to the three `RecordType` arms (`Observation`, `Event`,
`Relation`) at the `serde_json::from_value(...).map_err(...)`
sites.

### Tests

Five new tests in the module, sitting after the existing
`css_select` block:

- `css_select_rejects_oversized_extraction` — proves the live
  failure mode produces a small named error.
- `json_path_rejects_oversized_extraction` — symmetric coverage
  for the JSON path mode (the same shape would appear if a path
  resolved to a JSON-stringified object).
- `bound_extracted_passes_typical_field_sizes` — sanity that
  legitimately-sized extractions (numbers, formatted prices, a
  600-char description) still pass through unchanged.
- `truncate_content_assembly_reason_passes_short_messages_unchanged`
  — proves the truncator is a no-op below the cap.
- `truncate_content_assembly_reason_preserves_head_and_tail` —
  with a 4 KB synthetic content-assembly error: the bound holds,
  the head and tail both survive, the truncation marker reports
  the original length.

That's +5 tests on top of Session 32b's +3.

## What this patch is NOT

- **Not a fix for `sec_edgar` 403.** That's a per-source
  User-Agent issue: SEC EDGAR's fair-use policy requires the
  request UA include a real contact email
  (`Sample Org name@domain.tld`), and `SecureHttpConfig::user_agent`
  defaults to `situation_room/{version} (+url)` — no email. The
  `config/sources.toml` descriptor for `sec_edgar` even claims the
  UA is "already enforced by SecureHttpClient" but live evidence
  contradicts that claim. The fix needs per-source UA capability,
  which is a config-shape change with knock-on effects (where do
  operator emails get configured? does the `.env` carry a
  `SEC_CONTACT_EMAIL`? does this become per-source HTTP-config
  generally?) — properly its own session, probably with an ADR.
  Until then, `sec_edgar` recipes will keep failing at fetch with
  a small clean error; the chip mechanic surfaces that, the
  operator can flag.

- **Not a prompt change.** The recipe author authored a recipe
  against a navigation page rather than a data page. That's a
  prompt concern. 32b's discipline carries: prompt revisions are
  empirical, prompted by observed classifications, and one-at-a-
  time. After this patch ships and the operator runs again, the
  *next* prompt revision can address "prefer data endpoints over
  landing pages" with concrete language anchored in the
  `usgs_mcs` example.

- **Not a `css_select` selector heuristic.** The runtime does not
  inspect the selector for "this matches body, that's too broad."
  Pattern-matching against the recipe is the kind of brittle
  shortcut ADR 0007 explicitly rejected — see SESSION2's "do not
  shortcut around ADRs" warning. The size bound is a clean
  general check that catches the symptom regardless of which
  extraction mode the recipe used.

- **Not a change to `parse_extracted_scalar`.** The "try as
  number, fall back to string" strategy is correct: a recipe that
  maps a non-numeric extraction into `value: f64` *should* fail
  loudly. The fix is in how loud the failure is allowed to be in
  the operator's view, not in the silence-vs-failure decision.

- **Not a change to the chip / re-author / flag flow.** Sessions
  25/31/32a built the right escalation surface; the failure mode
  the patch fixes was hidden behind a multi-KB error message that
  made the chip-and-flag UI illegible. With this patch, the chip
  mechanic does its job because the failure message fits in the
  panel.

## Apply

```bash
cd /Users/aben/RustroverProjects/stockpile  # or .../situation_room
tar -xzf ~/Downloads/situation_room_session33.tar.gz --strip-components=1 -C .
cargo test -p situation_room-pipeline recipe_apply::tests
```

Specifically the five new tests:

```bash
cargo test -p situation_room-pipeline recipe_apply::tests::css_select_rejects_oversized_extraction
cargo test -p situation_room-pipeline recipe_apply::tests::json_path_rejects_oversized_extraction
cargo test -p situation_room-pipeline recipe_apply::tests::bound_extracted_passes_typical_field_sizes
cargo test -p situation_room-pipeline recipe_apply::tests::truncate_content_assembly_reason_passes_short_messages_unchanged
cargo test -p situation_room-pipeline recipe_apply::tests::truncate_content_assembly_reason_preserves_head_and_tail
```

Plus a sanity sweep across the full `recipe_apply` test set:

```bash
cargo test -p situation_room-pipeline recipe_apply
```

If those pass, the patch is good. Then:

```bash
cargo build --workspace
```

— and re-run `hungarian barley production` (or any topic that
nominates `usgs_mcs`). The expected new state of the live failure:

- `usgs_mcs` still fails @ apply (the LLM picked the wrong page;
  prompt territory). But the failure message is now small and
  named: `extraction returned NNNN bytes; recipes produce single
  scalar values …` — fits in the recipe panel, fits in the run
  log line on one screen.
- `sec_edgar` still fails @ fetch with `status error: 403 (with
  response headers)` (separate session).
- The chip mechanic (ADR 0014) now has legible content to chip
  against. Flag/reauthor work as designed.

## Honest expectation-setting

The "0 records → some records" path needs both this layer and the
prompt revision that comes after it. This patch alone does not
produce records on the next run; it makes the *failure* legible,
which is the precondition for the operator to either flag-and-
reauthor or for the next session to ship a prompt change with
real evidence anchored in this run.

Sessions 30 → 32a → 32b → 33 form a coherent arc:

- 30 (Session 30 screenshot) revealed the wrong-endpoint pattern
  for `world_bank_indicators`.
- 32a fixed the endpoint hint and revealed the JSON-null pattern.
- 32b filtered nulls and revealed the wrong-page pattern for
  `usgs_mcs`.
- 33 (this) makes the wrong-page failure legible so a future
  prompt session can pattern-match against the resulting
  `extraction returned N bytes` chip-readable error.

Each layer is smaller than the last. The layer this exposes is
specifically a prompt concern — "teach the LLM that
`https://www.usgs.gov/.../mineral-commodity-summaries` is a
navigation page, not a data endpoint, and prefer a per-commodity
PDF chapter or a different mode" — which the chip mechanic plus
the now-readable error message gives operator-side feedback for.

## File inventory

### Pipeline

- `crates/pipeline/src/recipe_apply.rs`:
  - New module-level constants `EXTRACTED_SCALAR_MAX_BYTES` (2048)
    and `CONTENT_ASSEMBLY_REASON_MAX_CHARS` (600).
  - New helpers `bound_extracted` and
    `truncate_content_assembly_reason`, plus a top-of-file comment
    block linking the change back to today's live run.
  - `bound_extracted` applied at the success-return site of every
    extractor (`extract_json_path`, `extract_css_select`,
    `extract_csv_cell`, `extract_pdf_table`, `extract_regex`).
  - `truncate_content_assembly_reason` applied at the three
    `RecordType` arms in `build_record` that wrap
    `serde_json::from_value` errors.
  - Five new unit tests under `mod tests`, immediately after the
    existing CssSelect tests.

That is the entire patch. No other crate is touched. No DTOs
change. No migration. No frontend change. ts-rs codegen does not
need to re-run.

End of followup.
