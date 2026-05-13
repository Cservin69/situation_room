# Session 44 — Patch 1

Piece B from the Session 43 handoff: PDF prefetch truncation gap.
Single-pass design (per operator's `go B but in one big run not in
two`), reframed during implementation away from explicit outline
parsing toward implicit navigation through the framed-table list
itself — see "Architecture" below for why.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

No new dependencies. No schema changes. No migration. The change is
contained in the prefetch helper, the prefetch budget constant, and
the recipe-author prompt's PDF strategy section.

## Files changed

- `crates/pipeline/src/fetch_executor.rs` —
  - `PREFETCH_EXCERPT_BUDGET` bumped from `32 * 1024` to `64 * 1024`,
    with a multi-line rationale comment naming the binding constraint
    (PDFs, not HTML/JSON/raw) and the calculation behind 64 KiB.
  - `render_pdf_text_with_tables`: the no-table branch is reduced
    from `marker + up to 4 KiB of page narrative + truncation
    notice` to `marker only`. The dropped `PER_PAGE_NARRATIVE_CAP`
    constant and the 30-line narrative emission block are gone.
  - Function-level rustdoc gains a new `**Why narrative is dropped
    (Session 44).**` paragraph explaining the budget calculus, the
    lithium MCS truncation gap that motivated it, and what falls
    out for the value-lives-in-narrative edge case.
  - `prefetch_excerpt`'s rustdoc updates the one sentence that
    described the no-table branch as "followed by the raw page text"
    to point to the new function-rustdoc rationale.
  - Two new unit tests (see "Tests" below).
- `config/prompts/recipe_author.md` —
  - Header bumped `v1.13` → `v1.14`.
  - "Strategy for PDF sources" section: the bullet describing the
    no-table branch as `[PDF page N] (no table detected)` followed
    by narrative text is rewritten to describe the new shape (marker
    line and nothing else). A new paragraph follows explaining that
    the framed-table list across the document IS the navigation
    index — page numbers in headers, row-0 column-header cells
    naming the table — so the LLM picks the right page/table by
    scanning the list rather than reading prose around it. The
    existing "do not author against no-table pages" rule is
    preserved (validator still rejects); the "use that text only to
    decide *whether*..." sentence is gone since there is no longer
    any text to decide against.
  - New `v1.14` changelog entry naming Session 44 and the prefetch
    truncation gap.

No other files were touched. No `Cargo.toml` edits, no new crates,
no schema or DTO changes, no recipe migration, no prompt placeholder
additions, no test fixture changes.

## Architecture

**The truncation gap.** Session 41 patch 1 added framed-table PDF
prefetch — every detected table on every page emits a
`[PDF page N, table M] (R rows × C cols)` header followed by quoted
row cells, in the runtime's coordinate space, so the LLM authors
`pdf_table` recipes against bytes byte-for-byte identical to what
the runtime will index at apply time. That worked architecturally
but Session 41's patch-1 lithium MCS run surfaced a downstream gap:
on PDFs longer than ~8–10 pages, the LLM saw the (often early-page)
TOC, identified the page it needed (page 110 for the lithium
chapter), and the prefetch excerpt's 32 KiB ceiling cut off
everything past page ~8 before the framed table on page 110 ever
reached the LLM. Authoring-time validation (Session 41 patch 1) then
honestly declined every recipe targeting page 110 because the bytes
the LLM saw didn't extend that far. Honest decline, but a wall the
system cannot get past on long PDFs.

**Where the budget went.** The 32 KiB ceiling was not the root
cause; the root cause was the *per-page narrative cap* on no-table
pages. Pre-Session-44 the no-table branch followed its marker with
up to 4 KiB of the page's text so the LLM could decide *whether*
the value lived on that page. On a typical regulatory or
statistical PDF — many narrative-heavy pages between data tables —
those 4 KiB blocks consumed the entire budget on the first 8 or so
pages and framed tables on later pages never reached the prompt.
The narrative was helping the LLM *navigate*; that navigation
benefit cost the LLM access to the data it would actually author
against.

**The fix.** Single pass, two moves, both uniform across every PDF:

1. **Drop the per-page narrative on no-table pages.** Pages without
   a detected table now emit one line — `[PDF page N] (no table
   detected)` — and nothing else. Pages with detected tables emit
   their framed-table headers and rows as before. The asymmetry
   (table pages already had no narrative pre-Session-44) flips: now
   no-table pages have nothing and table pages have the framed
   data.
2. **Bump `PREFETCH_EXCERPT_BUDGET` from 32 KiB to 64 KiB.** A
   110-page PDF with ~70 framed-table pages (the lithium MCS
   shape) renders to ~37 KiB total: 70 × ~500 B framed plus 40 ×
   ~50 B markers. 64 KiB carries that comfortably and leaves
   headroom for ~200-page documents. 32 KiB does not. The bump is
   uniform across all four prefetch branches (PDF, HTML, JSON, raw
   bytes); HTML/JSON/raw were not the binding constraint and
   simply inherit the headroom.

**Navigation, post-Session-44.** The LLM no longer reads narrative
prose to decide which page covers which topic. Instead it scans the
framed-table list — every `[PDF page N, table M] (R rows × C
cols)` header inlines its page number, and each table's row 0
typically names the table (column headers like `"Country",
"Production"` for a stat table; `"Date", "Decision", "Vote"` for a
press release). Across the whole PDF the framed-table list reads
like a structural index: dense, page-numbered, named-by-headers.
The recipe-author prompt's "Strategy for PDF sources" section is
edited to say so explicitly (v1.14 changelog entry).

**Why no explicit outline / TOC parsing.** The handoff named two
viable architectures for piece B: two-pass authoring (LLM nominates
a page range from a TOC, prefetch refetches that slice with bigger
budget, second pass authors the recipe) and TOC-aware excerpting
(prefetch parses the PDF's outline metadata and samples the pages
it points at). The operator picked single-pass with `go B but in one
big run not in two`, ruling out two-pass.

The natural reading of TOC-aware excerpting is "parse `lopdf`'s
outline, render the entries at the top of the excerpt, sample
pages from the outline's targets." Implementing it surfaces three
problems:

1. **Outline parsing is brittle.** Many PDFs lack outline
   metadata entirely (the producer never authored one); the
   fallback path then has to do something else for navigation,
   creating a two-shape architecture.
2. **Sampling pages from the outline is heuristic.** "Sample first
   N KiB of each chapter" is a hand-coded adapter for "shape of
   PDF with TOC"; "match topic to chapter title" is exactly the
   LLM's job (and the kind of source-specific routing the operator
   has flagged across sessions). Either is the failure mode.
3. **Cost vs. benefit.** An explicit TOC at the top of the excerpt
   costs ~1 KiB in the budget and adds a `lopdf` direct
   dependency. The framed-table list already inlines page numbers
   and names tables via row-0 column headers, so the navigation
   benefit of a separate TOC block is small.

Single-pass-with-implicit-navigation is the simpler version of the
operator's pick. The framed-table list IS the TOC in everything
that matters: page numbers, structural shape, named-by-cells. The
patch ships that version.

**No source-specific routing anywhere.** The two moves apply to
every PDF identically:

- Drop narrative → uniform structural rule keyed on `tables.is_empty()`,
  not on URL host, not on document class.
- Budget bump → constant, applied across every prefetch branch.
- Prompt edit → describes the new format every PDF source will hit,
  no named sources or document classes.

The vigilance the operator applied earlier in the session (`are we
again hardcoding sources?`) audits cleanly here: no `if host == X`
branch, no per-source narrative cap, no "if PDF looks like a USGS
commodity summary, sample chapter N" heuristic, no `match host`
on PDF byte content. The page-1 / page-N asymmetry is structural
(table presence), not source-specific.

## Tests

Two new unit tests in `fetch_executor.rs`'s test module:

- `render_pdf_text_with_tables_drops_narrative_on_no_table_pages_session_44`
  — pins the new shape: between the page-1 no-table marker and the
  page-2 marker that follows, only inter-page padding (`\n\n`) may
  appear. Anything else is narrative leaking through. Uses the
  existing 2-page lithium fixture; page 1 is a title-only page with
  no detected table, so the test exercises the no-table branch
  directly.
- `prefetch_excerpt_budget_is_at_least_64kb_session_44` — pins the
  budget floor at 64 KiB. A future session that lowers this without
  re-architecting the PDF excerpt format reintroduces the
  truncation gap; the test names the architectural reason and
  points at the doc-comments that must update before the floor
  moves.

The existing
`render_pdf_text_with_tables_emits_no_table_marker_when_detector_finds_nothing`
test continues to pass — it asserts only that the no-table marker
appears for page 1, which the new shape preserves; the dropped
narrative was not part of its assertions.

The existing
`render_pdf_text_with_tables_against_lithium_fixture_emits_framed_tables`
test continues to pass — table-bearing-page rendering is
unchanged.

The existing
`render_pdf_text_with_tables_surfaces_errors_for_non_pdf_bytes`
test continues to pass — the malformed-PDF error path is
unchanged.

No HTML / JSON / raw-bytes tests are affected; their per-branch
rendering is unchanged. They inherit the larger budget but the
existing fixtures (small inline HTML / JSON documents) fit well
under 32 KiB and certainly under 64 KiB, so the truncation
behavior they exercise still triggers when expected and not when
not.

The pre-existing live integration tests against PDF sources (when
run with real network and a PDF longer than 8 pages) inherit the
new shape automatically; the recipe author's authoring-time
validation runs against the same bytes regardless of the excerpt
shape, so any recipe that validates pre-Session-44 still validates
post-Session-44, and recipes that previously declined for "page
out of excerpt range" reasons may now succeed because the page
they targeted reaches the prompt.

## What to expect

For short PDFs (≤ ~8 pages of mixed content), the excerpt is
slightly smaller post-Session-44 because narrative on no-table
pages is gone. The LLM has the same framed tables it had before,
plus markers for any narrative pages.

For long PDFs (> ~8 pages with tables anywhere past page 8), the
excerpt is fundamentally different: it now spans the whole document
in framed-table form rather than the first 8 pages plus a
truncation marker. The LLM picks the page and table by scanning
the framed-table list. Recipes that previously declined because
the targeted page was beyond the budget cut should now succeed —
the bytes are in the prompt, the LLM's coordinate authoring runs
against them, and authoring-time validation confirms against the
same bytes.

The recipe-author prompt's behavior on no-table pages is unchanged
in policy (don't author `pdf_table` against pages declaring no
table) but tightened in description. The validator at apply time
hasn't moved; recipes that target a no-table page still get
rejected pre-persistence, same as Session 41 patch 1.

## Out of scope (still / carried)

- Network-layer issues from Session 40 (SEC user-agent placement,
  Reuters defunct-or-blocked, `industry.gov.au` timeouts) — Session
  45+ per the handoff's hard rule that this is its own session.
- xAI Responses API migration — only architecturally necessary if a
  live `grok-4.3` run shows chat/completions silently ignoring the
  `reasoning_effort` parameter Session 43 patch 1 plumbed.
- An explicit `[TOC]`/outline block parsed via `lopdf` — see
  "Architecture / Why no explicit outline parsing" above for why
  this didn't ship as part of B. If a future session finds a PDF
  whose framed-table list is genuinely insufficient for navigation
  (e.g. a 200-page document where all tables are near-identical
  and the LLM cannot disambiguate without prose context), that
  session can revisit the trade-off; the architectural change
  surface would be additive (a new outline-rendering helper above
  the framed-table list).
