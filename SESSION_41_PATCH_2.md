# Session 41 — Patch 2 of N

Item 2 from the Session 41 handoff: HTML structural digest in the prefetch excerpt.

## Apply

From the repo root, with patch 1 already applied:

```
tar -xzf ~/Downloads/stockpile_session41_p2.tar.gz --strip-components=1 -C .
```

Then:

```
cargo build --workspace
cargo test --workspace
cd apps/desktop && npm run check
```

## Files changed

- `crates/pipeline/src/fetch_executor.rs` (only file with code changes)
- `config/prompts/recipe_author.md`

`recipe_apply.rs` and `recipe_author.rs` are unchanged from patch 1 — item 2
adds authoring evidence, not a new validator. The patch-1 validator
(`validate_recipe_against_bytes`) keeps running against the same raw bytes
the digest was built from; the digest is purely an authoring aid.

## What this patch does

### Three-way prefetch dispatch

`prefetch_excerpt` now branches:

1. PDF (existing, patch 1) → `render_pdf_text_with_tables`
2. HTML (new) → `render_html_digest`
3. Anything else → existing `from_utf8_lossy` raw-bytes fallback

`is_html` sniffs by magic only — `<!DOCTYPE` (case-insensitive) or `<html`
(case-insensitive), with optional UTF-8 BOM and leading whitespace. JSON,
XML, RSS, SVG, and HTML fragments without a wrapping `<html>` all fall
through to the raw-bytes branch. Conservative on purpose: a false
positive would produce a misleading digest.

JSON sources still go through `from_utf8_lossy`. Item 3 lands the JSON
shape outline in patch 3.

### Digest shape

For an HTML response, the LLM now sees:

```
--- HTML structure (parsed by scraper) ---
<title>: Federal Reserve - H.4.1 Statistical Release
<h1>: H.4.1 Statistical Release

Tables:
  <table id="balance-sheet" class="data-table"> (15 rows × 8 cols)
  <table class="footnote"> (3 rows × 2 cols)

Lists:
  <ul class="navigation"> (12 items)
  <ol> (5 items)

Repeating element classes (iterator-eligible):
  div.card: 8 occurrences
  span.value: 24 occurrences

--- Visible text (script/style excluded, truncated) ---
H.4.1 Statistical Release Reserve Balances Held with the Federal Reserve...
```

Every shape in the digest is what `scraper::Html::parse_document` actually
parsed — the same parser `recipe_apply::extract_css_select` uses at apply
time. By construction, a `tag.class` selector the LLM reads off the digest
is one the runtime will find at apply time. The Session 40 Fed H.4.1 class
of failure (LLM authored `table#balance-sheet td.value` against markup
that didn't contain a `#balance-sheet` table) was already caught at
authoring time by patch 1's validator; with the digest it's caught earlier
— at the LLM's input, not at its output — and the Decline is "I cannot
find the data here" rather than "my selector didn't match."

### Per-section caps

Each subsection has its own bound:

| Section            | Cap                      |
|--------------------|--------------------------|
| Title              | 1024 bytes               |
| Each `<h1>`        | 1024 bytes               |
| Tables listed      | 50                       |
| Lists listed       | 50                       |
| Repeating classes  | 30                       |

A pathological page (1 MiB title, 10000 tables, 10000 lists) cannot crowd
the digest's other subsections out of view. Whatever budget remains after
the structure summary is spent on visible text; if the structure alone
exhausts the budget the visible text is replaced with an explicit
"[... structure summary consumed the budget; visible text elided]" line.

### Visible text rendering

`collect_visible_text` walks the body's element tree and emits text-node
content from non-script/non-style/non-noscript subtrees. A modern web
page can carry hundreds of KiB of inline JavaScript or CSS — relevant
for executing the page, not for authoring an extraction recipe. Excluding
those subtrees keeps the visible-text section focused on what an end-user
would read.

### Prompt update

Two principle-only edits in `recipe_author.md`:

1. The `css_select` bullet is extended with guidance to author against
   the elements listed in the `--- HTML structure (parsed by scraper) ---`
   section, and to use the `Repeating element classes` subsection for
   iterator candidates.
2. The `Document excerpt` section gets a new paragraph framing the
   PDF and HTML cases under one principle: "the framing is the runtime's
   view of the bytes, not a separate interpretation; a coordinate or
   selector you read off the excerpt is one the runtime will use
   unchanged."

No source-specific routing was introduced. No URL-pattern matching, no
"if the host is X" branches, nothing in the prompt that names a specific
publisher.

## Tests

Seven new tests in the existing test module of `fetch_executor.rs`:

- `is_html_recognizes_standard_html_markers` — DOCTYPE, html, leading
  whitespace, UTF-8 BOM, case insensitivity.
- `is_html_rejects_non_html_payloads` — PDF, JSON array/object, CSV,
  XML/RSS, plain `<note>` chevron-leading text, empty/short inputs.
- `render_html_digest_surfaces_title_h1_table_and_list_shapes` — pins
  exact format strings for title, h1, table-with-classes-and-id, ul, ol
  against an inline fixture.
- `render_html_digest_surfaces_repeating_tag_class_selectors` — verifies
  the `tag.class: N occurrences` format and the N>1 filter.
- `render_html_digest_excludes_script_and_style_subtrees_from_visible_text`
  — uses unique tokens (UNIQUE_SCRIPT_TOKEN, etc.) to assert script/style/
  noscript content is not in the visible-text section.
- `render_html_digest_handles_empty_body_gracefully` — SPA shell case
  (`<body><div id="root"></div></body>`); digest still emits its header
  but reports no tables / no lists, prompting the LLM to decline.
- `render_html_digest_truncates_visible_text_when_budget_is_small`
  — 1 KiB budget against 10 KiB of body text; truncation marker must be
  present.

Plus a new integration test
`prefetch_excerpt_for_html_url_yields_structural_digest_to_recipe_author`
that mirrors the PDF integration test from patch 1: walks the full
prefetch + propose-URL + recipe-author retry loop and asserts the
recipe-author prompt carries the digest header, the title from the
fixture, the table's id attribute in the table listing, and the
`div.card: 2 occurrences` repeating-class line. Same shape as patch 1's
`prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author`.

## How this changes the live runs

The Session 41 patch-1 lithium MCS run produced 7 declines, several of
which were HTML-shape sources (World Bank landing page, IEA report
landing page). Their declines named the exact shape problem ("Source
excerpt is report landing page HTML without structured tabular/JSON
data") — which the digest will now confirm or contradict at the LLM's
input layer. A landing-page-only source will show empty `Tables:` and
empty `Lists:` sections in the digest, making the decline more direct
and faster. A source that *does* have tabular HTML will surface those
tables in the digest with their `(rows × cols)` shape, and the LLM can
author against the parsed structure instead of guessing.

Net effect on the lithium MCS plan: I expect the IEA / World Bank /
EUR-Lex-shaped sources to either decline with better attribution
(empty digest → no structure to extract) or produce successful
recipes (digest surfaces a real table → LLM authors against confirmed
markup → patch-1 validator confirms it would extract).

The PDF-snapshot truncation issue I noted in the patch-1 run report
(USGS MCS at page 110, snapshot covers ~first 8-10 pages) is **not**
addressed by this patch. That's a separate finding for Session 42 and
deserves its own architectural treatment — likely a 2-pass authoring
loop where the LLM's first pass identifies which page range to fetch
in detail.

## Out of scope (still, per the handoff)

- Item 3 (JSON shape outline) — patch 3.
- Item 6 (json_path authoring-time validation) — patch 3, lands with
  item 3. Patch 1 already established that the validator dispatch is
  mode-agnostic, so item 6 needs no new validator code.
- Item 7 (xAI tier discipline) — patch 4.
- The PDF-snapshot truncation finding from the patch-1 lithium MCS
  run — Session 42.
- SEC 403 / Reuters / industry.gov.au network issues — Session 42
  per the handoff's hard rule.
