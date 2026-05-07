# ADR 0016 — Extraction iterator: representing listing-shaped sources

**Status**: Accepted
**Date**: 2026-05-07
**Accepted**: 2026-05-07 (Session 38 — Phase 1 implementation complete)
**Related**: ADR 0007 (research function: two-level LLM
architecture, especially the closed-extraction-vocabulary
discipline), ADR 0003 (six record types as governance boundary),
ADR 0014 (recipe provenance), ADR 0015 (LLM-emitted source
nominations)

---

## Context

Session 37's first cold-start live run exposed a contract gap that
had been latent in the codebase since ADR 0007 closed the
extraction vocabulary. The plan
`019e024e-3e2e-7f71-9311-59883a97f229` ("quantum computing hardware
roadmaps") nominated five sources; two recipes succeeded end-to-end
(`qt.eu` and `www.nature.com`), each producing exactly one Event
record. Inspection of those records — by querying the `events`
table directly during Session 37's debug pass — confirmed the
shape:

```
nature.com event.headline = "Nanoscale 'conveyor belt' teleports quantum
                             state of electron"
qt.eu event.headline      = "Strategic Advisory Board …"
```

Each headline is a real, leaf-level, semantically correct
extraction from one card on a listing page. The recipes themselves
are well-authored — `h3.c-card__title a` against the Nature
subjects page is the right selector, sitting at the right
specificity level. The runtime is consistent with its current
contract: `css_select` returns the *first* matching element, and
the runtime stored one record from one match.

The defect is structural, not local to either recipe. The Nature
subjects page lists ~30 articles. The qt.eu newsroom lists ~10
items. Each card on each page is plausibly one
`milestone_announced` event under the plan's vocabulary, and the
plan's `event_types` bucket was sized to be populated over a
730-day window — a timeline shape, not a single point. The recipes
captured one of N items per source; the other N-1 are
unrepresented in storage with no record of their omission.

### Why this didn't surface earlier

Pre-ADR-0015, the source registry was static and skewed toward
single-instance API endpoints — World Bank indicator URLs (`{ value:
[ ... ] }` with one path per metric), Eurostat datasets (one JSON
envelope per query), USGS PDFs (one table per page). The closed
extraction vocabulary's uniform scalar shape happened to match the
shape of the registered sources, so the gap stayed invisible.

ADR 0015 changed the source-emission distribution. The classifier
now nominates URLs from training-distribution authoritative
knowledge, and that knowledge skews heavily toward listing pages:
arXiv recent-papers feeds, IEEE Xplore search results, BBC RSS,
Nature subjects pages, USPTO patent search, news beats, agency
publication indexes. The cold-start quantum run was the first
classification where every nominated URL was a listing — the gap
that was structurally always there became operationally dominant
in one run.

### What's in the closed vocabulary today

[`crates/pipeline/src/recipes.rs::ExtractionSpec`](../../crates/pipeline/src/recipes.rs)
defines the closed enum (annotated):

```rust
pub enum ExtractionSpec {
    JsonPath    { path: String },                              // returns one value
    CssSelect   { selector: String, attribute: Option<String> }, // returns first match
    CsvCell     { column: String, row_filter: Option<RowFilter> }, // returns one cell
    PdfTable    { page: u32, table_index: u32, row: u32, col: u32 }, // returns one cell
    RegexCapture { pattern: String, group: u32 },              // returns first capture
}
```

All five modes are uniformly *scalar*. There is no verb in the
vocabulary for "iterate." A recipe targeting a listing URL captures
1 of N items, deterministically, with no error and no warning. The
runtime treats this as a successful extraction because — at the
extraction level — it is one.

### What this is *not*

This ADR does not threaten the six record types (ADR 0003). A
listing of N articles produces N records of the same record type
(Event for news listings, Document for paper listings, etc.). The
six-type governance boundary is unchanged.

This ADR does not threaten the LLM-free runtime invariant (ADR
0007's golden rule). Iteration is deterministic — for each match
of an iterator selector, apply the field_mappings. No per-record
LLM call. The recipe-author boundary stays where it is.

This ADR does not address pagination (the listing fetched today
shows 30 cards; the source's archive may have hundreds). Pagination
is a separate concern, deferred to a future ADR.

## Decision

Add **one orthogonal concept at the recipe level: an iterator**.
When present, the runtime evaluates the iterator's extraction spec
to obtain N matches, then evaluates the recipe's existing
`extraction` field once *per match* (scoped to that match's
sub-tree), producing one record per match per `produces` binding.
When absent, the recipe behaves exactly as today.

The iterator is *not* a sixth extraction mode. It is one new
optional field on `FetchRecipe` whose value is *itself* an
`ExtractionSpec` drawn from the existing closed enum. The closed
vocabulary stays five modes, plus one orthogonal "iterate?" axis.

### The shape

```rust
pub struct FetchRecipe {
    // ...existing fields unchanged...
    pub source_id: String,
    pub source_url: Url,
    pub method: HttpMethod,
    pub headers: Vec<HttpHeader>,
    pub static_payload: Option<String>,

    /// Existing field. In iterator mode, this is evaluated *per
    /// match* against the sub-tree the iterator selected. In
    /// non-iterator mode, it evaluates against the whole document
    /// (today's behaviour, unchanged).
    pub extraction: ExtractionSpec,

    /// NEW. When `Some`, the runtime evaluates this spec against
    /// the fetched document to obtain N matches, then evaluates
    /// `extraction` once per match scoped to that match's sub-tree,
    /// producing N records per `produces` binding. When `None`,
    /// the recipe produces exactly one record per binding (today's
    /// contract).
    ///
    /// The iterator's mode and the recipe's `extraction` mode must
    /// be congruent: a `css_select` iterator pairs with a
    /// `css_select` extraction (the inner selector evaluates
    /// against the matched node's sub-tree); a `json_path`
    /// iterator pairs with a `json_path` extraction (the inner
    /// path evaluates against the matched value); etc. Cross-mode
    /// pairings are rejected at recipe-author validation.
    pub iterator: Option<ExtractionSpec>,

    pub produces: Vec<ProductionBinding>,
    // ...
}
```

### Per-match evaluation semantics, by mode

| Iterator mode  | Inner extraction mode | Per-match scope                              |
|----------------|----------------------|----------------------------------------------|
| `css_select`   | `css_select`         | The matched DOM node's sub-tree              |
| `json_path`    | `json_path`          | The JSON value at the matched path           |
| `csv_cell`*    | `csv_cell`           | One CSV row (header retained for column ref) |
| `regex_capture`| `regex_capture`      | The full text of the iterator's match        |

*The CSV iterator case is a structural extension of the existing
`row_filter` mechanism: instead of "find the one row matching this
filter, return one cell," it becomes "iterate every row (or every
row matching a filter), apply the inner extraction per row." The
iterator's mode is logically `csv_rows` rather than `csv_cell`; see
"Naming" below.

PDF table iteration (every row of a table → one record) is a
natural extension but defers to a Phase 2 ADR — the PDF-table
fixture corpus needs broader testing first.

### Naming

The iterator field's `ExtractionSpec` value is *the same enum* as
the recipe's main extraction. Naming is preserved: `css_select`
for both, `json_path` for both, etc. The runtime distinguishes
"this is an iterator selector, take all matches" from "this is an
inner selector, take the first match within scope" purely by
*position* in the recipe (`recipe.iterator` vs
`recipe.extraction`), not by mode discriminator.

This means the closed-enum surface count stays at 5. The CSV case
is the one minor wart: today's `CsvCell` semantically returns one
cell; an iterator over CSV needs row-level matching, not
cell-level. Two clean fixes:

1. **Reuse `CsvCell` with `column` ignored at iterator position.**
   The iterator selects rows; the `column` field is meaningless on
   an iterator (it'll be applied per-row by the inner extraction).
   Validation rejects a non-empty `column` on the iterator.
2. **Add a sixth `CsvRows { row_filter }` variant.** Cleaner, but
   nudges the enum count up by one and loses the "iterator and
   inner extraction are the same shape" symmetry.

This ADR adopts (1) for symmetry. The validation rule "iterator's
`CsvCell.column` must be empty string" is documented in the recipe
author prompt and enforced at `build_validated_recipe` time.

### A worked example — the Nature recipe under this proposal

Today's recipe (one record):
```json
{
  "extraction": { "mode": "css_select", "selector": "h3.c-card__title a" },
  "iterator":   null,
  "produces": [{
    "expectation": { "list": "event_type", "index": 0 },
    "record_type": "event",
    "field_mappings": [
      { "path": "event_type", "source": { "kind": "literal", "value": "milestone_announced" } },
      { "path": "headline",   "source": { "kind": "extracted" } }
    ]
  }]
}
```

Under this ADR (N records):
```json
{
  "extraction": { "mode": "css_select", "selector": "h3.c-card__title a" },
  "iterator":   { "mode": "css_select", "selector": ".c-card" },
  "produces": [{
    "expectation": { "list": "event_type", "index": 0 },
    "record_type": "event",
    "field_mappings": [
      { "path": "event_type", "source": { "kind": "literal", "value": "milestone_announced" } },
      { "path": "headline",   "source": { "kind": "extracted" } }
    ]
  }]
}
```

The single difference is the new `iterator` field. The runtime now
evaluates `.c-card` against the page (returning ~30 matched DOM
nodes), then for each node evaluates `h3.c-card__title a` within
that node's sub-tree (returning that card's title link), and
produces 30 Event records — each with the same literal `event_type`
and a per-card `headline`.

## Two-phase implementation

### Phase 1 (Session 38) — single extracted field per match

Implement the iterator concept end-to-end with the constraint that
each `produces` binding has one extracted value per match (today's
single-extraction contract, applied per-iteration). This handles
the Nature/qt.eu/RSS/arXiv listing cases that motivated the ADR:
each card has one headline, one title, one paper name. The literal
fields and the one extracted field per binding are sufficient for
the dominant case.

### Phase 2 (deferred — separate ADR) — multi-extracted fields per match

When real listings need multiple extracted leaves per record
(headline + date + author from one news card, title + abstract +
arxiv-id from one paper card), the field_mappings need per-field
extractor sub-specs. This is a richer change that touches recipe
author validation, the runtime, the prompt, and the UI's recipe
inspection panel. It deserves its own ADR after Phase 1's contract
has run in production for a few cycles.

The Session 38 prompt revision should explicitly *not* attempt
multi-field per-match — the LLM should be taught to author a
single extracted field per binding under iterator mode, with all
other per-record fields as literals.

## Consequences

### Positive

- Listing-shaped sources become natively addressable. The Nature
  cold-start recipe under the new shape produces 30 records
  instead of 1; qt.eu produces ~10; RSS/Atom feeds produce per-item
  records; arXiv `quant-ph/recent` produces per-paper records.
- The closed extraction vocabulary stays at 5 modes. The new
  `iterator` field is structurally orthogonal — it does not extend
  the enum, it composes with it.
- The two-level LLM architecture is preserved. The LLM authors one
  recipe (now possibly with an iterator); the runtime applies it
  deterministically; no per-record LLM call.
- The six record types are unchanged. N matches produce N records
  of the same record type; the type selection still happens at
  recipe-author time per `produces` binding.
- Recipes that *don't* set `iterator` behave exactly as today —
  the change is fully additive. Existing recipes in storage and
  pre-Session-38 plans continue to work unchanged.

### Negative / costs

- Storage shape changes: the `extraction` JSON column on `recipes`
  now optionally carries an `iterator` sibling. No migration is
  needed if the column is JSON (which it is per migration 0003);
  Rust serde handles `Option<ExtractionSpec>` cleanly via
  `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  Older recipe rows missing the field deserialize with `iterator:
  None`, the correct semantics.
- Recipe-author prompt grows a section. The LLM must learn when
  the URL is a listing (populate `iterator`) vs an instance (omit
  it). v1.13 of `recipe_author.md` carries the new guidance plus a
  worked example.
- Per-match dedup becomes load-bearing. With one record per match,
  the natural-key discipline must include something stable per
  card (the headline, the article URL, the paper id) — not just
  per-recipe identity. This ADR notes the requirement; the dedup
  fix has a sibling defect (events table currently writes
  `dedup_key = NULL`; see Session 37 handoff §carry-forward) that
  needs addressing in the same session or one immediately after.
- Per-match record count is *unbounded* by the recipe shape. A
  listing page with 10000 items would produce 10000 records per
  fetch. Phase 1 imposes a hard runtime cap (suggested:
  `MAX_RECORDS_PER_RECIPE = 500`) and surfaces overflow as a
  `RecipeOutcome::Failed { stage: Apply, message: "iterator
  produced N matches; cap is 500" }` so the operator sees the
  truncation rather than getting silently partial data.
- Iterator + inner extraction mode-congruence is enforced at
  validation. A `css_select` iterator with a `json_path` inner
  extraction is rejected at `build_validated_recipe` because the
  per-match scope is a DOM sub-tree, not a JSON value. Same for
  every cross-mode combination.

### Alternatives considered

**(a) Five `_each` companion modes (`css_select_each`,
`json_path_each`, etc.)** Doubles the closed vocabulary's surface
count, conflates iteration with extraction at the type level, and
forces the LLM to choose between paired modes when the choice is
mechanical (always pair like with like). Rejected for surface
inflation and the conceptual conflation.

**(b) A `cardinality: { many | one }` flag on the existing
extraction.** Looks minimal but is structurally wrong: the
iterator and the per-match extraction are *different selectors*
operating at *different scopes* (the iterator selects card
boundaries; the inner extraction selects leaves within cards).
Collapsing them into one extraction with a cardinality flag forces
the runtime to invent the inner-vs-outer semantics from the same
selector string, which doesn't compose. Rejected as
under-specified.

**(c) Per-record LLM invocation at extraction time.** The LLM
sees N matches, picks the best one, or invents per-match
extractions. Violates ADR 0007's golden rule (runtime is LLM-free
once recipes exist) and introduces unbounded LLM cost per fetch.
Rejected categorically.

**(d) Decline-only at the recipe-author prompt.** Teach the LLM:
"if the URL is a listing, decline." This is a useful interim while
the iterator lands — it converts silent narrowness into honest
scarcity — but it does not recover the records. The Nature plan
under decline-only would have zero events from Nature (vs one
today, vs ~30 under this ADR). Useful as a prompt-language patch
between this ADR's acceptance and its implementation; not the
permanent answer.

### Carry-forward dependencies

Two existing defects in the events storage path become operationally
visible in iterator mode and must be addressed in the same session
or the one immediately after:

1. **`dedup_key` is NULL on events today.** Confirmed via Session 37
   debug query. With one record per recipe, the absence of dedup_key
   means re-fetching produces duplicate records but the duplication
   rate is bounded (one duplicate per fetch per source). Under
   iteration, the duplication multiplies by N. The fix is for the
   recipe-author prompt and validator to require a `dedup_key_field`
   (one of the field_mappings' paths) when iterator is present, and
   for the runtime to compute `dedup_key` per-record from that
   field's extracted value.

2. **`record_derived_from` table has 0 rows.** The provenance DAG
   linking records to the recipes that produced them isn't being
   written. Today this is recoverable via the
   `source_id = "host#recipe:<uuid>"` substring (also confirmed in
   Session 37 debug). Under iteration, with N records sharing one
   recipe, the recipe→records edge becomes a more important
   first-class concept and `record_derived_from` should carry it.

These two are noted here as ADR-level dependencies; they are not
the ADR's primary subject. They block iteration's Phase 1 from
being honestly shippable; they do not block iteration's design.

## Validation

This ADR is empirically falsifiable: re-classify "quantum computing
hardware roadmaps" against the post-Session-38 codebase, accept the
plan, run a fetch. The Nature recipe and the qt.eu recipe should
each produce N>1 Event records, with `headline` values matching the
N visible cards on the live pages at fetch time. If either produces
1 record, or if the headlines are concatenated, or if the count
differs from the page's visible card count by more than ±1 (for
async-loaded cards we don't see), the implementation is wrong.

## Status

**Accepted** (2026-05-07, Session 38). The decision was ratified by
the operator at the end of Session 37 with the framing "the work is
one level lower, in the extraction vocabulary, and the move is one
new concept (iterator) composed orthogonally with the existing five
modes." Phase 1 implementation landed in Session 38: storage column
(migration v15), typed pipeline plumbing, runtime path for
`css_select × css_select`, validator with four invariants (mode
congruence, csv_cell-iterator-column-empty, dedup_key_field-required-
on-iterator, dedup_key_field-references-real-path), DTO surface,
recipe-author prompt v1.13, and a live ignored test against a real
listing. Phase 2 (multi-extracted fields per match, JSON / CSV / PDF
iterator runtime) defers to its own ADR after Phase 1 has run in
production for a few cycles.

End of ADR.
