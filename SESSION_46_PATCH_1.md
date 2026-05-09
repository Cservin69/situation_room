# Session 46 — Patch 1

Operator-introspection surfaces over data the runtime already
produces (handoff piece A — recipe-success heatmap — bundled with
the live-run-flagged piece 2 — expectation coverage matrix — per the
operator's "attack 1 and 2 in one Go" override of the handoff's
"pick one" rule). The handoff's premise that piece A required no
migration turned out to be wrong on closer reading: `recipe_fetch_attempts`
only writes apply-stage failures, `fetch_runs` carries summary
counters only, and `Declined` outcomes have no `recipe_id` at all.
This patch adds migration v16 (`fetch_run_outcomes`) — the
"per-recipe outcome detail" table migration 0006 explicitly named
as deferred until per-recipe history mattered. The handoff's
constraint is the rule the rule was wrong about; the migration is
the principled fix the table's own header comment already
predicted.

`HostBackoff::snapshot()` ships as a drive-by — pure read accessor
plus DTO mirror — so piece B (per-host backoff status panel) is one
screen of work next session.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

One new migration (v16). Three new ts-rs DTOs (heatmap entry, heatmap
cell, coverage row, coverage recipe chip, host-backoff snapshot —
five total derives, four .ts files in
`apps/desktop/src/lib/api/types/`). Two new IPC commands. Two new
Svelte components slotted into `PlanReview.svelte`. Pure additive
across the workspace; existing reads are unaffected.

Build + test green: 319/319 pipeline (up from 315), 61/61 secure,
all other crates green; 12 ignored remain the existing `#[ignore]`
live integration tests. UI check green after `npm install`.

## Files changed

### Migration

- `migrations/0016_fetch_run_outcomes.sql` —
  - New `fetch_run_outcomes` table. Columns:
    `id` (UUIDv7 PK), `run_id`, `plan_id`, `recipe_id` (nullable for
    `Declined` / `LegacyPlanCannotAuthor`), `source_id`,
    `outcome_kind` (TEXT, closed enum enforced in Rust),
    `records_produced` (nullable; populated for `Succeeded`),
    `retry_after_seconds` (BIGINT, nullable; populated for
    `RateLimited` with parsed Retry-After), `failure_stage` (TEXT,
    nullable; populated for `Failed`), `message` (TEXT, nullable;
    populated for `Failed` / `Skipped` / `Declined`), `attempted_at`.
  - Three indexes:
    `idx_fetch_run_outcomes_plan_attempted_at`,
    `idx_fetch_run_outcomes_run_id`,
    `idx_fetch_run_outcomes_recipe_id` (regular B-tree; DuckDB
    doesn't support partial indexes, NULL-aware queries read
    contiguously past the NULL bucket).
  - Header comment spells out (a) why `fetch_runs` and
    `recipe_fetch_attempts` don't fit, (b) why two nullable id
    columns rather than polymorphic, (c) why no backfill, (d) the
    closed `outcome_kind` vocabulary matches `RecipeOutcomeDto::kind`
    on the wire one-for-one.

- `crates/storage/src/migrate.rs` —
  - Added the v16 migration entry referencing
    `migrations/0016_fetch_run_outcomes.sql`.

### Storage

- `crates/storage/src/fetch_run_outcomes.rs` (new) —
  - `FetchRunOutcomeKind` closed enum: `Succeeded` | `Skipped` |
    `Failed` | `RateLimited` | `Declined` | `LegacyPlanCannotAuthor`.
    `as_str()` writes the same lowercase strings the IPC's
    `RecipeOutcomeDto::kind` already uses; `FromStr` rejects
    unknown values with a hard error (same posture as
    `AuthoredFrom::from_str` from migration 0010).
  - `FetchRunOutcomeRow` / `StoredFetchRunOutcome` — write/read row
    shapes (mirror pattern, same as `RecipeRow` / `StoredRecipe`).
  - `Store::insert_fetch_run_outcome` — write site, called by the
    fetch executor at run completion.
  - `Store::fetch_run_outcomes_for_plan` — flat per-plan list,
    newest-first. Useful for tests and ad-hoc inspection; the
    history grouping query composes on top.
  - `Store::recipe_outcomes_history_for_plan(plan_id, run_limit)` —
    the heatmap's primary read. Returns one
    `RecipeOutcomeHistoryEntry` per distinct `(recipe_id, source_id)`
    pair, with that pair's per-run cells ordered oldest-first.
    `run_limit` clamps the runs (column) dimension; recipes that
    only appear in older runs fall off the result.
  - 9 unit tests: closed-enum round-trip, succeeded round-trip,
    declined-without-recipe-id round-trip, per-plan filter,
    newest-first ordering, history-empty-when-no-rows, grouping by
    (recipe_id, source_id) including the no-recipe-id branch,
    per-entry oldest-first ordering, runs-dimension clamp,
    per-variant payload (RateLimited.retry_after_seconds) carries
    through the join.

- `crates/storage/src/lib.rs` —
  - `fetch_run_outcomes` module added to the module list and the
    public re-exports (`FetchRunOutcomeKind`, `FetchRunOutcomeRow`,
    `RecipeOutcomeHistoryEntry`, `RecipeOutcomeHistoryRunRow`,
    `StoredFetchRunOutcome`).

### Pipeline / executor

- `crates/pipeline/src/fetch_executor.rs` —
  - New `persist_run_outcomes(store, plan_id, run_id, outcomes)`
    helper that writes one `fetch_run_outcomes` row per
    `RecipeOutcome`. Storage failures warn-log per row and skip;
    the user-facing `FetchReport` is preserved (same posture as
    `record_apply_failure_attempt` and `update_fetch_run`).
  - `failure_stage_as_str(FailureStage)` — wire-form helper for the
    `failure_stage` column. Same snake_case as serde's default;
    kept as a free function so it stays adjacent to the call site.
  - `run_fetch_for_plan` calls `persist_run_outcomes` in step 5a,
    immediately before the existing `update_fetch_run` close
    (step 5b). Matches the "auxiliary writes are non-fatal" pattern
    the surrounding code already documents.
  - Imports added:
    `situation_room_storage::fetch_run_outcomes::{FetchRunOutcomeKind,
    FetchRunOutcomeRow}`.
  - One new test pinning Succeeded outcome row persistence
    (round-trips through `fetch_run_outcomes_for_plan`); one new
    test for legacy plans pinning the no-recipe-id branch
    (`recipe_id` is None for `LegacyPlanCannotAuthor` rows).

- `crates/pipeline/src/fetch_backoff.rs` —
  - `HostBackoff::snapshot()` accessor — enumerates the per-host
    state map, returns `Vec<HostBackoffSnapshot>`. Pure read; takes
    a `Duration::ZERO` for hosts whose schedule has expired so the
    "0 wait + counter > 0" recovering case is distinguishable from
    "0 wait + counter == 0" clean state.
  - `HostBackoffSnapshot { host, consecutive_failures, wait_remaining }`
    public struct. `wait_remaining` is a `Duration`; the api
    crate's DTO converts to whole seconds.
  - 3 new tests: empty snapshot on fresh state, recorded failures
    surface with correct counter and wait, success preserves the
    row with counter reset to 0.

### API / IPC

- `crates/api/src/types_export.rs` —
  - Five new ts-rs DTOs: `RecipeOutcomesHistoryRunCellDto`,
    `RecipeOutcomesHistoryEntryDto`, `ExpectationCoverageRowDto`,
    `ExpectationCoverageRecipeDto`, `HostBackoffSnapshotDto`.
  - `outcome_kind` on the cell DTO is a `String` chosen from the
    same closed set `RecipeOutcomeDto::kind` already uses; the
    frontend's `outcomeTone` helper renders history cells
    identically to live outcomes.
  - `HostBackoffSnapshotDto::from_typed` collapses the
    `Duration` to `u64` whole seconds for the wire (sub-second
    precision isn't useful at the presentation layer).

- `crates/api/src/commands.rs` —
  - New constant `AppState::MAX_OUTCOMES_HISTORY_RUNS = 50`.
  - New IPC command `recipe_outcomes_history(plan_id, run_limit)` —
    pure read, clamps `run_limit` against the new constant, lifts
    typed `RecipeOutcomeHistoryEntry`s into the wire DTO.
  - New IPC command `expectation_coverage(plan_id)` — pure read.
    Loads the plan + recipes, walks `produces[].expectation`
    references against the plan's four binding-addressable buckets
    (`observation_metric`, `event_type`, `entity_kind`,
    `relation_kind`), and emits one row per (bucket, index) the
    plan declares. Uncovered buckets surface with `recipes` empty;
    orphan bindings (recipes targeting an index the plan no longer
    declares) surface with `label = ""` so the operator sees the
    inconsistency.
  - New free function `build_expectation_coverage(plan, recipes)` —
    the pure walker, lifted out of the command body so it's
    testable against synthetic plan + recipe pairs without a Store.
  - 4 new unit tests: uncovered expectations surface, multiple
    recipes group under one row, orphan bindings render with empty
    label, malformed `produces_json` doesn't crash the matrix
    (parse-on-error fallback, same posture as
    `RecipeDto::from_stored`).
  - Imports updated:
    `ExpectationCoverageRecipeDto`, `ExpectationCoverageRowDto`,
    `RecipeOutcomesHistoryEntryDto` added to the `types_export` use
    list.

- `apps/desktop/src-tauri/src/main.rs` —
  - Two new commands registered in the `tauri::generate_handler!`
    macro: `recipe_outcomes_history`, `expectation_coverage`.
    Comment block names them as Session 46 operator-introspection
    surfaces over existing data; "no LLM call".

### Frontend

- `apps/desktop/src/lib/api/client.ts` —
  - `recipeOutcomesHistory(planId, runLimit)` and
    `expectationCoverage(planId)` typed wrappers (default
    `runLimit = 20`).
  - Type imports added for the two new DTO families.

- `apps/desktop/src/stores/plans.svelte.ts` —
  - State extended with `outcomesHistory: RecipeOutcomesHistoryEntryDto[]`
    and `expectationCoverage: ExpectationCoverageRowDto[] | null`.
  - `selectPlan` and `runFetch` fire-and-forget refresh both
    surfaces; `clearSelection` resets them.
  - Two new exported helpers: `refreshOutcomesHistory(planId)`,
    `refreshExpectationCoverage(planId)`. Same non-fatal posture as
    `refreshFetchRuns` / `refreshRecipes` / `refreshRecords`.

- `apps/desktop/src/components/RecipeOutcomesHeatmap.svelte` (new) —
  - Horizontal strip: row per (recipe_id ?? source_id), cell per
    run. Cell tone reuses the closed `OutcomeTone` set
    (`ok`/`skip`/`fail`/`limited`/`declined`/`legacy`) from
    `outcomes.ts` so colours match the FetchReport panel exactly.
  - Hover detail surfaces the kind, run id, attempted_at, and
    per-variant payload (records produced, failure stage, retry-
    after, message). Coerces `bigint` → `Number` for retry-after
    interpolation (ts-rs v8+ emits Rust `u64` as TS `bigint`).
  - Empty state hint when `plans.outcomesHistory.length === 0`
    (pre-Session-46 plans, or freshly-accepted plans before their
    first fetch under the new persistence path).

- `apps/desktop/src/components/ExpectationCoverage.svelte` (new) —
  - Matrix surface: row per (bucket, index) with covered / uncovered
    border treatment (green / dim-neutral, mirroring FetchReport's
    `succeeded` / `declined` row borders). Recipes appear as chips
    showing `recipe_id` prefix + `source_id`. Orphan rows render an
    italic warning marker rather than blanking the row.
  - `$derived(plans.expectationCoverage)` carries the
    `T[] | null → T[]` narrowing through the template (proxy
    re-accesses inside `{#if}` blocks don't preserve the narrowed
    type to subsequent expressions; svelte-check flags it).
  - Renders nothing when the matrix hasn't loaded
    (`plans.expectationCoverage === null`); the surrounding bucket
    panels in `PlanReview.svelte` already cover the
    pending-or-pre-load state.

- `apps/desktop/src/components/PlanReview.svelte` —
  - Imports `RecipeOutcomesHeatmap` and `ExpectationCoverage`.
  - Slots both between the existing `FetchReport` (above) and
    `RecipesPanel` (below). Comment block in the template explains
    the vertical scan order: "what just happened" → "history of
    what happened" → "the recipes themselves" → "what each recipe
    covers / what's uncovered".

### Generated TypeScript (regenerated by ts-rs at `cargo test`)

- `apps/desktop/src/lib/api/types/RecipeOutcomesHistoryEntryDto.ts`
- `apps/desktop/src/lib/api/types/RecipeOutcomesHistoryRunCellDto.ts`
- `apps/desktop/src/lib/api/types/ExpectationCoverageRowDto.ts`
- `apps/desktop/src/lib/api/types/ExpectationCoverageRecipeDto.ts`
- `apps/desktop/src/lib/api/types/HostBackoffSnapshotDto.ts`

## What's NOT in scope

- **IPC for `HostBackoffSnapshotDto`.** The DTO and the
  `HostBackoff::snapshot()` accessor exist; mounting the IPC
  command and Svelte panel is piece B's job next session.
- **Backfill of historical outcomes.** Pre-Session-46 runs in
  `fetch_runs` have no per-outcome rows here. The heatmap renders
  those runs as missing columns rather than fabricating cells.
- **Per-bytes capture for successful runs.**
  `recipe_fetch_attempts` continues to write only apply-stage
  failures; the "what bytes produced this record" panel still
  earns its weight separately.
- **Multiple recipes per nomination.** The live USGS run produced
  one recipe targeting one of four obs metrics. The matrix surface
  now makes that visible; the architectural piece that would let a
  source author N scalar recipes for N expectations is its own
  session — not a prompt edit (the prompt's narrow-coverage
  discipline is intentional).
- **Charts on Observations / Events.** Still gated on promotion
  (ADR 0004) per the Session-46 handoff.

## Test deltas

- `crates/storage/src/fetch_run_outcomes.rs` — 9 new tests.
- `crates/pipeline/src/fetch_executor.rs` — 2 new tests.
- `crates/pipeline/src/fetch_backoff.rs` — 3 new tests
  (snapshot accessor).
- `crates/api/src/types_export.rs` — 5 new ts-rs export bindings
  (auto-emitted as tests by ts-rs's `#[ts(export)]`).
- `crates/api/src/commands.rs` — 4 new tests for
  `build_expectation_coverage`.

Pipeline test count: 315 → 319 (4 net). Other crates' counts
unchanged. All ignored tests (12) are the existing `#[ignore]` live
integration tests.

End of patch.
