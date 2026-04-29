# STOCKPILE — Session 8 handoff

You are starting Session 8. Session 7 shipped the plan-lifecycle
gate: plans now carry a three-state `status` (`pending` /
`accepted` / `rejected`), and the user accepts or rejects each plan
through the desktop UI. The frontend's filter strip lands on
Pending by default; accept/reject buttons appear in the review
pane; a status pill shows the current state in the listing.

Read this whole document before writing any code. Read ADR 0011
(plan lifecycle) and re-read ADR 0007 (research function: two-level
LLM architecture) — both are authoritative for this session.

## What works today

- Five Tauri commands: `classify`, `list_recent_plans` (now with an
  optional status filter), `get_plan`, `accept_plan`, `reject_plan`.
- `Store::set_plan_status` and
  `Store::recent_research_plans_by_status` in the storage crate.
  `PlanStatus` enum re-exported from `stockpile_storage`.
- `PlanStatusDto` in the api crate, mirrored to TS via ts-rs.
- `ResearchPlanDto::from_typed_pending` / `::from_typed_with_status`
  / `::from_stored` — three explicit constructors. There is no
  blanket `From<ResearchPlan>`; choose explicitly.
- `recipe_apply` runtime is shipped and tested (Session 3c). It
  takes a `FetchRecipe` plus pre-fetched bytes and produces typed
  records for four of five extraction modes. PDF table extraction
  returns `NotImplemented` per ADR 0007's Session-3 review note.
- `recipes_store::save_recipe` and `recipes_store::load_recipe`
  exist; `recipes` table has `(plan_id, source_id)` index.
- The desktop GUI has a working Pending → Accepted flow: the user
  classifies a topic, reviews the plan, clicks Accept. After
  Session 7 the plan sits at status=accepted and nothing else
  happens. **That "nothing else happens" is what Session 8 changes.**


## Session 8 priority

Build the **fetch executor** end-to-end against one source for one
extraction mode, with a UI button that triggers it on an accepted
plan. This is the first session where ADR 0007's "runtime is
LLM-free" claim is demonstrated by a UI flow rather than only by
the recipe_apply unit tests.

The slice is deliberately narrow. Subsequent sessions widen it.

### Scope

- One source. Pick from `config/sources.toml` whichever has the
  cleanest CSV endpoint. The slice does not need to be impressive;
  it needs to be end-to-end.
- One extraction mode: `CsvCell`. Already implemented in
  `recipe_apply`. The other modes stay as they are.
- One record type: `Observation`. The CSV path naturally produces
  observations; events / entities / relations come later.

### What "end-to-end" means here

1. User accepts a plan in the UI.
2. User clicks a new "Run fetch" button (only visible when
   `status === 'accepted'` and no fetch has run yet for this plan).
3. The frontend calls a new Tauri command, `run_fetch_for_plan(id)`.
4. The command:
    - Loads the accepted plan.
    - Loads recipes for the plan from `recipes` table.
    - If no recipes exist, runs Level-2 authoring against the plan
      (`pipeline::recipe_author::author_recipe`). One recipe per
      bound source, persisted via `recipes_store::save_recipe`.
    - For each recipe whose `extraction` is `CsvCell`: fetch the
      URL via `SecureHttpClient`, run `recipe_apply::apply_recipe`,
      normalize the resulting record(s) via `pipeline::normalize`,
      insert into the storage layer via the existing record paths.
    - Returns a `FetchReportDto` summarizing what happened: how
      many recipes ran, how many records produced, how many failed
      and why.
5. The frontend renders the `FetchReportDto` in the review pane
   (a small panel above the buckets, or below the buttons — your
   call).

### Why CSV first

CSV is the only mode currently in `recipe_apply` that round-trips
deterministically without HTML or JSON path complications. JSON
selectors can be brittle against API responses that the LLM saw at
recipe-author time but have since shifted shape; HTML selectors
have the same issue plus structural fragility. CSV is the cleanest
demonstration that the executor's plumbing is right; the harder
modes get their own sessions once the plumbing is proven.

### Storage changes (crates/storage)

- New migration v6: a `fetch_runs` table.
    - `id UUID PRIMARY KEY`
    - `plan_id UUID NOT NULL`
    - `started_at TIMESTAMPTZ NOT NULL`
    - `finished_at TIMESTAMPTZ` (nullable — populated on completion)
    - `recipes_attempted INTEGER NOT NULL DEFAULT 0`
    - `recipes_succeeded INTEGER NOT NULL DEFAULT 0`
    - `records_produced INTEGER NOT NULL DEFAULT 0`
    - `error_summary TEXT` (nullable — top-level error if the run
      failed before processing any recipe)
    - Index on `(plan_id, started_at DESC)`.
- The same DuckDB `ALTER COLUMN` lesson as Session 7 applies: do
  not try to add a column with a constraint to an existing table.
  This migration creates a fresh table so the issue doesn't arise,
  but **read migration 0005's comment block** before writing the
  SQL. The pattern is "TEXT column with DEFAULT, Rust enum is the
  load-bearing invariant". Repeat it.
- New `Store` methods: `insert_fetch_run`, `update_fetch_run` (for
  the started → finished transition + counters), and
  `recent_fetch_runs_for_plan(plan_id, limit)`.
- `Store::recipes_for_plan(plan_id)` — currently missing, needed by
  the executor. Mirrors `recent_research_plans_by_status` in shape.

### Pipeline changes (crates/pipeline)

- New module `pipeline::fetch_executor`. Public entry point:
  `run_fetch_for_plan(store, http, provider, plan_id) -> FetchReport`.
    - The function is *not* a `#[tauri::command]` itself — that's the
      api crate's job. Pipeline stays Tauri-agnostic per ADR 0001.
    - Internal structure: load plan, load-or-author recipes, iterate
      recipes, build `FetchReport` with per-recipe outcomes.
    - Authoring step is conditional: if `recipes_for_plan(plan_id)`
      returns a non-empty Vec, skip authoring. Re-authoring on
      failure is **not** in scope for Session 8 (per ADR 0007 it
      happens "on demand when a recipe fails validation"; that's a
      later session).
- `FetchReport` is a typed struct. Mirrors to a `FetchReportDto`
  in api/types_export. Carry per-recipe outcomes (succeeded /
  failed-with-reason) so the UI can render which sources worked.
- The executor uses `SecureHttpClient` for every HTTP call. No
  fresh `reqwest::Client::new()`. ADR 0009 §"The rule".

### API changes (crates/api)

- New command `run_fetch_for_plan(id: String)`. Validates id,
  checks the plan's status is `accepted` (return
  `CommandError::InvalidInput { field: "id", message: "plan must
  be accepted before fetch" }` if not — or add a new variant if
  you prefer; `InvalidInput` reads odd here but is honest about
  the source of the problem). Returns `FetchReportDto`.
- New command `list_fetch_runs(plan_id: String, limit: usize)` —
  returns `Vec<FetchRunSummaryDto>`. The UI shows the last few
  runs of a plan so a re-fetch is visible context.
- Wire both new commands in `apps/desktop/src-tauri/src/main.rs`'s
  `invoke_handler!`. Use the full path form
  (`stockpile_api::commands::run_fetch_for_plan`); see Session 6
  for why bare names break.
- Two new ts-rs DTOs: `FetchReportDto` (with nested
  `RecipeOutcomeDto`) and `FetchRunSummaryDto`.

### Frontend changes

- New `RunFetchButton.svelte` in the review pane. Visible when
  `plan.status === 'accepted'`. Triggers the `run_fetch_for_plan`
  command via the runes store. Disabled while in flight.
- New `FetchReport.svelte` panel. Renders the most recent
  `FetchReportDto` for the selected plan: total records produced,
  per-source success/failure list, timestamps. Sits above or below
  the bucket grid (your call).
- The runes store gains `runFetch(planId)` and a `fetchReport`
  state field. Optimistic update is *not* needed here — the
  executor is fast (single CSV) but the report is the response
  payload, so just await it.

### Tests to write

- Storage: round-trip `fetch_runs`, `recipes_for_plan` returns
  recipes for the right plan only.
- Pipeline: `fetch_executor` against an in-memory CSV fixture
  (point a recipe at a `data:text/csv,...` URL or a local file
  via a test-only HTTP mock — see how `recipe_apply` tests do
  this).
- API: `run_fetch_for_plan` rejects pending plans, succeeds on
  accepted plans, error variants serialize cleanly.
- Live: one `#[ignore]` test that classifies a real topic, accepts
  it, runs fetch against a real source, asserts ≥1 observation
  produced. Same shape as the Session 3 live xAI tests.


## What Session 8 is explicitly NOT

- **Not the other four extraction modes.** JSON / CSS / regex /
  PDF stay as `recipe_apply` already has them. JSON and CSS
  recipes will be authored by Level-2 but the executor will
  surface them as "skipped: extraction mode not yet enabled in
  executor" or similar. PDF stays `NotImplemented`.
- **Not recipe re-authoring on failure.** A failed recipe surfaces
  in the `FetchReport`; the user looks at it and decides what to
  do. Auto-re-authoring lands when the failure-mode taxonomy is
  better understood; speculating now produces the wrong abstraction.
- **Not freshness tracking, panel rendering, or anomaly detection.**
  Records get inserted; nothing reads them yet from a panel UI.
  That's Session 9+.
- **Not multi-plan or background fetch.** One plan, one click, one
  synchronous-from-the-user's-perspective run.
- **Not coverage reports.** ADR 0007 mentions an all-gaps
  CoverageReport for unmatched plans; that's a Session 9+ thing.
- **Not editing the plan or transitioning out of accepted.** Both
  forbidden by ADR 0011.


## Hard rules (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All
  HTTP through `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`, not internal error types.
  Add a variant if `run_fetch_for_plan` failures need a distinct
  kind from existing ones — likely yes. Suggested:
  `FetchFailed { recipes_attempted, recipes_succeeded, message }`.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test --package stockpile-api`. Never
  hand-edit. Ship the regenerated files in the patch tarball so
  the SvelteKit build passes immediately after extract.
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Migrations: read `migrations/0005_research_plan_status.sql`
  before writing migration 6. The DuckDB `ALTER` constraint trap
  is real. Creating a fresh table sidesteps it; the trap only
  bites when altering an existing table that has indexes on it.


## First thing to do in Session 8

Read this file. Read ADR 0011. Re-read ADR 0007 (especially the
"runtime path" section and the Session-3 review note about the
closed extraction-mode enum). Then look at the existing
`recipe_apply` end-to-end CSV test in
`crates/pipeline/src/recipe_apply.rs::tests::end_to_end_csv_recipe_produces_observation`
— that test is the closest existing thing to what the executor
does. It's the template.

Build incrementally:

1. Migration v6 + `fetch_runs` Store methods + `recipes_for_plan`
   method + tests. `cargo test --workspace` passes.
2. `pipeline::fetch_executor::run_fetch_for_plan` against an
   in-memory CSV fixture. `cargo test -p stockpile-pipeline`
   passes.
3. `FetchReportDto` + the two new commands + their unit tests.
   `cargo check --workspace` passes.
4. Frontend: store helper, RunFetchButton, FetchReport panel.
5. Pick a real CSV source from `config/sources.toml` (or add one
   if none fits — a small one). Classify a related topic. Accept.
   Run fetch. Verify a record landed.
6. Live test (one `#[ignore]`) that does steps 5 in code.

That order is so that every step has a green build behind it. Do
not write the entire session and then run cargo check at the end.


## Continuity note

The continuity note from Session 7 still applies. The human you
are working with is rigorous about security, prefers honesty about
uncertainty over false confidence, and reacts well to direct
disagreement when warranted. Stick to the plan. If you need to
deviate, say so and explain why. The "do not deviate" discipline
holds.

One specific carry-over from Session 7's debugging: **DuckDB's
ALTER TABLE has sharp edges.** `ADD COLUMN ... NOT NULL DEFAULT
...` is rejected outright. Splitting into ADD + SET NOT NULL is
rejected if any index exists on the table. The Session 7 fix was
to drop NOT NULL entirely and rely on the Rust enum as the
invariant. Session 8's fresh-table migration sidesteps the issue
but the lesson generalizes: when the Rust type system can hold
the invariant, prefer that to a SQL constraint, especially in
DuckDB. Read migration 0005's comment block in full before
writing migration 6's.

Codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.