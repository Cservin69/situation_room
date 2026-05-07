# STOCKPILE — Session 38 handoff

You are starting Session 38. Session 37 shipped ADR 0015 (the
LLM-emitted source nominations + sources-memory pivot) and proved
it end-to-end with a cold-start live classification and fetch. The
test surfaced one architectural gap that ADR 0016 now formalizes:
the closed extraction vocabulary is uniformly scalar, so listing-
shaped sources (Nature subjects pages, RSS feeds, arXiv recent,
news beats — exactly the kind of URLs cold-start classification
preferentially nominates) silently produce 1 record per recipe
when they ought to produce N.

Read this file. Read ADR 0016. Re-read ADR 0007 (especially the
"closed extraction vocabulary" section) and ADR 0014 (provenance
discipline that iteration leans on). Then look at
`crates/pipeline/src/recipe_apply.rs`'s existing `css_select`
implementation and the recipe shape in
`crates/pipeline/src/recipes.rs`. Don't write code until those
are loaded into your head.


## What works today (confirmed at end of Session 37)

- ADR 0015 cold-start classification produces real LLM-emitted URLs
  with priority tiers and (when memory has them) `known_id` stamps.
  Verified live: plan
  `019e024e-3e2e-7f71-9311-59883a97f229`, topic
  `quantum computing hardware roadmaps`, 5 nominations, all with
  `known_id=null` (genuine cold start), all prefetched as
  `authored_from="fetched_bytes"`.
- The five existing extraction modes work as specified: `css_select`
  returns first match (verified by direct DB query — both Nature and
  qt.eu events carry one verbatim headline from the first card on
  their respective listing pages); `csv_cell`, `json_path`,
  `pdf_table`, `regex_capture` all behave per their tests.
- Plan lifecycle (pending → accepted → fetched) end-to-end.
- The desktop app, the situation_room CLI, both classify and fetch
  paths, the recipes panel, the fetch report panel.

## What's broken / load-bearing for Session 38

Listing-shaped sources produce 1 record where the plan asked for N.
Operationally: the quantum-computing run produced 2 events (one
each from qt.eu and Nature) where it should have produced ~40
(roughly the number of visible cards across both listings). The
plan's `event_types` bucket was sized for a 730-day timeline; the
runtime delivered two points.

This is not new with ADR 0015 — it has been latent since the closed
extraction vocabulary was defined in ADR 0007 — but ADR 0015 made
it operationally dominant by shifting the source-emission
distribution toward listings.

## Session 38 priority

**Implement ADR 0016 Phase 1 end-to-end against one source on the
existing quantum-computing plan.**

The slice is deliberately narrow. Phase 2 (multi-extracted fields
per match) has its own ADR and its own session. Iteration over
PDF tables and CSV rows extends the same concept but defers to
later sessions once Phase 1's runtime semantics are proven against
the dominant case (HTML listings via `css_select`).

### Scope

- One iterator mode this session: `css_select` (the iterator
  selects N DOM nodes; the per-match extraction is also
  `css_select`, scoped to each matched sub-tree). The matching
  sentence in the validation logic needs to enforce mode
  congruence; the prompt teaches it.
- One existing source class: HTML listing pages already nominated
  by the post-ADR-0015 classifier. The Nature subjects page is the
  cleanest empirical anchor; qt.eu is a useful secondary case.
- One record type at a time: Event records first, since both live
  empirical cases produce events.
- Records-per-recipe cap: hard cap at 500 (suggested in ADR 0016
  §Consequences). Overflow surfaces as
  `RecipeOutcome::Failed { stage: Apply, message: ... }`.

### What "end-to-end" means here

1. ADR 0016's `iterator: Option<ExtractionSpec>` field lands on
   `FetchRecipe` in `crates/pipeline/src/recipes.rs`. Backwards
   compatible: existing recipes with no `iterator` field
   deserialize as `iterator: None` and behave exactly as today.
2. `recipe_apply::apply_recipe` learns the per-match evaluation
   path. When `recipe.iterator.is_some()`, evaluate the iterator
   to get `Vec<Match>`, then for each match evaluate
   `recipe.extraction` *scoped to that match's sub-tree* and run
   the field_mappings → produce one record per match per
   `produces` binding.
3. `build_validated_recipe` learns the iterator-specific validation:
   mode congruence (`css_select` iterator → `css_select` inner),
   `column` empty when `CsvCell` appears at iterator position
   (forward-compatible — the runtime doesn't yet exercise CSV
   iteration but the validator should reject the malformed case
   when the LLM produces it).
4. Recipe-author prompt (`config/prompts/recipe_author.md`) gains a
   new section teaching when to use `iterator`. v1.12 → v1.13.
5. The dedup story: when iterator is present, the recipe-author
   prompt requires a `dedup_key_field` referencing one of the
   `field_mappings` paths. The validator enforces presence and
   path-existence. The runtime computes per-record `dedup_key` from
   that field's extracted value (this also fixes the existing
   defect that events table writes `dedup_key = NULL` regardless of
   iteration — you'll be wiring it for the iterator case anyway).
6. Re-classify the quantum-computing topic against the new prompt;
   accept; run fetch; verify N>1 events per listing source. Live
   test (`#[ignore]`) added that does the same in code.

### Storage changes (crates/storage)

No migration needed. The `recipes.extraction` column is JSON
(per migration 0003). Adding `iterator` as a sibling field inside
that JSON serialises cleanly via serde's `skip_serializing_if =
"Option::is_none"`. Older recipe rows missing the field deserialize
to `iterator: None`.

The events table's `dedup_key` column already exists (migration
0001). What needs to happen there is the *runtime* writes a non-NULL
value for iterator-produced records — see §Pipeline changes.

If you find that iteration motivates writing `record_derived_from`
edges (recipe → record), that's the fix for the second
Session-37-flagged defect, but it's a clean separable change. Land
it in the same session if it fits; defer to Session 39 if it
expands the scope.

### Pipeline changes (crates/pipeline)

- `recipes.rs`: add `iterator: Option<ExtractionSpec>` to
  `FetchRecipe`. Add `dedup_key_field: Option<String>` to
  `ProductionBinding` (optional in non-iterator mode, required in
  iterator mode — enforced by validator, not type).
- `recipe_author.rs::build_validated_recipe`: extend validation:
  - When `iterator.is_some()`: validate mode congruence between
    iterator and extraction; require `dedup_key_field` on every
    binding; require `dedup_key_field` to reference a path that
    exists in `field_mappings`; reject `column` non-empty when
    iterator is `CsvCell` (forward-compatible guard).
  - When `iterator.is_none()`: existing validation, plus accept
    optional `dedup_key_field` (don't require it — pre-iteration
    recipes should still validate).
- `recipe_apply.rs::apply_recipe`: branch on `recipe.iterator`:
  - `None`: today's path, unchanged.
  - `Some(iter_spec)`: evaluate iter_spec → `Vec<Match>`; bound
    matches by `MAX_RECORDS_PER_RECIPE`; for each match, evaluate
    `recipe.extraction` scoped to the match, then evaluate
    field_mappings, then materialise one record per binding,
    computing `dedup_key` from the binding's `dedup_key_field`.
  - The "scope" abstraction needs care for `css_select`: the inner
    selector evaluates against the matched node's *own* sub-tree
    (use `scraper::ElementRef::select` rather than re-selecting on
    the document). For `json_path` (Phase 2), the inner path
    evaluates against the matched value as root.
- `fetch_executor.rs`: needs minor updates to handle N records per
  recipe in counting and reporting. `RecipeOutcome::Succeeded`
  already carries `records_produced: u32`; just propagate the
  iteration count instead of hard-coded 1.

### API changes (crates/api)

Minimal. The wire shape `RecipeDto` exposes the recipe's structure;
the new `iterator` field needs to land on the DTO with its own
ts-rs export (mirrors `ExtractionSpecDto` since the same enum is
reused). The frontend already handles `records_produced > 1` in the
fetch report panel — nothing structural to change there.

### Frontend changes

- `RecipesPanel.svelte`: render the optional iterator selector
  alongside the extraction selector when present. A small "× N"
  cardinality badge on the recipe card communicates "this recipe
  iterates."
- `EventsPanel.svelte` (if it exists in your branch's UI; otherwise
  the events bucket within `PlanReview`): no change needed — it
  already renders N records per type.
- The fetch report's per-source row already shows
  `{records_produced} records`; no change there.

### Tests to write

- `recipes.rs::tests::iterator_field_round_trips_through_serde` —
  shape-test the new optional field.
- `recipe_apply::tests::css_select_iterator_produces_n_records` —
  fixture: a small HTML page with 3 cards, each carrying an `h3`;
  iterator selects cards, inner selects `h3`; assert 3 records with
  the 3 `h3` texts.
- `recipe_apply::tests::iterator_caps_records_at_max` — fixture
  with 600 cards; assert the apply call returns Failed/Apply with
  the cap message, no records persisted.
- `recipe_author::tests::iterator_validates_mode_congruence` —
  reject a `css_select` iterator with `json_path` extraction.
- `recipe_author::tests::iterator_requires_dedup_key_field` —
  reject an iterator-bearing recipe with no `dedup_key_field` on
  any binding.
- `recipe_author::tests::dedup_key_field_must_reference_existing_path` —
  reject when the named field isn't in field_mappings.
- `fetch_executor::tests::run_fetch_with_iterator_recipe_produces_n_records` —
  end-to-end via a static fetcher fixture; one HTML body, 5 cards,
  iterator → 5 events persisted.
- Live: one `#[ignore]` test
  (`live_iterator_against_real_listing_produces_n_records`) that
  re-classifies the quantum-computing topic and asserts ≥10 events
  end up persisted from at least one source. Same shape as Session
  37's `live_classify_topic_against_xai_produces_valid_plan`; runs
  against real xAI + real Nature/qt.eu/etc. Reads `XAI_API_KEY`
  from env.

### Recipe-author prompt (`config/prompts/recipe_author.md` v1.12 → v1.13)

The prompt revision is small but consequential. Add a section under
"Closed extraction vocabulary" (or after "Endpoint discipline —
instance vs listing") teaching:

- The five extraction modes are scalar — each returns one value.
- A new optional field, `iterator`, makes a recipe produce N
  records from N matches. Use it when the URL is a listing of
  multiple items (a news index, a search results page, a subjects
  feed, an archive index).
- The iterator's mode must match the extraction's mode (CSS with
  CSS, JSON path with JSON path, etc.).
- When using `iterator`, every `produces` binding must specify
  `dedup_key_field` — one of the field_mappings' paths whose
  extracted value identifies the record across re-fetches.
- A worked example: a Nature subjects page. Show the iterator-
  bearing recipe shape from ADR 0016 §"A worked example". Add a
  contrasting non-iterator example (a single instance URL like a
  particular paper) so the LLM sees the contrast.

Keep the prompt principle-only. **Do not name specific URLs or
sources** in the new section. The Session 34 / Session 37 lesson
about prompt principles vs source-specific routing rules applies
in full force.

### Validation gates

- After step 1: `cargo test -p stockpile-pipeline --lib
  recipes::tests` green.
- After step 2: full pipeline tests green; the new iterator unit
  tests pass.
- After step 3: validator tests green.
- After step 5: prompt edits visually checked; v1.13 changelog
  entry added at top of file.
- After step 6: live test passes (or runs to completion — the
  threshold is N≥10 records from at least one source; if it lands
  at 8 or 12 that's still a pass, listings have variable
  cardinality day-to-day).
- Always: `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, `npm run check` in apps/desktop.


## What Session 38 is explicitly NOT

- **Not Phase 2 of ADR 0016.** Multi-extracted fields per match
  (headline + date + author per news card) is its own ADR and its
  own session. Phase 1 produces records with one extracted field +
  any number of literal fields per record. That's enough for the
  quantum-computing live case and most news/paper listings where
  the headline is the load-bearing field.
- **Not pagination.** A listing's first page produces N records.
  Page 2, 3, … is a separate ADR and session.
- **Not PDF table iteration.** Same shape as the CSV iterator
  (every row → one record) but the PDF fixture corpus needs work
  before the runtime path can be validated. Defer.
- **Not the `record_derived_from` provenance fix at scale.** If you
  end up writing edges as part of iteration's natural work,
  excellent — but don't expand scope to backfilling old records or
  building a UI for the DAG.
- **Not changing `dedup_key`'s shape across the codebase.** The fix
  here is local: the runtime computes and writes `dedup_key` for
  iterator-produced records. Pre-iteration records remain NULL and
  that's fine for now (the bug exists and is documented; iteration
  isn't the place to clean it up universally).
- **Not multi-source fan-out.** One source per recipe, one recipe
  per `(plan, source)` pair, one fetch per recipe — these are all
  unchanged.


## Hard rules (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All
  HTTP through `SecureHttpClient`.
- Bounds checking on every IPC string input. Bounds enforcement
  on iterator output (the 500-records cap).
- Tauri commands return `CommandError`, not internal error types.
  The new failure mode (iterator over-cap) maps to existing
  `FetchFailed` — no new variant needed.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test -p stockpile-api`. Never
  hand-edit. Ship the regenerated files in the patch tarball.
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- **No source-specific language in any prompt revision.** This is
  the Session 34 lesson, and it applies to v1.13 in full force.
  The new section must teach the LLM *when to use the iterator*
  via principles ("URL is a listing of items"), not via worked
  routing rules ("for nature.com, use `.c-card`").
- **DuckDB ALTER TABLE is sharp.** This session's storage work is
  a no-migration change (JSON column extension); if scope drift
  pushes you toward an ALTER, read migration 0005's comment block
  first. The Session 7/8 lesson about preferring Rust-type
  invariants to SQL constraints stands.


## Carry-forward (not for Session 38)

These were surfaced or confirmed in Session 37 and remain open.
Session 38 should not address them unless they become unavoidable
in the iterator work.

1. **`record_derived_from` table is empty.** The provenance DAG
   linking records to recipes isn't being written. Recoverable
   today via `source_id = "host#recipe:<uuid>"` substring match.
   Fix it when there's a session shaped for "rebuild provenance
   path"; iteration may surface it as more urgent but doesn't
   require fixing it.
2. **`dedup_key` is NULL on all events today.** Iteration's
   work in Session 38 fixes it for iterator-produced records.
   Non-iterator records (older) stay NULL until a backfill or
   migration session.
3. **Recipe-author selector discipline still under-fires on
   JS-rendered SPAs.** The Session 37 cold-start failed on
   ieeexplore.ieee.org (got script bootstrap), arxiv.org listing
   (got page chrome), ppubs.uspto.gov (got empty selector). These
   are pre-Session-37 weaknesses. v1.13 should *not* try to fix
   the SPA-decline path while it's also adding iterator language
   — two prompt changes in one revision is too many. Queue the
   SPA-decline refinement for v1.14.
4. **Anthropic provider stub.** Stays a stub (Session 3 carry).
5. **Apply-runtime strict deserialization.** Permissive (Session 3
   carry).
6. **PDF table extractor is unimplemented for the iterator case.**
   Phase 2 territory.
7. **`SecureHttpClient` doesn't surface response headers.**
   Long-standing carry.


## First thing to do in Session 38

Read this file, ADR 0016, ADR 0007 (closed-vocabulary section),
ADR 0014. Then `view` these in this order:

1. `crates/pipeline/src/recipes.rs` — the `FetchRecipe` and
   `ExtractionSpec` shapes you're extending.
2. `crates/pipeline/src/recipe_apply.rs` — the existing apply path
   you're branching. Especially the `css_select` arm and how it
   uses `scraper::ElementRef`.
3. `crates/pipeline/src/recipe_author.rs::build_validated_recipe`
   — the validator you're extending.
4. `config/prompts/recipe_author.md` v1.12 — the prompt you're
   revising. Read the existing "Closed extraction vocabulary" and
   "Endpoint discipline — instance vs listing" sections; v1.13's
   iterator section sits next to them.
5. The Session 37 debug record below — the empirical anchor for why
   this work matters.

Then build incrementally:

1. `recipes.rs` field addition + serde round-trip test. Green.
2. `recipe_author.rs` validator extension + new validator tests.
   Green.
3. `recipe_apply.rs` iteration path + unit tests. Green.
4. `fetch_executor.rs` per-recipe count propagation + test update.
   Green.
5. API DTO export + frontend recipe panel update + visual check.
6. Prompt v1.12 → v1.13 + worked example. Visual check.
7. Re-classify quantum-computing topic → accept → run fetch →
   verify N>1 events per listing source.
8. Live `#[ignore]` test that does step 7 in code. Run with
   `cargo test -p stockpile-pipeline --ignored
   live_iterator_against_real_listing_produces_n_records`.
9. ADR 0016 status `Proposed` → `Accepted` with date.

Each numbered step has a green build behind it before moving on.
Do not write the entire session and run cargo check at the end.


## Session 37 debug record (the empirical anchor)

Pasted from the Session 37 path-C investigation.

Plan: `019e024e-3e2e-7f71-9311-59883a97f229`
Fetch run: `019e024e-6db0-7e80-a7a8-a373e711bf9c`

```sql
SELECT id, source_id, source_url, content FROM events
WHERE source_id LIKE 'www.nature.com%';
```

Returned one row. Content (.mode json):
```json
{
  "event_type": "milestone_announced",
  "headline": "Nanoscale 'conveyor belt' teleports quantum state of electron",
  "actors": [],
  "direction": null
}
```

```sql
SELECT id, source_id, source_url, content FROM events
WHERE source_id LIKE 'qt.eu%';
```

Returned one row. Content:
```json
{
  "event_type": "milestone_announced",
  "headline": "Strategic Advisory Board …",
  "actors": [],
  "direction": null
}
```

The recipes that produced these (from the fetch report):

Nature: `extraction = { mode: css_select, selector: "h3.c-card__title a" }`
qt.eu:  `extraction = { mode: css_select, selector: ".teaser-item-content .h4" }`

Both selectors are leaf-correct. Both pages have N>1 cards
matching. The runtime returned the first match per the closed-
vocabulary contract. The honest reading: the extraction is
honest-but-narrow. The architecture lacks a way to express
"and the other N-1 cards." That's what Session 38 fixes.


## Continuity note

The continuity note from Session 37 still applies. The operator is
rigorous about security, prefers honesty about uncertainty over
false confidence, and reacts well to direct disagreement when
warranted. Stick to the plan in ADR 0016. If implementation
discoveries push you to deviate (the iterator's scope semantics
turn out trickier in `scraper` than the ADR assumed, the dedup
story needs a different shape, the cap value needs to be different),
say so and explain why. The "do not deviate" discipline holds
unless you have evidence to deviate.

One specific carry-over from Session 37's prep: **prompt revisions
are principle-only.** The Session 34 lesson — never bake source-
specific routing rules into the prompt — applies in full to v1.13.
The iterator section teaches the LLM via principles ("URL is a
listing of items"), not via examples that name specific sites.
The worked example in v1.13 should use a generic illustration, not
the concrete Nature URL that motivated the work.

Codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.
