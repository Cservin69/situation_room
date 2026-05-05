# Stockpile — Session 29 handoff

**Status at the end of Session 28:** Track B (decline path +
schema-aware authoring + `{{PREVIOUS_FAILURE_REASON}}` /
`{{OPERATOR_GUIDANCE}}` placeholder wiring + recipe-author prompt
v1.9 + ADR 0007 amendment 4) is **complete as a code drop** and ready
for `cargo check` + `cargo test --workspace` + `npm run check`. Track
C (pdf_table extraction) from the Session 25 spec remains the queue
for Session 29.

The patch is at `~/Downloads/stockpile_session28_track_b.tar.gz`.
Apply with the standard incantation from the repo root:

```
tar -xzf ~/Downloads/stockpile_session28_track_b.tar.gz --strip-components=1 -C .
```

Then verify with `cargo check`, `cargo test --workspace`, and
`npm run check` from `apps/desktop`. Expected baseline pre-Track-B
was 441 tests green (Session 27 close); this session adds **~30 new
tests** (3 in `core::schema::content`, 17 in `recipe_author::tests`,
2 in `fetch_executor::tests`, 1 extension to and 1 new test in
`api::types_export::tests`, plus the implicit derive coverage). The
expected new baseline is **~471 tests green**.

If anything fails, the earlier Session 27 lesson applies: read what
failed before doing anything else.

---

## STEP 0 — APPLY AND VERIFY (5 minutes)

```
cd /Users/aben/RustroverProjects/stockpile
tar -xzf ~/Downloads/stockpile_session28_track_b.tar.gz --strip-components=1 -C .
cargo check --workspace
cargo test --workspace
cargo test -p situation_room-api  # regenerates ts-rs files; should be no-op diff
cd apps/desktop && npm run check
```

**Expected:** zero compile errors, ~471 tests green, zero TS errors.

If `cargo test -p situation_room-api` produces a non-empty diff to
`apps/desktop/src/lib/api/types/RecipeOutcomeDto.ts`, the regenerated
form is canonical and overrides the hand-written version this patch
ships. Most likely cause if it diverges: ts-rs's exact whitespace /
attribute-order has shifted between the version pinned in
`Cargo.toml` and what I matched against. Take the regenerated file.

---

## STEP 1 — HUMAN-LOOP TEST OF THE DECLINE PATH (15 minutes)

This is the actual evidence-gathering step that earns the next
gate-condition tick on ADR 0012 (which is *separate* from this
session's work but benefits from the Track B surface). Run a fetch
against a source where the LLM is likely to decline.

1. Pick a plan that includes a JS-rendered SPA in its
   `document_sources` (e.g. a topic about "real-time market data"
   that the classifier nominates a SPA-shaped news source for). If
   none come naturally, manually add one to a fresh plan: edit
   `config/sources.toml` to include a SPA you know is JS-rendered
   (a Bloomberg landing page, a SPA-shaped finance dashboard).
2. Classify, accept, run fetch.
3. Inspect the fetch report panel:
   - Does the SPA source surface as a `Declined` outcome (left
     border in `--fg-tertiary`, `decl·` marker in the recipe-id
     column, the LLM's reason in the detail row)?
   - Does the run summary show `recipes_attempted` and
     `recipes_succeeded` excluding the declined source?
4. **Document the outcome** in a new file under
   `docs/failure_cases/class_b/` (or — more honestly — a new
   `docs/failure_cases/class_c/` directory if the source is JS-
   rendered, since that's structurally Class C territory). The
   ADR 0012 gate counts only Class B failures with documented
   predicate strings; declines documented as Class C are
   informational, not gate-counting.
5. If the LLM authors a recipe instead of declining, the prompt's
   "decline path" section may need refinement — note which sources
   the LLM should-have-but-did-not decline against, in case Track B
   v1.10 is needed.

Document any new patterns you observe. The decline channel is a
hypothesis ("this LLM, with this prompt, will decline against
genuinely-undoable sources"); the test is "do real runs against
real sources?" The answer is empirical.

---

## STEP 2 — TRACK C (pdf_table extraction) (1–1½ sessions)

Session 25 chose **approach (a)**: pure-Rust layout-heuristic table
detection rather than an external Tabula/JBIG2 dependency. Implement
in `crates/pipeline/src/recipe_apply.rs`'s `PdfTable` arm
(currently returns `RecipeOutcome::Skipped { ... not implemented ... }`).

Approach in brief:

1. Use `pdf-extract` or `lopdf` to walk page text + positions.
2. Cluster glyphs by y-coordinate to find rows.
3. Cluster glyphs within rows by x-coordinate to find columns.
4. Address by (row, col) per the existing `PdfTable {
   page, table_index, row, col }` extraction spec.

**Test fixture:** USGS Mineral Commodity Summaries (any chapter
PDF, public-domain). Add to
`crates/pipeline/tests/fixtures/pdf/`. Pick one with a clearly
tabular layout (not multi-column prose) and use it as the
end-to-end test's input.

Writes ADR 0007 amendment 5.

When Track C lands, the `Skipped` outcome for `pdf_table` recipes
becomes `Succeeded` (or `Failed @ Apply`) instead — the LLM has
been authoring `pdf_table` recipes the whole time per the prompt's
"Use `pdf_table` for authoritative annual reports" guidance, but
the runtime has been declining to run them. Track C closes that
gap.

---

## STEP 3 — Track B follow-ups (½ session each, in priority order)

### B.1 — Live test of the decline path through xAI

The patch ships unit tests that exercise `DecliningProvider` (a
test double that always returns a `decline_reason`). It does **not**
ship an `#[ignore]` live test that exercises a real xAI call against
a known-undoable source and asserts the LLM declines. Such a test is
worth adding once the prompt's decline-path section has shaken out;
mirror `live_author_recipe_against_xai_produces_valid_recipe`'s
shape, with a fixture URL pointing at a known JS SPA.

### B.2 — Schema regeneration check in CI

`target_record_schemas` is computed at every authoring call. If a
future session adds a field to `ObservationContent` /
`EventContent` / `RelationContent` without bumping the prompt or
the LLM's understanding of "what fields exist," the LLM will
author against a schema it doesn't know about. The compile is
fine; the recipes silently miss the new field. A small CI test
that snapshots the JSON Schema for each content type and fails on
diff would catch this.

### B.3 — Recipe-feedback channel for declines

When the LLM declines, ADR 0013's recipe-feedback channel is the
natural follow-up: the operator types "I don't agree this is
undoable; the source's API endpoint at `/api/v2/...` returns
JSON" and the next authoring run sees that note. The plumbing
already exists for the `RECIPE_FEEDBACK` channel; declines just
need a UI hook to launch the feedback dialog from a declined
outcome (similar to the re-author button on apply failures).
Small. Worth a session when the empirical decline rate is
understood.

---

## What landed this session — file inventory

### Core (`crates/core`)

- `src/vocab.rs` — `JsonSchema` derive on `Topic`, `CountryCode`,
  `EntityId`, `EventType`, `Unit`, `Currency`. `Stance` and
  `Confidence` deliberately left as-is (not in recipe authoring
  surface).
- `src/schema/geometry.rs` — `JsonSchema` derive on `Position`,
  `Geometry`, `PointGeom`, `LineStringGeom`, `PolygonGeom`,
  `MultiPolygonGeom`.
- `src/schema/content.rs` — `JsonSchema` derive on
  `ObservationContent`, `EventContent`, `RelationContent`,
  `ObservationPeriod`, `EventDirection`. Three new tests pin
  schema generation.

### Secure (`crates/secure`)

- `src/bounds.rs` — `Bounds::DECLINE_REASON = 2_000`.

### Pipeline — recipe author (`crates/pipeline/src/recipe_author.rs`)

- `AuthoringError::Declined { reason }` variant.
- `RecipeAuthoringOutput.decline_reason: String` (empty-string-
  as-absent).
- `build_validated_recipe` step 0: short-circuit to `Declined`
  with `Bounds::DECLINE_REASON` length check, before any URL or
  binding validation.
- `target_record_schemas() -> Result<String, serde_json::Error>` —
  returns the schemars JSON Schemas for the three authorable
  record-content types, wrapped as a single pretty-printed JSON
  object keyed by snake_case record-type name.
- `render_previous_failure_reason()` — plain-prose framing,
  no fence (failure messages are executor-side, no injection
  vector).
- `render_operator_guidance()` — fenced with per-call UUID nonce,
  same byte-walk discipline as `RECIPE_FEEDBACK`.
- `sanitize_for_fence` refactored into parametric
  `sanitize_for_fence_named(s, fence_id, tag)`; the original
  `sanitize_for_fence` becomes a thin wrapper that passes
  `"recipe_feedback"`. The new `operator_guidance` fence reuses
  the same algorithm.
- `build_prompt_with_fence_id` substitutes `{{TARGET_RECORD_SCHEMA}}`,
  `{{PREVIOUS_FAILURE_REASON}}`, and `{{OPERATOR_GUIDANCE}}` in
  addition to the existing placeholders. Docstring on `build_prompt`
  enumerates the new substitutions.
- 17 new tests covering decline-path short-circuit, `Bounds::
  DECLINE_REASON` length check, whitespace handling, schema-helper
  output, render-helper happy paths and breakout-attempt
  sanitization, and end-to-end placeholder substitution.

### Pipeline — fetch executor (`crates/pipeline/src/fetch_executor.rs`)

- `RecipeOutcome::Declined { source_id, reason }` variant.
- `load_or_author_recipes` returns `Result<(Vec<FetchRecipe>,
  Vec<RecipeOutcome>), FetchExecutorError>`. Per-source
  `AuthoringError::Declined` is lifted into the second Vec; other
  authoring errors keep the prior log-and-continue behaviour.
- `run_fetch_for_plan` consumes the tuple, prepends declines to
  `outcomes`, and the per-recipe match arm handles the (non-
  reaching-but-defensive) `Declined` case explicitly.
- `DecliningProvider` test double + 2 new tests:
  `declined_source_surfaces_as_declined_outcome` and
  `second_run_after_decline_re_attempts_authoring`.

### API (`crates/api/src/types_export.rs`)

- `RecipeOutcomeDto::Declined { source_id, reason }` variant
  (no `recipe_id` — distinguishing wire-shape feature).
- `From<RecipeOutcome>` arm for `Declined`.
- Extension to
  `recipe_outcome_dto_serializes_with_kind_tag_per_variant`
  pinning the `kind: "declined"` shape and the absence of
  `recipe_id` on the wire.
- New `recipe_outcome_dto_lifts_declined_from_typed` test.

### Frontend

- `apps/desktop/src/lib/api/types/RecipeOutcomeDto.ts` —
  regenerated to include the `declined` branch (no `recipe_id`).
- `apps/desktop/src/lib/outcomes.ts` — extended `OutcomeTone` to
  include `'declined'`; `outcomeTone` / `outcomeLabel` /
  `outcomeDetail` handle the new variant; new `outcomeKey()`
  helper for keyed-each rendering (declines have no
  `recipe_id`); `outcomeForRecipe()` now narrows past declines
  before searching by `recipe_id`. `formatRetryAfter` accepts
  both `number` and `bigint` (defensive: ts-rs's `u64` mapping
  has shifted between releases).
- `apps/desktop/src/components/FetchReport.svelte` — keyed-each
  switched from `(o.recipe_id)` to `(outcomeKey(o))`; the row
  renders a `decl·` italic marker in the recipe-id column when
  the variant is `declined`; `[data-tone="declined"]` CSS uses
  `--fg-tertiary` border + `--bg-panel-alt` background, distinct
  from `failed` (red) / `rate_limited` (amber) / `skipped` (no
  background) tones.

### Prompt (`config/prompts/recipe_author.md`)

- Bumped to v1.9 with full changelog entry.
- Four new prose sections inserted before "The plan you are
  authoring for":
  1. "When no recipe is honestly possible — the decline path"
  2. "What the records you produce look like" (introduces
     `{{TARGET_RECORD_SCHEMA}}`)
  3. "Type honesty" (null-vs-numeric, numeric-string-vs-numeric)
  4. "Zero records is a valid outcome"
  5. "Defensive variants — what to do when your first attempt
     may not match" (the BBC CDATA case, optional-field-presence
     case)
- `{{PREVIOUS_FAILURE_REASON}}` and `{{OPERATOR_GUIDANCE}}`
  placeholders added inline after `{{RECIPE_FEEDBACK}}`. Empty
  substitution when context is `None`, so fresh-authoring runs
  look identical to the v1.8 path.

### Documentation

- `docs/adr/0007-research-function.md` — Amendment 4 appended
  with full rationale, code references, what-this-amendment-
  does-NOT-do scope guard, and the architectural reasoning for
  flat `decline_reason` vs discriminated union.

---

## Known gaps and follow-ups

- **The hand-written `RecipeOutcomeDto.ts`** matches what ts-rs
  will emit on the next `cargo test --package situation_room-api`
  run as best I could match. If ts-rs's exact format has shifted
  between the version pinned in `Cargo.toml` and what I matched
  against, the regenerated form wins. The build doesn't depend on
  the TS file matching precisely (the frontend imports are
  structural); a regeneration with a slightly different format
  is fine.
- **`formatRetryAfter` now accepts `number | bigint`.** This is a
  defensive widening: prior to this patch the function was typed
  as `(secs: number) => string`, but ts-rs's emitted DTO has
  `retry_after_seconds: bigint | null` (the `u64` mapping). The
  change is purely additive — any existing caller passing a
  `number` still works. If the codebase consolidates on `bigint`
  for `u64` fields the `number` branch can be removed in a
  follow-up; for now both work.
- **The `decl·` marker in the recipe-id column** is visual
  scaffolding and not load-bearing for any other code. Future
  design polish may want to hide the column slot entirely for
  declined rows; the current treatment keeps the column grid
  uniform across all variants, which I judged worth the small
  visual cost.
- **No integration test for the IPC boundary.** The Track A
  pattern of integration tests in `crates/api` against a
  `RecordingProvider`-style LLM scaffold doesn't yet cover the
  decline path end-to-end. Worth adding when the
  test-only LLM provider is extended to support per-call canned
  outputs (today it's one-canned-output-per-instance, which
  doesn't compose well for "first call succeeds, second call
  declines" flows).
- **Live xAI test of the decline path is not in the suite.** See
  Step 3 "Track B follow-ups" above.
- **Carried forward from Session 27:**
  - Anthropic provider and others are stubs.
  - Apply-runtime strict deserialization is permissive.
  - PdfTable extractor is unimplemented (Track C, this is the
    next session's queue).
  - Authoring latency is 30–60s (xAI gateway, not us).
  - Crate-level `#![allow(...)]` lint suppressions still need a
    sweep across crates that aren't `api`.

---

## What this session did not change

- The 6 record types stay 6.
- The two-level LLM architecture (classifier → recipe author) is
  unchanged.
- The closed extraction-mode enum is unchanged. Adding `pdf_table`
  to the *runtime* (Track C) doesn't add a sixth mode — it wires
  an existing fifth-of-five to an actual extractor.
- ADR 0009's security posture is unchanged. The decline reason and
  operator-guidance channels carry text the operator typed
  (operator-guidance) or the LLM emitted (decline-reason / failure-
  reason); both go through the existing fence + nonce + length-
  bound discipline. No new HTTP path, no new secret surface.
- The xAI provider integration is unchanged. The `decline_reason`
  field is part of the `RecipeAuthoringOutput` schema the existing
  `provider.complete()` call sends; no provider-side change is
  required.
- ADR 0012's gate (10 documented Class B failures) is unchanged.
  This amendment does not move it. The decline channel is a
  separate concern: it gives the LLM an honest exit when the
  closed vocabulary genuinely cannot address the source. Class B
  remains "the LLM authored a recipe and it failed at apply
  time" — declines never become Class B because no recipe is
  authored. Class C ("structurally impossible source," e.g. JS
  SPA) is what `decline_reason` typically catches; once enough
  declines accumulate, a `docs/failure_cases/class_c/` directory
  becomes worth populating.

---

## One thing to look at before any new code

The schemars-derived JSON Schema in `{{TARGET_RECORD_SCHEMA}}` is
**recomputed at every authoring call**. This is intentional: the
helper is on a slow path (each authoring call is a 30–60s LLM
round-trip) and memoizing would introduce visible state for tiny
gain. **Do not add a `OnceLock` or `lazy_static` here without
measuring first.** If the helper ever shows up in a profile, the
fix is the kind of small refactor that wants its own session, not
a drive-by addition.

End of handoff.
