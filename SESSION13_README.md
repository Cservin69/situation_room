# Session 13 — P2 + P5 + duckdb leftover cleanup

Three small frontend polish items from the Session 13 handoff,
plus the one obvious rename leftover that was breaking the
"local DBs must not be committed" rule already on the books.

Apply on top of the green Session 12 P2 build:

    tar -xzf ~/Downloads/session13_patch.tar.gz --strip-components=1 -C .
    rm -f stockpile.duckdb stockpile-e2e.duckdb
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cd apps/desktop && npm install && npm run check
    cd ../..

The `rm` is required: a tarball cannot delete files, only add or
overwrite them. The two DuckDB files at the workspace root are
local user data that the existing `.gitignore` already excludes
via `*.duckdb` — they were committed before that rule was
tightened. Removing them brings the working tree into agreement
with the ignore policy.

No Rust changes; no migrations; no new tests on the Rust side. All
changes are in `apps/desktop/src/`. `cargo` should pass unchanged.

## What this patch does

### P2 — Per-recipe outcome badge on each recipe card

The Session 13 handoff §P2 ergonomics: with the recipes panel landed
in S11 P2.5 and the fetch report panel landed earlier, the user can
see the *list* of recipes and the *list* of outcomes — but matching
"which recipe failed where" required eyeballing recipe IDs across two
panels. This change surfaces each recipe's most-recent fetch outcome
inline on the recipe card itself.

- **`apps/desktop/src/lib/outcomes.ts`** (new). Pure-functions module
  exporting `outcomeTone`, `outcomeLabel`, `outcomeDetail`, and
  `outcomeForRecipe`. Plain `.ts` (no runes), so the file extension
  is correct. Lifted out of `FetchReport.svelte` so the two panels
  can't drift in their rendering of the same `RecipeOutcomeDto` wire
  shape. Adds an `OutcomeTone = 'ok' | 'skip' | 'fail' | 'none'` —
  the fourth value covers the recipes-panel-only case of "this
  recipe wasn't part of the most recent run".

- **`apps/desktop/src/components/RecipesPanel.svelte`** (rewritten).
  Each recipe card now shows an outcome strip below the header:
  - `"N records"` in positive green when the recipe succeeded.
  - `"failed @ apply"` in negative red when the recipe failed at
    apply (or fetch, or insert), with the failure message
    expandable in a `<details>`.
  - `"skipped"` in chrome grey with the reason expandable.
  - `"no fetch run yet"` in tertiary grey when the recipe hasn't
    been touched by a run.
  Tone semantics + label/detail strings come from the shared module.
  Border-left accent mirrors the FetchReport outcome rows so the
  visual language stays consistent across the two panels.

  The recipe-card head, URL row, extraction/produces blocks, and
  footer are unchanged.

### P5 — Distinct empty-state when zero recipes were attempted

The Session 13 handoff §P5 footgun: the Session 12 production run
on "hungary's frozen EU funds" classified, accepted, fetched → 0
recipes attempted, no signal in the UI why. The diagnosis (from
the handoff): no source in `config/sources.toml` had authoritative
coverage, so the classifier's `document_sources` hints didn't bind
to any registered descriptor at authoring time, so
`load_or_author_recipes` had nothing to do.

- **`apps/desktop/src/components/FetchReport.svelte`** (rewritten).
  When `report.recipes_attempted === 0` and `report.outcomes` is
  empty, the panel now shows a dedicated message:

      No recipes were attempted.
      The plan's document sources didn't bind to any registered
      source in config/sources.toml. Either add a matching source
      descriptor, or re-classify the topic in terms the registry
      covers.

  The message uses an amber border-left to distinguish it from the
  plain "no outcomes" tertiary-grey case (case 2 in the file's
  in-code taxonomy comment — defensive only; shouldn't happen in
  practice).

  This is a local fix, not a coverage report. The handoff §P5 was
  explicit: full coverage reporting is its own design — see the
  ADR 0007 deferred CoverageReport. This narrows the immediate
  ergonomic gap without preempting that work.

- Same file: the long-standing design-token drift from S6/S11 is
  fixed in passing. The component used `var(--signal-ok, #5b9c5e)`
  and `var(--signal-error, #c83c3c)` — the named vars don't exist
  in `global.css`, so the hex fallbacks were what actually painted.
  Both the missing-var ergonomic problem and the embedded-hex
  policy violation (handoff hard rules: "Components only use CSS
  vars from global.css. No hardcoded hex.") are resolved by
  switching to the canonical `--signal-positive` /
  `--signal-negative` tokens and dropping the fallbacks. ADR 0006.

### duckdb leftover cleanup (rename followup)

- **`stockpile.duckdb` and `stockpile-e2e.duckdb`** at the
  workspace root are removed by the apply step above. The existing
  `.gitignore` already lists `*.duckdb` so these were committed
  before the policy was tightened; the `rm` brings the working
  tree into agreement with the ignore. The `situation_room` rename
  did not touch these by accident.

- **`.gitignore`** drops two leftover lines:

      stockpile_codebase.txt
      stockpile_delta_*.zip

  The `situation_room_codebase.txt` and `situation_room_delta_*.zip`
  equivalents that supersede them are already present (lines 49-50
  of the pre-patch file). Removing the stockpile entries closes
  the loop on a small piece of post-rename drift.

## What this patch does NOT do

- **Does not rename the workspace crates.** `crates/*/Cargo.toml`
  package names remain `situation_room-*` (with underscore). Going
  to dashes (`situation-room-*`) is a much larger Cargo-resolver
  and lockfile change; its own session if and when. The Session 13
  handoff's hard rules are explicit that we don't deviate, and
  this is outside the handoff's scope.

- **Does not rename `apps/situation_room/`'s package** from
  `situation_room-situation-room` to something cleaner. Cosmetic
  and Cargo-output-only; same reasoning as above.

- **Does not promote `RegexCapture` (handoff §P3).** Out of scope
  for this session; mechanically identical to the Session 12
  CssSelect work when the time comes.

- **Does not author ADR 0012 (handoff §P4).** Wants more failure
  shapes from real runs; revisit after the diagnostic improvements
  in this patch make those shapes easier to read.

- **Does not bump the recipe-author prompt to v1.4.** Same Session
  5/12 discipline.

## Verifying after apply

The Rust workspace is unchanged; `cargo check` / `cargo test` /
`cargo clippy --workspace --all-targets -- -D warnings` should be
green with the same counts as the end of Session 12 (260 unit
tests + 6 ignored).

For the frontend:

1. `npm run check` (in `apps/desktop/`) should pass with no new
   svelte-check warnings. The new shared module is plain `.ts` and
   exports four pure functions; the two component changes consume
   it via `import { ... } from '$lib/outcomes'`.
2. `./scripts/run_desktop.sh` should boot cleanly.
3. Open a previously-fetched plan from the listing. The recipes
   panel now shows an outcome strip per recipe matching the
   FetchReport outcomes. Click an outcome strip with `details` to
   confirm the expandable detail renders.
4. Classify a topic that won't bind to any registered source
   (e.g. "underwater basket weaving regulations"), accept it,
   click Run fetch. The FetchReport should render the dedicated
   "No recipes were attempted" amber-bordered block instead of
   the previous bare zero-counters.

## Files in this patch

    .gitignore
    apps/desktop/src/lib/outcomes.ts                 (new)
    apps/desktop/src/components/RecipesPanel.svelte
    apps/desktop/src/components/FetchReport.svelte

Three files updated, one new. Plus the `rm` of the two stale
DuckDB files, which the tarball can't do for you.
