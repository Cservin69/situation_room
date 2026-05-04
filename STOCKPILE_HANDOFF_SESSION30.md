# Stockpile — Session 31 handoff

**Status at the end of Session 30:** Two of the deferred items from
the Session 28/29 rolling backlog land as a single small patch:

1. **Flag-from-decline UI hook (ADR 0013)** — declined outcomes in
   `FetchReport.svelte` now expose the same flag/FLAGGED affordance
   the recipes panel uses, closing the only failure shape where the
   operator could *see* a problem but not *attach feedback* to it.
2. **Two `#[ignore]`d live tests** — one for the `pdf_table` runtime
   end-to-end against a real USGS MCS PDF (Track C.1 from Session 29),
   and one for the recipe-author decline path against a real xAI call
   on a JS-SPA-shaped excerpt (Track B.1 from Session 28).

The patch is at `~/Downloads/stockpile_session30.tar.gz`. Apply with:

```
tar -xzf ~/Downloads/stockpile_session30.tar.gz --strip-components=1 -C .
```

Then verify with `cargo check`, `cargo test --workspace`, and `npm
run check` from `apps/desktop`.

Pre-Session-30 baseline was ~485 tests (Session 29 close). This
session adds **2 new tests** (both `#[ignore]`d, so they don't
appear in the green count). New baseline: still ~485 tests green
in CI; manual count of `#[ignore]`d tests goes from 4 to 6.

If anything fails, the lesson from Sessions 27/28 applies: read
what failed before doing anything else.

---

## STEP 0 — APPLY AND VERIFY (5 minutes)

```
cd /Users/aben/RustroverProjects/stockpile
tar -xzf ~/Downloads/stockpile_session30.tar.gz --strip-components=1 -C .
cargo check --workspace
cargo test --workspace
cd apps/desktop && npm run check
```

**Expected:** zero compile errors, ~485 tests green, zero TS errors.

Specific things to verify:

1. `FetchReport.svelte` imports cleanly — the new imports are
   `flagRecipe` from `$stores/plans.svelte` and `RecipeFlagDialog`
   from `$components/dialogs/RecipeFlagDialog.svelte`. Both already
   exist; this file is simply the second consumer.

2. The `{@const feedback = ...}` binding parses — Svelte 5 allows
   `{@const}` as an immediate child of `{#if}`. The binding is
   placed as the first child of the declined branch, before the
   comment block, so even older / stricter Svelte parsers should
   accept it.

3. `cargo test -p situation_room-pipeline -- --include-ignored`
   skips the live tests by default; to actually run them you need
   `XAI_API_KEY` in the environment / `.env` file and explicit
   opt-in via the test name. The new tests are
   `live_fetch_against_real_pdf_produces_observation_and_closes_run`
   and `live_author_against_jsspa_excerpt_produces_decline`.

---

## STEP 1 — HUMAN-LOOP TEST OF FLAG-FROM-DECLINE (10 minutes)

This is the visible work the patch enables. The flow exists end-
to-end in code; this step confirms the UX feels as designed.

1. Edit `config/sources.toml` to add a JS-SPA-shaped source the
   classifier might nominate — a Bloomberg landing page, a SPA-shaped
   finance dashboard, or just append a fake `endpoint_hint` to an
   existing source pointing at a SPA URL. The decline-path is
   probabilistic; this nudges it.
2. Classify a topic that's likely to nominate the SPA source
   ("real-time market data", "live commodity prices", etc.).
3. Accept the plan. Run fetch.
4. The fetch report panel should show the SPA source as a
   `Declined` outcome (left border in `--fg-tertiary`, `decl·`
   marker in the recipe-id column, the LLM's reason in the detail
   row). **Below the detail row, you should now see a small
   `flag` button right-aligned.** This is the new affordance.
5. Click `flag`. The same `RecipeFlagDialog` that opens from the
   recipes panel should open, with an empty initial note (since
   no feedback exists yet for this source).
6. Type a correction ("the source has an undocumented JSON API at
   /api/v2/quotes that returns extractable data") and submit.
7. The dialog closes. The button on the row should now read
   `FLAGGED` (in `--signal-info` chrome). Hover over `FLAGGED`:
   the tooltip should show your note's full text.
8. Click `FLAGGED` to reopen the dialog — the textarea should
   pre-fill with your saved note. You can edit and resubmit, or
   submit empty to clear.
9. Run fetch again. The recipe-author runs for the previously-
   declined source; this time the `{{RECIPE_FEEDBACK}}` placeholder
   in the prompt is populated with your note. The model may now
   author a recipe (your correction was useful), decline again
   with a different reason (your correction was wrong about the
   source's shape), or decline citing your note (the LLM
   acknowledges the correction but maintains its judgment). All
   three are valid outcomes — the channel works either way.

The recipe-feedback flow is now symmetric across both panels:
RecipesPanel handles flagging recipes that exist (apply failures,
wrong-shape extractions); FetchReport handles flagging declines
(no recipe ever existed).

---

## STEP 2 — RUN THE LIVE TESTS (15 minutes, optional)

The two new `#[ignore]`d tests are documentation + manual-trigger
paths. They don't run in CI; they exist to be invoked when the
operator wants empirical confirmation of the wired path.

### live_fetch_against_real_pdf_produces_observation_and_closes_run

```
XAI_API_KEY=sk-... cargo test -p situation_room-pipeline -- \
  --ignored live_fetch_against_real_pdf_produces_observation_and_closes_run \
  --nocapture
```

(`XAI_API_KEY` is technically not required here since the test uses
`UnreachableProvider` — the recipe is pre-authored. But the
`dotenvy::dotenv()` call at the top of the test loads the `.env`
file, which is the convention all live tests share.)

The test points at
`https://pubs.usgs.gov/periodicals/mcs2024/mcs2024-lithium.pdf` by
default with coordinate `page=2, table_index=0, row=2, col=1`. If
that addresses an empty cell on the actual PDF, override the
coordinates via `FETCH_LIVE_PDF_PAGE` / `FETCH_LIVE_PDF_TABLE_INDEX`
/ `FETCH_LIVE_PDF_ROW` / `FETCH_LIVE_PDF_COL`, or point at a
different PDF via `FETCH_LIVE_PDF_URL`.

The test asserts only on wiring (recipe attempted, outcome is not
`Skipped` / `Declined`, audit row closed). A `Failed @ Apply` is a
green test — it means the runtime took the right branch and
surfaced a typed failure; the address was just wrong for this PDF.
File real-PDF apply failures under `docs/failure_cases/` per the
Session 29 handoff §"Class C patterns."

### live_author_against_jsspa_excerpt_produces_decline

```
XAI_API_KEY=sk-... cargo test -p situation_room-pipeline -- \
  --ignored live_author_against_jsspa_excerpt_produces_decline \
  --nocapture
```

This test calls the real xAI provider with an excerpt
hand-crafted to look like an empty SPA skeleton (an
`<div id="root"></div>` with no rendered content). Three outcomes
are possible:

- **`Err(AuthoringError::Declined)`** — the happy path. The model
  recognized the SPA shape and declined. The `eprintln!` surfaces
  the LLM's verbatim reason for the operator to inspect.
- **`Ok(recipe)`** — the model authored a recipe anyway. The test
  fails noisily with the recipe shape; this is informational, not
  a code defect — it means the prompt's decline-path discipline
  (production prompt v1.9) may be too weak for this model on this
  excerpt. Refine v1.10 if real declines also surface this failure
  mode.
- **`Err(other)`** — a different authoring error (URL guard, schema-
  deser, etc). The test fails with the variant; this is likely a
  fixture drift, not a decline-path concern.

The test's prompt template deliberately *omits* the v1.9
decline-path prose section; it only documents `decline_reason` as
"populate this field if no recipe is honestly possible." The model
relies on the structured-output schema rather than prompt-side
hand-holding to find the decline channel — which is the
architectural shape ADR 0007 amendment 4 chose. Production v1.9
*does* include the prose section, so the production decline rate
should be ≥ what this test surfaces.

---

## What landed this session — file inventory

### Frontend (`apps/desktop`)

- `src/components/FetchReport.svelte`
  - Imports `flagRecipe` from `$stores/plans.svelte` and
    `RecipeFlagDialog` from `$components/dialogs/RecipeFlagDialog.svelte`.
  - Local state `flagDialogSourceId: string | null` and
    `flagSubmitting: boolean`, plus the `openFlagDialog` /
    `closeFlagDialog` / `onFlagSubmit` helpers — same shape
    RecipesPanel uses.
  - Declined outcome rows render an `.actions` row (full-width
    grid span, right-aligned) containing either a `flag` button or
    a `FLAGGED` chip, gated on `plans.recipeFeedback[o.source_id]`.
    The `{@const feedback = ...}` binding is the first child of
    the declined-branch `{#if}` for type-narrowing.
  - `<RecipeFlagDialog>` mount conditional on `flagDialogSourceId
    !== null`, with `authoredFrom={'unknown'}` (no recipe was
    authored, so the ADR 0014 stub-hint banner doesn't apply).
  - New CSS rules: `.actions`, `.flag-button`, `.flagged-chip`.
    All mirror the RecipesPanel rules byte-for-byte (chrome,
    sizing, hue) so the affordance reads identically across panels.
  - Two new docblock sections at the top of the file: one
    explaining flag-from-decline rationale, one noting the
    multiple-dialog-mounts caveat (acceptable for the surgical
    patch; lift-to-store is the cleaner refactor when a third
    panel needs it).

### Pipeline (`crates/pipeline`)

- `src/fetch_executor.rs`
  - One new test:
    `live_fetch_against_real_pdf_produces_observation_and_closes_run`.
    `#[ignore]`d. Mirrors `live_fetch_against_real_csv_*` and
    `live_fetch_against_real_json_*` in shape — pre-authors a
    `pdf_table` recipe pinned to a hard-coded coordinate, fetches
    a real USGS MCS PDF over the public internet via real
    `SecureHttpClient`, runs through `UnreachableProvider`, asserts
    structurally on the wiring (recipe attempted, outcome is not
    `Skipped`/`Declined`, audit row closed). Override the URL and
    coordinate via `FETCH_LIVE_PDF_*` env vars.

- `src/recipe_author.rs`
  - One new test:
    `live_author_against_jsspa_excerpt_produces_decline`.
    `#[ignore]`d. Calls real xAI provider with a context whose
    `document_excerpt` is a hand-crafted SPA-skeleton HTML
    (`<div id="root"></div>` and no body content). Asserts
    `Err(AuthoringError::Declined)` is the result; surfaces the
    LLM's verbatim reason via `eprintln!`. The test's prompt
    template documents `decline_reason` in the schema sense but
    omits the v1.9 prose section, so the test exercises whether
    the model declines on its own without prompt-side
    encouragement.

### Documentation

- `STOCKPILE_HANDOFF_SESSION30.md` — this file.

---

## What this session did not change

- The 6 record types stay 6.
- The two-level LLM architecture is unchanged.
- The closed extraction-mode enum stays at 5.
- ADR 0009's security posture is unchanged. The new flag-from-
  decline path goes through the existing `flagRecipe` store helper
  → `set_recipe_feedback` Tauri command → `check_user_text` against
  `Bounds::RECIPE_FEEDBACK` validator → `recipe_feedback` table.
  Same trust boundary, same validator, same persistence. No new
  surface.
- The recipe-author prompt is unchanged. v1.9 remains production.
- The classifier prompt is unchanged.
- The xAI provider integration is unchanged.
- The `recipe_feedback` storage schema (ADR 0013) is unchanged.
- ADR 0012's gate (10 documented Class B failures) is unchanged.
- No new ADR is required. ADR 0013 anticipated the per-(plan,
  source) flag for any failed recipe and any declined source; this
  patch is ADR 0013's deferred UI surface for the decline shape,
  not a new architectural commitment.

---

## Known imperfections

### 1. Two dialog mounts can collide

If the user opens the flag dialog from RecipesPanel, then *also*
opens it from FetchReport before submitting the first one, both
modals will stack visually. Each writes through the same
`flagRecipe` store helper, so the worst case is a confusing UX,
not a state bug. The cleaner long-term shape is lifting
`flagDialogSourceId` into the runes store as a singleton, with
both panels reading the same key. Defer until a third panel
needs the dialog — then the lift earns its weight.

### 2. The flag affordance is decline-only

Failed-apply outcomes in FetchReport do *not* show a flag button —
the operator goes to RecipesPanel to flag the recipe (that affordance
already exists there, since failed-apply outcomes carry a
`recipe_id` and have a recipe row). Adding the same affordance
to failed-apply rows in FetchReport is purely convenience; if the
empirical pattern shows operators often flag from FetchReport, add
it as a follow-up. Today it would be duplicated UI surface for an
already-supported flow.

### 3. The live PDF test's default coordinate may not address a real cell

The default coordinate (`page=2, row=2, col=1`) was chosen against
the synthetic test fixture in `recipe_apply.rs::tests`; it may or
may not address a useful cell on the real USGS lithium MCS PDF. A
`Failed @ Apply` is the test's expected failure mode in that case;
override via env vars to target a specific known-good coordinate.
A future session could ship a curated coordinate per known-stable
USGS chapter — but the `#[ignore]`d nature of the test means that
operator-intervention for tuning is acceptable.

### 4. The live decline test is non-deterministic

The LLM is non-deterministic; the test may pass or fail based on
the model's behaviour at run time. The failure mode is informational
("here's the recipe the model authored when it should have
declined") rather than a code defect. The test serves as a manual
smoke-test of the decline path against the production model, not
as a hard gate.

### 5. Carried forward from Session 29

- Recipe-feedback channel for declines is now wired (this patch);
  the same channel for `pdf_table` failures (Session 29 §C.2)
  technically already works through the existing RecipesPanel
  flag button on the failed-apply recipe — that path lights up
  the moment a `pdf_table` recipe fails at apply time.
- Multi-cell PDF extraction (§C.3) — defer until empirical
  evidence shows the per-cell shape is a real friction point.
- Approach (b) for PDFs the heuristic can't address (§C.4) —
  defer until empirical evidence shows approach (a) failures
  that approach (b) would have addressed.
- Schema regeneration check in CI (§C.5 from S28) — the existing
  substring tests already serve as field-presence tripwires; a
  snapshot test adds marginal value over what's there.
- Anthropic provider stubs — multi-session lift, scope confirmed
  for "full go-live" milestone, not now.
- Apply-runtime strict deserialization is permissive — would
  require careful migration of existing recipes; its own session.
- Crate-level `#![allow(...)]` lint suppressions — high-risk
  without compiler verification of the surfaced warnings; deserves
  its own session.

---

## Suggested Session 31 priorities

In rough order of leverage:

### P1 — Lint suppression sweep (½–1 session)

The three crate-level `#![allow(...)]` blocks
(`crates/llm/src/lib.rs`, `crates/pipeline/src/lib.rs`,
`crates/storage/src/lib.rs`) have been deferred since Phase 1.
A session with the compiler in hand can attempt removal one
crate at a time, fix surfaced warnings (or scope `#[allow(...)]`
to the specific items that legitimately need it), and ship a
clean lint posture. Start with `storage` — only `dead_code` is
suppressed there, smaller blast radius than the other two.

### P2 — Apply-runtime strict deserialization (1 session)

The Session 23/27 carried-forward item. Add `#[serde(deny_unknown_fields)]`
to the recipe types and other apply-path structs; verify that
existing persisted recipes still deserialize cleanly (they
should — the deny is for *new* unknown fields, not existing
ones). The risk is ergonomic: future contributors adding fields
forget to bump the recipe version and the existing-recipe deser
breaks. ADR-level worth considering.

### P3 — Live test of recipe-feedback flowing into a re-author (½ session)

The flag-from-decline patch ships the UI hook; a live
`#[ignore]`d test that classifies + accepts + runs fetch (gets
declined) + flags + runs fetch again (verifies the recipe was
authored *and* the prompt the LLM saw included the operator's
note) would be the natural integration test. Mirror the shape
of the new live decline test; the fetch_executor's authoring
path picks up the feedback automatically (lines 788–803 of
`fetch_executor.rs`), so the test is mostly setup + assert.

### P4 — Anthropic provider (multi-session)

Carried forward to "full go-live" per the Session 30 scope
discussion. Not Session 31's queue.

---

## Files to read first when starting Session 31

In order of importance:

1. This file.
2. `STOCKPILE_HANDOFF_SESSION29.md` — Track C / pdf_table context.
3. `STOCKPILE_HANDOFF_SESSION28.md` — Track B / decline-path context.
4. `docs/adr/0013-recipe-feedback-channel.md` — the recipe-feedback
   channel design that justifies flag-from-decline.
5. `apps/desktop/src/components/FetchReport.svelte` — the file
   this session edited; the new docblock sections explain the
   design choices in context.
6. `apps/desktop/src/components/RecipesPanel.svelte` — the
   sibling consumer of `RecipeFlagDialog`; reference for any
   future refactor that lifts dialog state to the store.

---

## Rules of the road (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All
  HTTP through `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test --package situation_room-api`.
  Never hand-edit. (This patch ships no TS DTO changes — the wire
  shape is unchanged; only the frontend's render of an existing
  shape changes.)
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
  (The new `.flag-button` / `.flagged-chip` CSS reuses `--signal-info`,
  `--fg-tertiary`, `--border-subtle` etc. — the same vars
  RecipesPanel uses for the same chrome.)
- Runes-using files end in `.svelte.ts`, not `.ts`. (This patch
  edits one `.svelte` file and adds no new `.ts` files; the
  existing `outcomes.ts` is plain `.ts` because it has no runes
  state, which is the established pattern.)
- Migrations: read prior migrations before writing a new one. (No
  migrations in this patch — the recipe_feedback table from
  Session 19 is unchanged.)

---

## One thing to look at before any new code

The flag-from-decline UX in this session is the first time two
panels in the app mount the same modal. The local-state pattern
(per-panel `flagDialogSourceId`) preserves RecipesPanel's existing
shape and keeps this patch surgical. **It is also the wrong shape
when a third consumer arrives.** If a future session adds a third
mount (an outcomes drilldown, a coverage report, anything), the
right move is to lift `flagDialogSourceId` into the runes store
as a singleton with `openFlagDialog(sourceId)` /
`closeFlagDialog()` helpers, then mount the dialog in *one* place
(probably `+page.svelte`) and let any panel call into the store.
That refactor is itself a small session — it just doesn't earn
its weight at two consumers.

End of handoff.
