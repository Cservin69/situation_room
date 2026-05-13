# STOCKPILE — Session 47 handoff

You are starting Session 47. Session 46 shipped pieces A and 2 from
the Session 46 handoff (recipe-success heatmap and expectation-
coverage matrix) bundled in one tarball as `SESSION_46_PATCH_1.md`,
plus a `HostBackoff::snapshot()` drive-by. Build + test green across
the workspace (319/319 pipeline, 61/61 secure, all other crates
green; 12 ignored remain the existing `#[ignore]` live tests). UI
check green.

**Read this file. Read `SESSION_46_PATCH_1.md`. Do not start by
writing an ADR. Do not propose architectural revisions. Start
working.**

The operator's standing principle, re-stated because every prior
session that drifted got it wrong:

> Every fix must be one of:
> - Teaching the LLM what the runtime actually does.
> - Showing the LLM ground-truth bytes/structure.
> - Network-layer truth (UA, timeouts, backoff) with no LLM path.
>
> Anything that smells like "if URL contains X, do Y" or "for
> source S, use endpoint E" is the failure mode.

Session 46 surfaced a new principled question this handoff has to
answer head-on: when one source's prefetch evidence supports
multiple plan expectations (lithium PDF carries production AND
reserves), should the architecture allow N recipes per nomination
or stay at 1? Read the "Architectural piece — multi-recipe per
nomination" section below before picking a scope.

## What works today (post-Session-46)

Carrying forward from Session 45's "What works today" + Session 46's
additions:

- L1 classifier emits descriptions only (Session 38).
- L2 propose-URL retry loop commits to a URL per nomination (Session
  39).
- PDF prefetch frames detected tables + spans the whole document
  via the implicit framed-table list (Session 41 patch 1 + Session
  44 patch 1).
- HTML prefetch produces a structural digest via `scraper` (Session
  41 patch 2).
- JSON prefetch produces a path/type shape outline via `serde_json`
  with polymorphic-leaf annotation (Session 42 patch 3).
- Authoring-time validation runs the runtime extractor against the
  prefetched bytes for every authored recipe (Session 41 patch 1).
- xAI provider config defaults to `grok-4.3` across tiers (Session
  42 patch 4).
- `CompletionRequest::reasoning_effort` threads per-tier
  Low/Medium/High to xAI's chat/completions endpoint (Session 43
  patch 1).
- Default `User-Agent` is the build-time identifier
  `SituationRoom/<version> (+<repo-url>)` (Session 45 patch 1).
- `FetchError::Timeout(Duration)` typed variant; per-host backoff
  layer reads the type, not the message string (Session 45 patch 1).
- `HostBackoff` + `BackoffFetcher` per-host adaptive backoff state,
  recorded on 429 / `Retry-After` / `Timeout`, decayed on success.
  State lives in `AppState` (Session 45 patch 1).
- The Anthropic provider is implemented and chooseable via
  `LLM_PROVIDER=anthropic` + `ANTHROPIC_API_KEY` in `.env`. Default
  remains `xai`.
- **`fetch_run_outcomes` table** (migration 0016) — per-(run,
  recipe-or-source, outcome_kind) rows persisted by the executor at
  run completion. Closed `outcome_kind` enum mirrors
  `RecipeOutcomeDto::kind` exactly. Storage failures here are
  warn-logged and skipped — the user-facing report is preserved
  (Session 46 patch 1).
- **`recipe_outcomes_history(plan_id, run_limit)` IPC** — the
  recipe-success heatmap's primary read. One row per (recipe-or-
  source, source) pair, runs ordered oldest-first within the row.
  `run_limit` clamps the runs (column) dimension; clamped at
  `AppState::MAX_OUTCOMES_HISTORY_RUNS = 50` (Session 46 patch 1).
- **`RecipeOutcomesHeatmap.svelte`** — slotted between `FetchReport`
  and `RecipesPanel`. Cell tone reuses the closed `OutcomeTone` set
  from `$lib/outcomes.ts` so colours match the live outcomes list
  exactly (Session 46 patch 1).
- **`expectation_coverage(plan_id)` IPC** — pure walk over
  `produces[].expectation` references against the plan's four
  binding-addressable buckets. Uncovered expectations surface
  explicitly; orphan bindings (recipes targeting an index the plan
  no longer declares) surface with empty `label` (Session 46 patch
  1).
- **`ExpectationCoverage.svelte`** — matrix slotted in
  `PlanReview.svelte`. Covered rows get a positive border treatment;
  uncovered rows get the dim neutral-attention treatment from
  declined outcomes (Session 46 patch 1).
- **`HostBackoff::snapshot()`** — pure read accessor returning
  `Vec<HostBackoffSnapshot>`. `HostBackoffSnapshotDto` mirrors it on
  the wire. No IPC command yet — drive-by left ready for piece B
  (Session 46 patch 1).

## Session 47 scope — three candidates, pick one

Session 46's deliberate bundling exhausted the "two related pieces
that fit cleanly in one tarball" budget. Session 47 returns to the
handoff discipline: pick one piece, ship it, write the handoff.

### Architectural piece — multi-recipe per nomination

The expectation-coverage matrix surfaced what the live USGS run
made obvious: one nomination → one recipe → one binding → one
expectation. The recipe-author prompt's narrow-coverage discipline
is **intentional** (one scalar per fetch → one binding per scalar →
no padded bindings). The matrix shows which expectations a plan
declares that no recipe covers; the prompt is doing the right
thing within its current contract.

The architectural question the matrix surfaces: **should one
nomination be allowed to author N scalar recipes when the prefetch
evidence supports N distinct expectations?** Today the executor's
authoring loop is 1:1: each nomination yields zero or one recipe
(the propose-URL retry loop commits to one URL, the recipe-author
LLM produces one recipe for that URL, the recipe targets one
expectation index).

The lithium MCS PDF's prefetch carries both production and reserves
tables. A 1:N authoring shape would let the LLM emit two recipes
for the same nomination — same `source_id`, possibly same URL,
different `pdf_table` coordinates and different expectation
references. The prompt's coverage discipline already accepts this
(it forbids *padded bindings off one scalar*; multiple recipes
each with their own scalar are fine).

Why this is the right Session 47 piece:

- **The data backbone supports it.** Recipes are per-version per
  `(plan_id, source_id)`. Today the dedup_key is
  `{plan_id}:{source_id}:{nomination_id}` (or similar — see
  `crates/pipeline/src/recipe_author.rs`). Extending to
  `{plan_id}:{source_id}:{nomination_id}:{expectation_bucket}:{expectation_index}`
  is a one-column-wider key, no schema change.
- **The author prompt's discipline already handles it.** v1.14's
  "Coverage discipline — bindings vs expectations" tells the LLM
  honest narrow coverage > padded coverage. A multi-recipe shape
  asks the LLM to author *one recipe per expectation it can
  honestly cover from the prefetch evidence*; the discipline is the
  same, just iterated across the bucket rather than across
  bindings within one recipe.
- **The expectation-coverage matrix already shows whether it
  worked.** A successful 1:N authoring would land multiple chips on
  the same row's recipe list, or the same `source_id` on multiple
  rows — both shapes the matrix renders today.

Where it lives:

- `crates/pipeline/src/fetch_executor.rs` — the authoring loop in
  `load_or_author_recipes`. Today it calls `author_one(...)` per
  nomination and either persists one recipe or surfaces a Declined
  outcome. The new shape is "iterate over the plan's expectations
  the prefetch evidence could support, call author_one per
  expectation". The bucket boundary is per-nomination (a
  nomination targets a topic-shaped slice of the plan; not every
  expectation makes sense for every nomination).
- `crates/pipeline/src/recipe_author.rs` — the prompt-assembly +
  LLM call. Today the prompt receives the full plan; the LLM
  picks an expectation. The new shape is "the prompt receives the
  plan AND a target expectation reference; the LLM either authors
  a recipe for that expectation or declines for that one". The
  `expectation` field in `produces[]` becomes a constraint, not a
  choice.
- `config/prompts/recipe_author.md` — small principled edit to
  reflect the constraint. **No source-specific text.** The prompt
  goes from "pick the expectation that best fits" to "this is the
  expectation; can the prefetch evidence support it?". Same
  discipline; different framing of the question.
- `crates/storage/src/recipes.rs` — the dedup_key shape may need
  to widen to include the expectation reference so multiple
  recipes per (plan, source) coexist without collision. Check the
  current value before changing — Session 10 picked
  `{plan}:{source}:{metric_name}` for some shapes already.

Why it's safe to build now:

- **No promotion dependency.** Multi-recipe authoring writes more
  recipes; promotion (ADR 0004) consumes records. Orthogonal.
- **No iterator-Phase-2 dependency.** Iterator recipes produce N
  records per fetch from one recipe; multi-recipe-per-nomination
  produces N recipes for N expectations. Different axes.
- **The expectation-coverage matrix is the visible feedback loop.**
  Operator runs fetch → matrix shows which expectations got
  covered → operator can flag any miss with a Session-30
  `RecipeFlagDialog` note that feeds back into the next authoring
  attempt.

Things to watch out for:

- **Don't loop the LLM N times for one nomination by default.**
  Each authoring call costs a few seconds and a few thousand
  tokens; a plan with 4 obs metrics + 3 event types + 2 entity
  kinds is 9 calls per nomination. Bound the per-nomination call
  count (3? 5?) and let the LLM decline gracefully when an
  expectation isn't supportable from the prefetch.
- **Don't bake an expectation-to-source mapping anywhere.** The
  LLM decides, per (nomination, expectation) pair, whether the
  prefetch supports it. No `if source == "pubs.usgs.gov" then
  metric in [production, reserves]` in code or prompt.
- **Watch for orphan bindings in the matrix.** If the
  expectation-bucket-per-recipe gets the bucket index wrong, the
  matrix will surface orphan rows with `label = ""`. That's the
  signal something drifted; treat it as a bug.

### Piece B — per-host backoff status panel (drive-by-ready)

`HostBackoff::snapshot()` and `HostBackoffSnapshotDto` ship in
Session 46 patch 1. Mounting the IPC command and the Svelte panel
is one screen of work.

Where it lives:

- New IPC command: `host_backoff_state()` →
  `Vec<HostBackoffSnapshotDto>`. No parameters; pure read over
  `state.host_backoff.snapshot()`. Lives in
  `crates/api/src/commands.rs` next to `recipe_outcomes_history`.
- New Svelte component: `HostBackoffStatus.svelte`, slotted at the
  top of `PlanReview.svelte` (or in a sibling status strip if the
  visual real estate is tight). Auto-refresh on a 5s polling
  interval — same pattern the FetchReport panel doesn't use today
  (Session 46's heatmap is on-mount + post-fetch only). Polling
  is the right fit here because backoff state changes during a
  fetch run, not in lockstep with the user clicking Run Fetch.
- `apps/desktop/src/lib/api/client.ts` — `hostBackoffState()`
  wrapper. `apps/desktop/src/stores/plans.svelte.ts` — new
  `hostBackoff` state field, `refreshHostBackoff` helper, polling
  setup/teardown around plan selection.

Why it's safe to build now:

- The backoff layer landed in Session 45; the snapshot accessor
  landed in Session 46. State already exists; this just exposes
  it.
- No backend change beyond the IPC command. The state map's
  behavior is unchanged.

Things to watch out for:

- **Don't surface "wait_seconds_remaining" as the only signal.**
  A host with `consecutive_failures = 0, wait_remaining = 0` is
  clean; a host with `consecutive_failures > 0, wait_remaining = 0`
  is *recovering* (the schedule expired but the failure history is
  still in effect for the next signal). Render both.
- **Don't poll when the panel is hidden.** `selectPlan` mounts the
  panel; `clearSelection` should stop the timer. Memory-leak-class
  bug otherwise.

### Piece C — sources-memory panel (carried over)

`Store::sources_memory` already returns the recency-sorted list of
URLs that have succeeded at least once for any plan. The classifier
sees this as `{{SOURCES_MEMORY}}`; the operator doesn't see it at
all. A panel that mirrors what the classifier sees ("here are the
30 URLs your prior plans have learned to fetch from") makes the
classifier's grounding visible.

This was piece C in the Session 46 handoff and didn't ship there;
the rationale and "where it lives" notes from that handoff still
apply.

## Out of scope for Session 47

- **Promotion pipeline (ADR 0004).** Substantial piece. Its own
  handoff. The handoff before that one will need to spend time on
  the authoritative-vs-consensus design choice.
- **Iterator Phase 2 (ADR 0016).** Its own session.
- **Charts on Observations / Events.** Hold until promotion lands.
- **xAI Responses API migration.** Only if a live `grok-4.3` run
  shows chat/completions silently ignoring `reasoning_effort`.
- **Cross-plan recipe-failure aggregation (Session 46 piece D).**
  The expectation-coverage matrix ships per-plan; cross-plan
  aggregation is a different surface. Pick it later.

## Hard rules carried over

Same as Sessions 41–46:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. The piece you pick must not
  add a mode.
- ADR 0009: every HTTP call goes through `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; never hand-edit.
- ts-rs DTOs and pipeline structs are intentionally separate.
  Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`.
- L1 prompt edits come from observed classifications, not
  speculation.
- **Stockpile prompts: principle-only language.** Never bake
  source-specific routing rules. ADR 0007's golden rule applies to
  prompt text as much as code.
- **Do not write code to pass tests.** If a test is obsolete,
  delete it with a comment explaining why and replace it with a
  test that pins the new behaviour.

UI-specific:

- Components compose via Svelte snippets / props; no global state
  outside `apps/desktop/src/stores/plans.svelte.ts`.
- IPC commands return DTOs, not domain types. The `crates/api`
  boundary is where the projection happens.
- Polling intervals are explicit constants in the component, not
  magic numbers in the call site.
- **ts-rs emits Rust `u64` as TS `bigint` since v8.** Coerce to
  `Number(...)` before string interpolation. The
  `RecipeOutcomesHeatmap` already does this for retry-after; check
  any new wire types you introduce.
- **Svelte template narrowing across proxy property accesses can
  be lost.** When a store property is `T | null`, pull it through
  `$derived(plans.X)` once and narrow on the local; svelte-check
  flags template-level re-accesses otherwise. The
  `ExpectationCoverage` component documents the pattern.

## Things you will be tempted to do that are wrong

Same as Sessions 41–46, plus new ones surfaced by Session 46:

- **Edit the recipe-author prompt to "cover all expectations
  exhaustively."** Don't. v1.14's "narrow honest coverage > padded
  coverage" rule is intentional and load-bearing. The right move
  if coverage feels thin is the multi-recipe-per-nomination
  architectural piece, not a prompt edit.
- **Backfill `fetch_run_outcomes` from old `fetch_runs` rows.**
  Don't. Pre-Session-46 runs have no per-outcome detail; the
  heatmap renders sparse columns for them honestly. Synthesising
  rows from the summary counters would fabricate data.
- **Make `recipe_fetch_attempts` write success rows too.** Don't.
  That table's contract is bytes-and-failure for the re-author
  flow (ADR 0012 amendment 1). Session 46's `fetch_run_outcomes`
  is the per-(run, outcome) row table; no need to widen the older
  one.
- **Add a "retry SEC with a different UA" knob.** Source-specific
  routing in disguise. The Session 45 default UA is the answer for
  every host; SEC's HTML-site 403 is a URL-family problem, not a
  UA problem.
- **Configure `[per_host."<hostname>"]` timeout overrides.** No.
  Parameters are uniform; runtime adapts on observed signals.
- **Edit the propose-URL prompt to "stop suggesting SEC search
  pages."** Source-specific routing in prompt text. The
  expectation-coverage matrix and the heatmap make those failures
  visible; the prompt's principle teaches the LLM about *fetchable
  endpoint shapes*.
- **Rebuild `RecipesPanel` instead of slotting alongside it.**
  The existing components are good; add new components, don't
  rewrite. Each new piece should be its own component file in
  `apps/desktop/src/components/`.
- **Add a chart library.** The recommended pieces don't need one.
  Charts land post-promotion.
- **Try to surface "live progress" of an in-flight fetch.** The
  current architecture is one synchronous `run_fetch_for_plan`
  call per click. Per-host backoff state panel (piece B) is the
  closest principle-clean way to make in-flight state visible —
  read state asynchronously while a synchronous fetch is running.
- **Bundle the multi-recipe architectural piece with anything
  else.** Don't. It touches the executor's authoring loop, the
  recipe-author prompt, and the recipe storage's dedup_key shape;
  it earns a session by itself.
- **Write a "Session 47 plan" document.** This file is the plan.
  Read it once, then code.
- **Ship a tarball / commit that "doesn't compile but is the right
  shape."** Each commit must compile.

## Files to read first

In order. Stop when you have enough to make the fix.

1. This file.
2. `SESSION_46_PATCH_1.md` (and `SESSION_45_PATCH_1.md` for
   network-layer context if piece B is the chosen scope).
3. The piece-specific files. For the multi-recipe piece
   (RECOMMENDED):
   - `crates/pipeline/src/fetch_executor.rs` — the authoring loop
     in `load_or_author_recipes` and `author_one`. The current
     1:1 shape is the constraint to relax.
   - `crates/pipeline/src/recipe_author.rs` — prompt assembly +
     LLM call. The expectation reference flows through here.
   - `config/prompts/recipe_author.md` — v1.14's coverage
     discipline + the `produces[].expectation` schema. The prompt
     edit is principled, not source-specific.
   - `crates/storage/src/recipes.rs` — dedup_key shape; check
     whether widening is necessary or the existing key already
     handles per-expectation rows.
   - `crates/api/src/commands.rs::build_expectation_coverage` —
     the matrix walker. It already groups recipes by `(bucket,
     index)`; multi-recipe-per-nomination should land naturally
     under the same shape.
4. For piece B:
   - `crates/pipeline/src/fetch_backoff.rs::snapshot` — the
     accessor Session 46 added; the IPC command lifts its return
     into `Vec<HostBackoffSnapshotDto>`.
   - `crates/api/src/commands.rs::recipe_outcomes_history` — the
     pattern to follow for the new IPC command. Pure read, no
     LLM, clamped limit.
   - `apps/desktop/src/components/RecipeOutcomesHeatmap.svelte` —
     the slot pattern + tone-reuse-from-`outcomes.ts` pattern.
5. For piece C:
   - `crates/storage/src/sources_memory.rs` — the existing query
     and DTO surface (check whether `MemorySourceDto` already
     exists; if not, add the ts-rs mirror).

## Live-run observations from end-of-Session-46

No new live run since Session 45's lithium plan. The Session 45
observations still stand:

- 5 of 7 nominations declined; varied reasons (SEC 403, World Bank
  404, IEA 404, industry.gov.au timeout, Fastmarkets paywall,
  Reuters 401).
- USGS PDF and IEA EV Outlook were the two that authored recipes.
  The USGS recipe targets `observation_metric[0]` (production)
  only despite the plan declaring 4 obs metrics — the multi-
  recipe-per-nomination piece's motivating case.
- The expectation-coverage matrix from Session 46 now makes the
  1-of-4 coverage visible at a glance.
- The recipe-success heatmap from Session 46 will fill in as more
  runs accumulate against the same plan.

These observations are the ground truth Session 47's chosen piece
should be evaluated against post-ship. Run a fetch on the same
lithium plan after the patch lands; the matrix and heatmap should
both reflect the change.

## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git remote
they want the agent involved with, paranoid about security,
prefers honesty about uncertainty over false confidence.

**Workflow.** Direct in-place editing in the workspace folder
(`~/Documents/Claude/Projects/SituationRoom/`). Operator runs cargo
on their Mac with output teed into the repo root:

```
cd ~/Documents/Claude/Projects/SituationRoom && \
  (cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log && \
  (cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

For UI-touching pieces, also run the desktop check (after `npm
install` if `node_modules` is fresh):

```
cd ~/Documents/Claude/Projects/SituationRoom/apps/desktop && \
  npm run check 2>&1 | tee ../../ui-check.log
```

(Operator's existing rsync block excludes `.idea/` and `.env`. The
block is theirs; don't re-print it.)

The agent reads `build.log`, `test.log`, and `ui-check.log`
directly; the sentinel `EXIT=0` lets the agent tell "done and
green" from "still streaming." Sandbox bash cannot reach `crates.io`
or `sh.rustup.rs` — there is no way to run `cargo` from inside the
agent's container, and that's fine because the operator's Mac is
the source of truth anyway.

After patch + green logs, agent says "rsync" or "ship it"; operator
pastes the saved rsync block (with the `.env` exclusion) to mirror
the workspace folder to `~/RustRoverProjects/situation_room/` for
git/remote management.

Operator approves with terse signals — "go", "continue", a log
dump. Reciprocate. Don't pad responses with status preamble or
summary postamble; lead with the actual move. Resume mid-stream on
"continue", don't restart.

When operator pushes back, listen. They have caught architectural
drift more than once across these sessions and have been right
every time. The most important push-back to internalize: **the LLM
is the only specialist; do not hand-code commodity adapters or
source-specific routing.** Sessions 38–46 honor this rule by giving
the LLM better evidence (PDF framed tables, HTML scraper digest,
JSON shape outline, whole-document PDF coverage), reacting at the
network layer (default UA, typed timeouts, per-host backoff)
without source-specific knowledge, and surfacing state legibly
(recipe-success heatmap, expectation coverage matrix) — never
encoding source-specific knowledge anywhere.

After Session 47 ships its chosen piece, the runway opens for
promotion (ADR 0004) — the largest remaining backbone piece. That
session's handoff should spend time on the authoritative-vs-
consensus design choice before committing to a shape.

End of handoff.
