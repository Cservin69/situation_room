# STOCKPILE — Session 39 handoff

You are starting Session 39. Session 38 shipped ADR 0016 Phase 1 —
the extraction iterator — end-to-end against the cleanest case
(`css_select × css_select`). Listing-shaped sources can now produce
N records per fetch instead of 1.

Read this whole document before writing any code. Read ADR 0016
(now `Accepted`, dated 2026-05-07). Re-read ADR 0007's "closed
extraction vocabulary" section for the discipline iteration
preserves: the closed enum stays at five modes, plus one orthogonal
"iterate?" axis.

## What works today (end of Session 38)

- **Storage (migration v15).** `recipes.iterator JSON NULL`. NULL
  on disk → `iterator: None` in Rust. The codebase's fifth
  precedent for an additive nullable recipe column (after 0008
  static_payload, 0010 authored_from, 0011 prior_recipe_id, 0012
  reauthor_reason). Zero ALTER trap, no re-authoring required for
  any existing recipe.
- **Typed pipeline.** `FetchRecipe.iterator: Option<ExtractionSpec>`
  and `ProductionBinding.dedup_key_field: Option<String>`, both
  with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  Legacy recipe JSON (no iterator field, no dedup_key_field on
  bindings) deserializes via serde defaults. Pinned by
  `legacy_recipe_without_iterator_or_dedup_key_field_deserializes`
  in `recipes.rs::tests`.
- **Recipe-author validator.** Four new invariants enforced at
  `build_validated_recipe`:
  - mode congruence (iterator and inner extraction must share a mode)
  - csv_cell-iterator-column-empty (forward-compatible guard for
    Phase 2 CSV iteration)
  - dedup_key_field required on every binding when iterator is set
  - dedup_key_field must reference an existing field_mappings.path
  Plus a defensive collapse of empty/whitespace dedup_key_field to
  None at convert_binding time.
- **Runtime path (Phase 1: `css_select × css_select`).** The
  iterator's selector picks N DOM nodes; `scraper::ElementRef::select`
  scopes the inner extraction to each node's sub-tree. One record
  per match per binding. Records carry per-record `dedup_key`
  computed as `{recipe.id}:{extracted_field_value}`, bounded at 200
  chars, sourced from the binding's `dedup_key_field`.
  - Hard cap: `MAX_RECORDS_PER_RECIPE = 500`. Overflow surfaces as
    `ApplyError::Extraction { mode: "css_select", reason: "iterator
    produced N matches; cap is 500" }`.
  - Other mode pairings (json_path × json_path, etc.) surface as
    `ApplyError::NotImplemented { mode: "iterator", reason: ... }`.
    Phase 2 wires those.
  - Cross-mode pairings (validator should reject them) surface as
    `NotImplemented` defensively if a hand-edit slips one through.
- **Fetch executor.** Zero functional change confirmed — the
  existing `records_produced: records.len() as u32` flow already
  propagates the iteration count correctly. Cap-overflow surfaces
  as `RecipeOutcome::Failed { stage: Apply, ... }` automatically.
  One new end-to-end test
  (`run_fetch_with_iterator_recipe_produces_n_records`) and one new
  live test (`live_iterator_against_real_listing_produces_n_records`).
- **DTO surface.** `RecipeDto.iterator: Option<serde_json::Value>`
  with `#[ts(type = "unknown")]`. The frontend's
  `prettyJson(recipe.extraction)` block is unchanged; a sibling
  `prettyJson(recipe.iterator)` block renders next to it when
  iteration is in play.
- **Frontend `RecipesPanel.svelte`.** Three additions:
  1. `ITERATES` chip in the recipe head, alongside BAKED and
     STUB-AUTHORED. Hue: `--signal-info` (informational, not
     degraded — the recipe is healthy, just non-default cardinality).
  2. Iterator details block between extraction and produces blocks,
     open by default.
  3. Doc-comment preamble updated to enumerate the chip semantics.
- **Recipe-author prompt v1.13.** New top-level "Iterating over
  listings" section after the revised "Coverage discipline." Two
  worked examples (iterator-bearing news index; contrasting single-
  instance filing) — principle-only, no named URLs (Session 34/37
  lesson). Three new "What NOT to produce" bullets.
- **ADR 0016 status: Accepted** (2026-05-07).

## Test count (added by Session 38)

- `crates/storage/src/recipes.rs` — 3 new tests:
  `recipe_round_trips_with_no_iterator`,
  `recipe_round_trips_with_iterator`,
  `recipes_for_plan_carries_iterator_through`.
- `crates/pipeline/src/recipes.rs` — 3 new tests:
  `iterator_is_optional_and_omits_when_absent`,
  `iterator_field_round_trips_through_serde`,
  `legacy_recipe_without_iterator_or_dedup_key_field_deserializes`.
- `crates/pipeline/src/recipes_store.rs` — 2 new tests:
  `scalar_recipe_round_trips_with_no_iterator`,
  `iterator_recipe_round_trips_through_storage`.
- `crates/pipeline/src/recipe_apply.rs` — 6 new tests:
  `css_select_iterator_produces_n_records`,
  `iterator_caps_records_at_max`,
  `iterator_with_zero_matches_reports_iterator_selector`,
  `iterator_inner_selector_miss_reports_inner_layer`,
  `iterator_with_cross_mode_pair_is_not_implemented`,
  `scalar_recipe_records_still_carry_no_dedup_key`.
- `crates/pipeline/src/recipe_author.rs` — 6 new tests:
  `iterator_with_mode_congruence_and_dedup_key_field_validates`,
  `iterator_validates_mode_congruence`,
  `iterator_requires_dedup_key_field`,
  `dedup_key_field_must_reference_existing_path`,
  `empty_dedup_key_field_collapses_and_iterator_rejects_it`,
  `iterator_csv_cell_with_non_empty_column_is_rejected`,
  `scalar_recipe_without_dedup_key_field_still_validates`.
- `crates/pipeline/src/fetch_executor.rs` — 1 new offline test:
  `run_fetch_with_iterator_recipe_produces_n_records`.
  1 new live (`#[ignore]`) test:
  `live_iterator_against_real_listing_produces_n_records`.
- `crates/api/src/types_export.rs` — 2 new tests:
  `recipe_dto_iterator_is_none_when_absent`,
  `recipe_dto_iterator_round_trips_from_stored`.

Net: +23 unit tests, +1 ignored live test.

## Build verification (operator's first move)

Session 38 was run in an environment without a Rust toolchain — no
code in the patch was compiler-verified end-to-end. Type signatures
were cross-referenced against every dependent file before each
patch, and a Rust-aware brace-balance check passed across every
modified file, but the first thing Session 39 should do is:

```
cargo build --workspace
cargo test --workspace --lib
cargo test -p situation_room_api    # regenerates ts-rs files
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The hand-written `apps/desktop/src/lib/api/types/RecipeDto.ts`
matches the format ts-rs would emit but cargo test in the api crate
will overwrite it; treat ts-rs's output as canonical and let it
regenerate. Spot-check the diff: it should be cosmetic at most.

If the live test runs (`cargo test -p situation_room_pipeline
--ignored live_iterator_against_real_listing_produces_n_records`),
override the env vars to target a real listing — the default targets
a stable W3C HTML page just to keep the CI-like default happy. The
ADR-0016 empirical claim ("≥10 events per source on a real listing")
is what the operator confirms by setting `FETCH_LIVE_ITERATOR_URL`
to a Nature subjects URL, qt.eu newsroom, arXiv recent, etc.

## Known imperfections (carry-forward)

Carried from Session 37; iteration didn't address them.

1. **`record_derived_from` table is empty.** Provenance DAG isn't
   being written. Recoverable today via
   `source_id = "host#recipe:<uuid>"` substring match. Iteration
   surfaced this more sharply (N records share one recipe), but
   the fix is still its own session.
2. **`dedup_key` is NULL on pre-iteration records.** ADR 0016
   §Carry-forward dependencies notes this. Iterator records now
   carry `dedup_key`; scalar records (and historical records) stay
   NULL. Backfill is its own session.
3. **Apply-runtime strict deserialization is permissive.** Session
   3 carry. Session 38 didn't tighten it.
4. **Anthropic provider is a stub.** Session 3 carry.
5. **PDF iterator unimplemented.** Phase 2 territory.
6. **JSON path / CSS / regex / CSV iterators surface as
   `NotImplemented` at apply time.** The validator accepts them
   (mode-congruent pairings are valid recipe shapes); the runtime
   only wires `css_select × css_select` in Phase 1. Phase 2 is the
   natural follow-up.
7. **`SecureHttpClient` doesn't surface response headers.**
   Long-standing carry.

## Suggested Session 39 priorities

In rough order of leverage:

### P1 — Run the build, fix what the compiler flags, ship a tarball

Session 38 was uncompiled. `cargo build` is the first move; expect
some `clippy` lint fixes (unused-mut, redundant-clone, etc.) and
possibly a missing import or two. If anything's mismatched, the
balance check passing means the issue is type-level, not
brace-level — the Rust compiler will name it precisely.

### P2 — Empirical re-classification + fetch against the quantum-computing topic

The ADR 0016 §Validation claim:

> Re-classify "quantum computing hardware roadmaps" against the
> post-Session-38 codebase, accept the plan, run a fetch. The
> Nature recipe and the qt.eu recipe should each produce N>1
> Event records.

This is the empirical falsifiability test for ADR 0016. Run it.
If both sources produce N>1, the architecture is confirmed and
Session 38 is done. If either produces 1, the implementation is
wrong somewhere — likely candidates are the prompt (the LLM
didn't pick up iterator), the validator (the recipe was rejected
silently), or the runtime (scope semantics off).

### P3 — `record_derived_from` rebuild

The provenance DAG defect predates iteration but iteration makes
it more visible (one recipe, many records — the recipe→records
edge is now plural). Population code is missing somewhere on the
insert path; surface and fix.

### P4 — Phase 2 iterators (one mode at a time)

`json_path × json_path` is the next-cleanest case (RSS feeds with
JSON wrappers, GraphQL listings, paginated API responses). After
that: `regex_capture × regex_capture` (newline-as-row text feeds).
`csv_cell` and `pdf_table` iterators are the trickiest; defer
those until the simpler modes have run in production for a few
cycles.

### P5 — Prompt v1.14: SPA-decline refinement

Session 37 surfaced JS-rendered SPA failures
(ieeexplore.ieee.org, arxiv.org listing, ppubs.uspto.gov) that
v1.13 deliberately didn't address — two prompt changes per
revision is too many. v1.14 should refine the decline path so
SPA cases produce a clean `decline_reason` rather than a
plausible-but-wrong recipe.

## What Session 39 is explicitly NOT

- **Not Phase 2 of ADR 0016** unless P2 confirms Phase 1 works
  end-to-end against real sources. Phase 2 (multi-extracted
  fields per match — headline + date + author per news card)
  has its own ADR pending.
- **Not pagination.** Listing's first page is what Phase 1
  addresses; page 2, 3, … is a separate ADR.
- **Not editing the iterator's worked examples in the prompt to
  name specific sources.** The Session 34/37 lesson is in full
  force: principle-only, no named URLs.
- **Not changing the cap.** 500 is generous for real listings;
  raise it only if a real listing's first page exceeds it (which
  would more likely be a sign the iterator selector is too broad).

## Hard rules (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All
  HTTP through `SecureHttpClient`.
- Bounds checking on every IPC string input; bounds enforcement
  on iterator output (the 500-records cap).
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via cargo test. Never hand-edit (Session 38's
  `RecipeDto.ts` was hand-written to match ts-rs's emit format
  for shipping; let cargo test overwrite it on first build).
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- **No source-specific language in any prompt revision.** v1.13's
  iterator section is principle-only; v1.14 must hold the line.
- **DuckDB ALTER TABLE is sharp.** Session 38's migration v15 used
  the safe shape (nullable, no default, no index added). The
  codebase precedent for this exact pattern now spans five
  migrations (0008, 0010, 0011, 0012, 0015) — when adding the
  next optional recipe field, mirror them.

## First thing to do in Session 39

Read this file. Then:

1. `cargo build --workspace` — fix what compiles wrongly.
2. `cargo test --workspace --lib` — fix what tests fail.
3. `cargo test -p situation_room_api` — let ts-rs regenerate.
4. `cargo clippy --workspace --all-targets -- -D warnings` —
   address lint.
5. Run the desktop app (`scripts/run_desktop.sh`); classify the
   quantum-computing topic; accept; run fetch; verify N>1 events
   from at least one source. This is the ADR 0016 §Validation
   test in muscle-memory form.

Each step has a green build behind it before moving on.

## Continuity note

The continuity note from Session 38 still applies. The operator is
rigorous about security, prefers honesty about uncertainty over
false confidence, and reacts well to direct disagreement when
warranted.

Session 38 surfaced one storage-shape decision worth recording:
ADR 0016 said "no migration needed" but the actual codebase has
separate JSON columns per typed field (`extraction_json`,
`produces_json`, etc.). Adding `iterator` as a third such column
followed the four-precedent pattern (0008/0010/0011/0012) — strictly
less risky than the envelope-shape alternative. The operator
confirmed Path B (migration) before any code was written. The
pattern: when an ADR's "no migration" claim is load-bearing for
the simplicity argument, but the actual storage shape makes it
ambiguous, surface and ask. Don't pick silently.

End of handoff.
