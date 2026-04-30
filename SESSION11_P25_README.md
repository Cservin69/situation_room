# Session 11 P2.5 — Recipe inspection panel

Adds a recipe-inspection panel to the desktop app's plan review pane.
The Level-2 authored recipes have always been in the database;
they just weren't visible in the UI. With this patch, every
authored recipe shows its `source_id`, `source_url`, full
`extraction` spec, and full `produces` bindings inline alongside
the plan body.

The motivation is the Session 11 first-real-plan run on "Swiss
national debt" which produced two recipes, both of which failed —
World Bank with HTTP 400 (LLM picked country code `CH` instead of
the alpha-3 `CHE`) and IMF with `$.values[-1]` matching no nodes
(LLM picked an endpoint with a year-keyed response shape, which
standard JSONPath can't address). Diagnosing those failures
required a DuckDB query against the `recipes` table. With this
panel, the same diagnosis happens in the desktop app at a glance.

Apply on top of the green Session 10 build:

    tar -xzf ~/Downloads/session11_p25_patch.tar.gz --strip-components=1 -C .
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    # then re-run the desktop app to see the panel

## What this patch does

### Backend (3 files)

- `crates/api/src/types_export.rs` — new `RecipeDto` with `From<StoredRecipe>`.
  Strongly-typed scalar fields; `extraction` and `produces` carry
  through as `serde_json::Value` (`unknown` in TypeScript). Two
  unit tests cover the round-trip happy path and the
  malformed-JSON-surfaces-as-`_parse_error` path.
- `crates/api/src/commands.rs` — new `list_recipes_for_plan(plan_id)`
  Tauri command. Mirrors `list_fetch_runs` exactly. New
  `AppState::MAX_RECIPES_LISTING = 100` ceiling defends the IPC
  payload against pathological recipe counts.
- `apps/desktop/src-tauri/src/main.rs` — registers the new command
  in `invoke_handler!` (eight commands now total).

### Frontend (5 files)

- `apps/desktop/src/lib/api/types/RecipeDto.ts` — generated TS type,
  hand-written to match what `cargo test --package situation_room-api`
  would emit. Gets overwritten on next test run; the hand-written
  copy means the SvelteKit build doesn't break in the meantime.
- `apps/desktop/src/lib/api/client.ts` — new `listRecipesForPlan`
  wrapper.
- `apps/desktop/src/stores/plans.svelte.ts` — new `recipes:
  RecipeDto[]` field on the plans state, new `refreshRecipes`
  helper, hooks into `selectPlan` (load alongside fetch runs) and
  `runFetch` (refresh after Level-2 authoring runs).
  `clearSelection` resets the array so previous-plan recipes don't
  bleed across selection boundaries.
- `apps/desktop/src/components/RecipesPanel.svelte` — new component.
  One card per recipe: source_id and short id in the header, URL in
  monospace, two collapsible details panels for `extraction`
  (open by default — that's what you want to look at) and
  `produces` (collapsed — that's the noisier of the two). Renders
  nothing when `plans.recipes` is empty (the legitimate state for
  a freshly-classified plan).
- `apps/desktop/src/components/PlanReview.svelte` — mounts the new
  component below the FetchReport panel. Two-line addition.

## What this patch does NOT do

- **No per-mode bespoke rendering.** `extraction` is rendered as
  pretty-printed JSON. The five-mode closed enum (`json_path`,
  `css_select`, `csv_cell`, `pdf_table`, `regex_capture`) could each
  get a custom view (e.g. CssSelect could show the selector with
  syntax highlighting), but that's polish work for a future session
  if this view ever ships beyond the developer audience. JSON
  faithfulness > visual polish, for now.
- **No fetch-run cross-reference.** The panel doesn't show which
  recipe contributed to which `RecipeOutcome` in the latest
  `FetchReport`. The matching is by `recipe_id` — both DTOs carry
  it — and a future iteration could highlight failed recipes in
  the panel based on the most recent run. Not in scope for this
  patch; the FetchReport's per-recipe outcomes plus the panel's
  recipe text are enough to triangulate by hand.
- **No edit / re-author / delete.** Recipes are immutable from the
  UI's perspective. Re-authoring on failure is the deferred Session
  11 Option C, which the handoff still flags as needing a failure-
  mode taxonomy first.
- **No copy-to-clipboard for URLs.** The URLs are selectable text;
  click-to-copy is a half-day polish that doesn't unblock anything.

## Tests added

- `recipe_dto_round_trips_from_stored_happy_path` — well-formed
  JSON in `extraction_json` / `produces_json` parses to typed
  `Value`s on the wire.
- `recipe_dto_surfaces_corrupt_extraction_as_structured_error` —
  malformed JSON in `extraction_json` produces a
  `{"_parse_error": "...", "_raw": "..."}` object on the wire
  rather than crashing the listing. (This shouldn't normally
  happen — the executor authors valid JSON — but defends against
  hand-edits and future schema changes.)

No new tests on the command itself; it's a thin wrapper over the
already-tested `recipes_for_plan` storage method, mirroring the
shape of the also-thin `list_fetch_runs`.

## Files in this patch

    apps/desktop/src-tauri/src/main.rs
    apps/desktop/src/components/PlanReview.svelte
    apps/desktop/src/components/RecipesPanel.svelte                 (NEW)
    apps/desktop/src/lib/api/client.ts
    apps/desktop/src/lib/api/types/RecipeDto.ts                     (NEW)
    apps/desktop/src/stores/plans.svelte.ts
    crates/api/src/commands.rs
    crates/api/src/types_export.rs

Eight files. Three new, five updated.

## Verifying after apply

The fastest end-to-end check:

1. Build the desktop app: `./scripts/run_desktop.sh` (or your
   preferred dev command).
2. Open the plan you classified earlier: `019dda03-...` (the Swiss
   debt one).
3. The recipe panel appears below the fetch report. You should see
   two cards: `world_bank_indicators` with the malformed `CH`-not-
   `CHE` URL, and `imf_weo` with `$.values[-1]`. Both rendered
   inline, no DuckDB needed.
