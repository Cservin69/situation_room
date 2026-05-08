# Session 40 — apply instructions

Patch shape: tarball of files to overwrite. Apply from the repo root:

```
tar -xzf ~/Downloads/session40.tar.gz --strip-components=1 -C .
```

The `--strip-components=1` strips the leading `./` from the archive
entries; the directory layout inside the archive matches the
workspace exactly so files land at the right paths.

## What this patch does

Two bugs Session 39 left behind, plus the prompt-side truth-up.

### Bug 1 — duplicate decline source_ids (the visible symptom)

`derive_source_id_for_decline` was taking the first 8 hex chars of
the nomination's UUIDv7 (`&s[..8.min(s.len())]`). UUIDv7's first 48
bits are entirely the millisecond Unix timestamp, and the
classifier mints all of one plan's nominations in one tight loop
— well under a millisecond — so every decline in a plan came back
with the same source_id (`nom:019e06b0` repeated seven times in
the live titanium-supply-chain run; you can verify in your own
logs by grepping `source_id=nom:`).

Two consequences:

1. The frontend's `FetchReport.svelte` keys its outcomes list
   with `declined:<source_id>`. Duplicate keys → Svelte 5 throws
   `each_key_duplicate` → the outcomes list does not paint. Your
   "looks identical before and after Run Fetch" symptom was the
   panel stuck on its summary header while the body refused to
   render.
2. Recipe-feedback (ADR 0013) keys on `(plan_id, source_id)`, so
   flagging one declined nomination flagged all of them. The
   flag-from-decline channel was unusable for any plan with >1
   decline.

The fix swaps the prefix for the full nomination_id. `recipe_feedback.source_id`
is `TEXT NOT NULL` with no length cap, and the API command
bounds-checks against `Bounds::URL` (2 048 chars), so the longer
string passes through unchanged. Log scannability is preserved by
the existing `position` + `total` fields on every per-nomination
log line.

A regression test (`decline_source_ids_are_unique_across_nominations`)
builds a plan with five nominations, mints them back-to-back to
preserve the same-millisecond invariant, runs the all-decline
path, and asserts pairwise-distinct source_ids. The existing
`declined_source_surfaces_as_declined_outcome` test gets a length
assertion added (40 chars = `nom:` + 36-char UUID) so a future
session can't accidentally reintroduce a prefix.

### Bug 2 — recipe-author was being shown raw PDF binary

The handoff identified Issue A as "wire `pdf_table` from
`NotImplemented` to actually extract." That framing is stale:
`recipe_apply::extract_pdf_table` was already wired in Session 29
(ADR 0007 amendment 5). The actual gap was at *authoring* time.

Before this patch, `prefetch_excerpt` fed PDF bytes through
`String::from_utf8_lossy` — i.e., a wall of `0xEF 0xBF 0xBD`
replacement chars with no readable structure. The recipe-author
LLM correctly declined every PDF-bearing source it ever met with
"the excerpt is a binary PDF dump, no extractable structure."
That reason is verbatim in your titanium-supply-chain logs against
USGS MCS PDFs.

The fix sniffs `%PDF-` magic at offset 0 of the prefetched bytes,
runs them through `pdf_extract::extract_text_from_mem_by_pages`
(the same library and same per-page reader the runtime apply path
uses), joins the result with `[PDF page N]` markers (1-indexed,
matching the `pdf_table.page` recipe coordinate the runtime
expects), and feeds the readable text to the LLM. The runtime
apply path is unchanged — it still re-fetches the original bytes
and runs `extract_pdf_table` on them at apply time. The two paths
use the same pdf-extract library so what the LLM sees at authoring
time is byte-for-byte the text the runtime will index into when
the recipe runs.

Failures of pdf-extract (encrypted, malformed, exotic glyphs)
fall through to a clear "could not extract text" annotation in
the excerpt; the LLM declines rather than authoring against
garbage. The 32 KiB excerpt budget now applies to the rendered
body length (PDFs can balloon ~3× from raw bytes to extracted
text), char-boundary safe so the truncation can't slice mid-
codepoint.

Three new tests against the existing `LITHIUM_PDF` fixture pin
the behaviour:

- `is_pdf_recognizes_pdf_magic` — the byte-sniffer.
- `render_pdf_text_against_lithium_fixture_emits_page_markers_and_table_text`
  — `[PDF page 1]` marker + `Country` / `Production` / `Australia`
  / `Chile` / `Argentina` cell values appear in the rendered text,
  no `\u{FFFD}` replacement chars.
- `render_pdf_text_surfaces_errors_for_non_pdf_bytes` — pdf-extract
  errors don't get silently swallowed.

A fourth end-to-end test
(`prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author`)
walks `run_fetch_for_plan` with a `PromptCapturingProvider`
mirroring the existing `DecliningProvider` pattern. It asserts the
recipe-author prompt the executor finally builds carries the
`[PDF page 1]` marker + table cells + the `PDF (text extracted)`
annotation in the excerpt header. This is the test that would
have caught the "raw PDF binary in the LLM prompt" regression
end-to-end.

### Prompt truth-up

`config/prompts/recipe_author.md` had two stale claims:

- Line 631-632 (paraphrasing): "the closed extraction vocabulary's
  `pdf_table` mode exists for this case but is not yet wired in
  the runtime."
- Line 672 (paraphrasing): "PDF-only sources become addressable
  without a `pdf_table` runtime."

Both were lies as of Session 29. Both came out.

The "First move: hunt for the HTML equivalent" subsection listed
filename patterns for USGS MCS, SEC EDGAR, EUR-Lex — that's
exactly the source-specific routing your standing rule prohibits
(your Session 34 push, captured in the rule book: principle-only
language; never bake "if URL contains X, do Y"). I rewrote the
section to teach the same principle (prefer HTML when both
formats publish the same data, because CSS-selector authoring
against semantic markup is deterministic on the markup tree
whereas pdf_table depends on positional column alignment that the
typesetter could break) without naming any specific source.

The `pdf_table` mode docs at the top of the closed-vocabulary
list now explain the `[PDF page N]` marker convention and the
multi-word-cell limitation honestly, so the LLM knows exactly
what it is looking at when the prefetch returns extracted PDF
text.

The "Worked example — HTML found" anchored on USGS-specific URL
patterns is gone. The "Worked example — HTML absent, bake the
values" stays (its URLs are fictional, principle is generic). The
"Anti-example — bake when HTML exists" was de-source-named and
generalized.

## Hard rules carried over from the rule book

- Six record types. No seventh.
- Closed enum of N extraction modes. This patch does NOT add a
  mode (the handoff floated `xlsx_cell` as one option; it stays
  for a future session). It wires authoring-time visibility for
  an existing mode.
- ADR 0009: every HTTP call goes through `SecureHttpClient`. The
  new `pdf_extract` call operates on bytes already fetched
  through that path.
- TS files in `apps/desktop/src/lib/api/types/` not touched (no
  DTO change in this patch — the wire-shape source_id field is
  already a `String` and storage is already string-keyed; only
  the contents change).
- Components only use CSS vars from `global.css`. No frontend
  change in this patch.
- L1 prompt unchanged.
- ADR 0013 (recipe-feedback channel) honored — keying on the
  full nomination_id makes the channel work *per-nomination*
  for the first time, which is what the ADR's "(plan_id,
  source_id)" surface always intended.

## Files touched

- `crates/pipeline/src/fetch_executor.rs`
  - `derive_source_id_for_decline` — full nomination_id, not
    8-char prefix. Docstring documents the UUIDv7 timestamp
    clustering trap.
  - `prefetch_excerpt` — branches on PDF magic, calls
    `render_pdf_text`, char-boundary-safe truncation.
  - New private helpers `is_pdf`, `render_pdf_text`.
  - Existing test `declined_source_surfaces_as_declined_outcome`
    — comment updated, length assertion added.
  - New tests:
    `decline_source_ids_are_unique_across_nominations`,
    `is_pdf_recognizes_pdf_magic`,
    `render_pdf_text_against_lithium_fixture_emits_page_markers_and_table_text`,
    `render_pdf_text_surfaces_errors_for_non_pdf_bytes`,
    `prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author`.

- `config/prompts/recipe_author.md`
  - `pdf_table` mode docs at the top of the closed-vocabulary
    list — multi-word-cell + page-marker explanation.
  - "Strategy for PDF sources" section rewritten — no
    "not yet wired" lie, no source-specific routing rules.

No DTO regen, no frontend change, no migration.

## What to verify after applying

1. `cargo build --workspace` — should compile cleanly. The new
   tests use only types already in scope in the existing tests
   module (`CompletionRequest`, `CompletionResponse`, `LlmError`,
   `LlmProvider`, `ModelTier`, `async_trait`, `LITHIUM_PDF`,
   `StaticFetcher`, etc.).

2. `cargo test --workspace` — all five new tests should pass.
   The existing `declined_source_surfaces_as_declined_outcome`
   test gains a length assertion that will fail loudly (40 vs
   <40) if anyone partially reverts the source_id fix.

3. `cd apps/desktop && npm run check` — should be clean. No TS
   change.

4. Live re-run of "titanium supply chain": every nomination still
   declines (the closed extraction vocabulary genuinely cannot
   address SEC 403s, Reuters paywalls, IEA SPAs — those need
   network-layer fixes that are out of scope for this patch),
   but:
   - Each decline now has a unique `nom:<full-uuid>` source_id,
     so the FetchReport panel renders the seven outcome rows.
   - The USGS MCS attempt should reach the recipe-author with
     `[PDF page N]` markers in the excerpt rather than binary
     garbage. If the LLM authors a `pdf_table` recipe against it,
     the runtime will fetch the PDF and index into the table at
     apply time. If it still declines (multi-word cells, etc.),
     the decline reason will be more honest than the old "binary
     PDF dump" boilerplate.

If the FetchReport panel still doesn't render after the source_id
fix, that's a frontend reactivity bug separate from this patch
and worth digging into in Session 41 (the existing diagnosis path
the handoff sketched applies). But based on the source_id
collision and the each-key-duplicate failure mode of Svelte 5,
the symptom should resolve here.
