# Stockpile Session 9 Handoff

You are picking up after Session 8. Read this and ADR 0011 first;
both are authoritative.

## What Session 8 shipped

- Migration v6: `fetch_runs(id, plan_id, started_at, finished_at,
  recipes_attempted, recipes_succeeded, records_produced,
  error_summary)`. The migration follows the v5 comment-block
  discipline — fresh table, no ALTER trap, NOT NULL on counters
  with `DEFAULT 0` is safe inside CREATE TABLE.
- `Store::insert_fetch_run`, `update_fetch_run`,
  `recent_fetch_runs_for_plan` — typed `FetchRunRow` /
  `StoredFetchRun`. Tested against in-memory store.
- `Store::recipes_for_plan(plan_id) -> Vec<StoredRecipe>` for the
  executor's load step.
- `pipeline::http_fetcher::HttpFetcher` trait + `SecureHttpClient`
  impl. Tests use `StaticFetcher` (URL→bytes map) — production
  passes the secure client.
- `pipeline::fetch_executor::run_fetch_for_plan`:
  - Status-validates the plan (`PlanNotAccepted` if not
    `accepted`).
  - Loads recipes; if empty, runs Level-2 authoring once per
    bound source via `recipe_author::author_recipe`, stamps
    `source_id` + `dedup_key`, persists.
  - Iterates recipes. CSV → fetch + apply + insert. Other modes
    → `Skipped { reason }`.
  - Per-recipe failures don't abort the run; a `Failed { stage,
    message }` outcome rides on the `FetchReport`.
  - Opens a `fetch_runs` row at start, closes it at end with
    counters; wholesale failures close with `error_summary`.
- API: `FetchReportDto`, `RecipeOutcomeDto`, `FailureStageDto`,
  `FetchRunSummaryDto`, plus the corresponding TS files.
  `CommandError::FetchFailed { recipes_attempted, recipes_succeeded,
  message }` and shadow DTO. Two new commands: `run_fetch_for_plan`,
  `list_fetch_runs`.
- Frontend: `RunFetchButton.svelte` (only visible when plan is
  `accepted`), `FetchReport.svelte` (renders `plans.fetchReport`
  + `plans.fetchRuns`), slotted into `PlanReview.svelte` after the
  bucket grid. Store gained `fetching`, `fetchReport`, `fetchRuns`
  state and `runFetch`, `refreshFetchRuns` helpers. `selectPlan`
  resets per-plan fetch state and refreshes the runs strip.
- `desktop/src-tauri/src/main.rs`: loads `recipe_author.md` via
  `include_str!`, threads `Arc<SecureHttpClient>` and the prompt
  into `AppState::new`, registers both new commands.
- New source descriptor `csv_demo` in `config/sources.toml` — a
  legible target for the demo / live test.
- Live ignored test in `fetch_executor.rs`: real network,
  pre-authored recipe, `UnreachableProvider`. Drives a real CSV
  through the fetch path. Override defaults via
  `FETCH_LIVE_CSV_URL`, `FETCH_LIVE_CSV_COLUMN`,
  `FETCH_LIVE_CSV_FILTER_COL`, `FETCH_LIVE_CSV_FILTER_VAL`.
- ADR 0011 written to retroactively codify the lifecycle and the
  LLM-free-runtime invariant.

## The LLM-free runtime invariant — please don't break this

ADR 0007 + ADR 0011: Level 1 (classifier) is one LLM call. Level 2
(recipe author) is one LLM call per `(plan, bound source)`,
*conditional* on the plan having no recipes yet. After that, runs
are deterministic and cheap. The offline tests use
`UnreachableProvider` which **panics** on any `complete` call —
that's the invariant's enforcement. If your test wants the LLM
mocked, mock the recipe-author surface, not the executor's
provider plumbing.

## Verifying the build

I could not run `cargo` in my sandbox. The patch is mechanically
checked but not compiler-checked. Before extending it:

1. `cargo check --workspace` — catch any signature drift I missed.
2. `cargo test --package stockpile-storage` — migration v6 + the
   new `fetch_runs` tests.
3. `cargo test --package stockpile-pipeline` — five offline
   `fetch_executor` tests + the unchanged recipe_apply / author
   suites.
4. `cargo test --package stockpile-api` — DTO round-trips +
   regenerates the TS type files. **Compare against the hand-
   written TS files I shipped**; if ts-rs disagrees with my
   shape, ts-rs is right and I made a mistake — adjust the DTO.
5. `cargo test --package stockpile-pipeline -- --ignored` (with
   network + a stable CSV URL) — the live test.

If any step fails, the offending file is one of:
`crates/storage/src/{lib.rs, migrate.rs, fetch_runs.rs, recipes.rs}`,
`crates/pipeline/src/{lib.rs, http_fetcher.rs, fetch_executor.rs,
recipes_store.rs}`,
`crates/api/src/{commands.rs, types_export.rs}`,
`apps/desktop/src-tauri/src/main.rs`,
`apps/desktop/src/lib/api/{client.ts, types/*.ts}`,
`apps/desktop/src/stores/plans.svelte.ts`,
`apps/desktop/src/components/{PlanReview.svelte,
RunFetchButton.svelte, FetchReport.svelte}`.

## Session 9 priority — pick one

The fetch path now produces records end-to-end for one extraction
mode. The next direction depends on what hurts most when you
exercise the path on a real source:

### Option A — Promote one more extraction mode (likely JSON)

Most APIs return JSON. The recipe_apply runtime already supports
`JsonPath`; the executor needs to switch the `Skipped` arm for
`ExtractionSpec::JsonPath` to a real fetch + apply path mirroring
the CSV one. Probably one afternoon.

### Option B — Coverage report

ADR 0007 specifies a `CoverageReport` describing which expectations
were filled and which weren't. The plumbing exists in `research.rs`;
the executor needs to compute one alongside the `FetchReport` and
return it. The UI's bucket panels grow a "filled / pending / gap"
indicator per expectation.

### Option C — Re-author on failure

Today, a `Failed { stage: Apply }` outcome means the source's shape
drifted under the recipe and the user is stuck. A re-authoring flow
that re-runs Level-2 with the new excerpt and offers to swap the
recipe is the obvious next step — but it needs a policy decision
(when do we offer? do we apply automatically? what about cost?).
ADR 0011 explicitly defers this; if you take it on, write ADR 0012.

### Option D — Per-recipe outcome persistence

If the synchronous report is enough for the UI, skip this. If the
user has been asking "what failed last week?" and you want a query
that doesn't depend on the report still being in memory, design
the `recipe_outcomes` schema and the queries that consume it.

I'd recommend **A** — it's the smallest patch with the largest
payoff for variety of sources you can actually fetch from. It also
exercises the executor's dispatch surface in a way that catches
regressions in the CSV path without a big surface area increase.

## Hard rules (carry-over)

- LLM-free at runtime. Period. Don't add provider calls inside
  `run_one_recipe` or below.
- DuckDB ALTER trap: never `ADD COLUMN ... NOT NULL DEFAULT ...` in
  one statement, and never use the split form on a table with
  indexes. Fresh tables in `CREATE TABLE` are fine — read v5's
  comment block.
- Patch packaging: tar.gz extracted at `~/Downloads/`, copied to
  `/Users/aben/RustroverProjects/stockpile/`. **No `#`-prefixed
  hashtag comments in copyable shell commands** — they break zsh.
- One `SecureHttpClient`. Never `reqwest::Client::new()`.
- Quotes from web search ≤ 15 words and ≤ 1 per source — N/A here
  but flagged because Anthropic's instructions emphasized it; not
  relevant to coding tasks.

## What Session 8 is explicitly NOT

- Re-authoring on failure (Option C above; deferred, ADR 0011).
- Per-recipe persistence (Option D; deferred).
- Coverage report (Option B; deferred).
- Background scheduling. One plan, one call, user-initiated.
- A complete UI for fetch-run history beyond the timeline strip
  on the review pane.

## Things I'd flag for review

1. **The synthetic sample URL.** `author_one` builds a placeholder
   `https://example.invalid/{source_id}` for the AuthoringContext.
   The real fix is to fetch a small excerpt of the source's
   content first and pass that. I deliberately narrowed scope
   here; the demo binary in Session 3 had a richer pre-fetch
   step that should be revived in Session 9 or 10.
2. **`error_summary` truncation.** The column is unbounded TEXT;
   a pathological provider error could be huge. Consider clamping
   to a few KB at write time.
3. **The `FetchFailed` arm carries `recipes_attempted` /
   `recipes_succeeded`** even though both are always zero on the
   wholesale-failure path. The shape is forward-compatible — the
   wholesale-failure-after-some-recipes-ran case (e.g. a panic
   mid-run that doesn't reach the close path) could populate
   them — but today they're always 0. Worth a re-look.

## Continuity note

Session 8 came in with an eye on a specific live CSV demo. That
worked: the offline path is solid, the wiring is honest, the live
test is structural rather than asserting on values. Session 9
should resist the temptation to broaden ambition — pick one of
A–D, ship it, write the next ADR if it warrants one.
