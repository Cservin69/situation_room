# STOCKPILE — Session 46 handoff

You are starting Session 46. Session 45 shipped piece D from the
Session 43 handoff (network-layer issues: default UA, Reuters,
per-host backoff) as patch 1 (`SESSION_45_PATCH_1.md`). Single
tarball; no schema or migration; build + test green across the
workspace (315/315 pipeline, 61/61 secure, all other crates green;
12 ignored are the existing `#[ignore]` live integration tests).

**Read this file. Read `SESSION_45_PATCH_1.md`. Do not start by
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

Session 46 is the **first UI-shaped piece in many sessions**, but
the principle still holds: introspection surfaces over data the
runtime already produces, never per-source rendering branches.

## What works today (post-Session-45)

Carrying forward from Session 44's "What works today" + the Session
45 additions:

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
- **Default `User-Agent`** is the build-time identifier
  `SituationRoom/<version> (+<repo-url>)`, sourced from
  `CARGO_PKG_VERSION` and `CARGO_PKG_REPOSITORY` — applies to every
  request, no SEC-specific bake-in (Session 45 patch 1, D-1).
- **`FetchError::Timeout(Duration)`** is a typed variant; the
  per-host backoff layer reads the type, not the message string
  (Session 45 patch 1, D-3).
- **`HostBackoff` + `BackoffFetcher`** — per-host adaptive backoff
  state (keyed at runtime on the URL host, parameters uniform across
  hosts) recorded on 429 / `Retry-After` / `Timeout`, decayed on
  success. State lives in `AppState`, applied transparently around
  `state.http` at `run_fetch_for_plan` time (Session 45 patch 1,
  D-3).
- The Anthropic provider is implemented and chooseable via
  `LLM_PROVIDER=anthropic` + `ANTHROPIC_API_KEY` in `.env`. Default
  remains `xai`. Both providers stay in the codebase; the operator
  flips per deployment.

## Session 46 scope — operator-introspection UI

The data backbone (ADR 0004 promotion pathway in
`crates/pipeline/src/promote.rs`, ADR 0016 Phase 2 iterator runtime
expansion) hasn't landed. Until it does, **metric/event charts on
Observations/Events would render anemic** (single-point series per
fetch, no multi-source consensus) **and risk UI churn** when the
promoted-record shape supersedes the raw-record shape. Hold the
chart work; build operator-introspection first.

The post-Session-45 live run surfaced the exact gap this scope
fills: 5 of 7 nominations declined, varied reasons (SEC 403, World
Bank URL not found, IEA URL miss, industry.gov.au timeout). The
operator currently has no in-UI way to see *why* each declined
without reading server logs. The recipe-author and propose-URL
prompts can't be tuned from log scraping. Surfaces that make
those failures legible are this session.

### Pick the highest-leverage piece from this list

These are mutually independent. **Pick one.** Each is its own
session; do not bundle more than one into a single tarball / commit
pair. The architectural risk is small — every piece reads existing
DTOs over the existing IPC surface — but the test surface is wider
because frontend tests + backend tests for new commands need to
come along.

#### A. Fetch-runs-over-time strip per plan (RECOMMENDED first)

The data exists in `recipe_fetch_attempts` (Migration 13). Today
the fetch-report panel shows the latest run's outcomes; the prior
runs sit in storage but aren't visualised. A horizontal strip
("recipe-success heatmap") with one column per fetch run, one row
per recipe, cells colored by outcome (success / declined /
rate-limited / timeout / failed) would make "is this source flaky
or did it just fail once?" answerable at a glance.

Where it lives:
- New IPC command: `recipe_outcomes_history(plan_id, limit)` →
  `Vec<(recipe_id, source_id, runs: Vec<(run_id, attempted_at,
  outcome_kind)>)>`. The DTO is small; closed-vocabulary outcome
  kind (Session 22's `RecipeOutcomeDto` already enumerates them —
  use that enum, don't invent a parallel).
- New Svelte component slots between `RecipesPanel` and
  `FetchReport` in `PlanReview.svelte`.
- The `recipe_fetch_attempts` table already has every column needed
  (recipe_id, attempted_at, succeeded, failure_message,
  response_content_type). The query is one SQL with a JOIN to
  `recipes` for source_id, ordered by attempted_at. No migration.

Why it's safe to build now:
- No promotion dependency. Fetch attempts are runtime artifacts;
  promotion doesn't touch them.
- No iterator-Phase-2 dependency. Iterator recipes write *more*
  attempts per recipe-run, not different shapes.
- DTOs stable since Session 22.

#### B. Per-host backoff state surface

Session 45's `HostBackoff` is in-memory and introspectable but has
no UI. When a fetch is delayed by pre-flight backoff the operator
sees only the eventual outcome, not "we waited 8s on this host
before issuing the request." A small status panel at the top of
`PlanReview` showing the top-N hosts with active backoff
(`consecutive_failures > 0`, `next_allowed_at > now`) lets the
operator see *why* a fetch is slow before it completes.

Where it lives:
- New IPC command: `host_backoff_state()` → `Vec<HostBackoffEntry
  { host, consecutive_failures, wait_seconds_remaining,
  last_signal_kind: "rate_limited" | "timeout" }>`.
- `HostBackoff` needs one new public method (`snapshot()`) that
  returns the entries; today only `pre_flight_wait` and
  `consecutive_failures` are exposed and there's no enumeration
  surface. Add it; it's a pure read over the existing `Mutex<HashMap>`.
- A small Svelte component above the topbar or as a compact strip.
  Auto-refresh on a polling interval (5s) — no event push needed.

Why it's safe to build now:
- The backoff layer landed in Session 45. The state already exists;
  this just exposes it.
- No backend change beyond the `snapshot()` accessor and the IPC
  command. The state map's behavior is unchanged.

#### C. Sources-memory panel

`Store::sources_memory` already returns the recency-sorted list of
URLs that have succeeded at least once for any plan. The classifier
sees this as `{{SOURCES_MEMORY}}` substitution; the operator
doesn't see it at all. A panel that mirrors what the classifier
sees ("here are the 30 URLs your prior plans have learned to fetch
from") makes the classifier's grounding visible.

Where it lives:
- The IPC command for this exists already? If not, add
  `sources_memory(limit)` → `Vec<MemorySourceDto>` (mirror of
  `MemorySource` from `crates/storage/src/sources_memory.rs`).
- Standalone view, not slotted into `PlanReview` — this is
  cross-plan context, not per-plan. Could live as a left-pane
  bottom section under `RecentPlansList`, or as a separate route
  (the SPA is single-route today; adding a second route is small).

Why it's safe to build now:
- Pure read over the storage view that already powers the
  classifier prompt.
- No promotion dependency; sources_memory only surfaces
  *successful* sources by definition.

#### D. Cross-plan recipe-failure aggregation

A pure derived view over `recipe_fetch_attempts ⨝ recipes`: which
sources have the highest failure rate? Which extraction modes fail
most? Useful for "is World Bank just unreliable?" or "is the JSON
extractor underperforming the CSV extractor?"

Where it lives:
- New IPC command: `failure_aggregates()` → list of (source_id,
  attempts, failures, top_failure_message). One SQL query.
- View rendered as a sortable table — no charting, just counts
  and percentages. Could share the new sources-memory route or
  live below it.

Why it's safe to build now:
- Pure derived view; no schema change, no promotion dependency.
- The data is already there from every fetch run.

### Trivial drive-bys (optional with the chosen piece)

- **`HostBackoff::snapshot()` accessor** — needed by piece B
  anyway; if the chosen piece is A, C, or D, adding `snapshot()`
  as a drive-by makes piece B trivially landable next session.
- **`MemorySourceDto`** — if it doesn't exist yet, add the ts-rs
  mirror of `MemorySource`. Same story: trivial, unblocks piece C.

### Out of scope for Session 46

- **Charts on Observations / Events.** Hold until promotion
  (`crates/pipeline/src/promote.rs` Phase 3) lands. Without it the
  series is single-point-per-fetch; with it the consensus-promoted
  Observation is the right thing to chart.
- **Iterator Phase 2 (ADR 0016).** Its own session; named
  explicitly as such in Session 45's "next sessions" rundown.
- **Promotion pipeline (ADR 0004).** Its own session; a substantial
  piece. The handoff before that one will need to spend time on the
  authoritative-vs-consensus design choice.
- **xAI Responses API migration.** Only if a live `grok-4.3` run
  shows chat/completions silently ignoring the `reasoning_effort`
  parameter Session 43 plumbed.
- **LLM provider switching.** Already wired —
  `LLM_PROVIDER=anthropic` + `ANTHROPIC_API_KEY` in `.env` flips
  to Claude; default stays `xai`. No code change needed unless a
  new failure mode surfaces with Claude.
- **SEC 403 deeper fix.** The Session 45 default UA targets
  `data.sec.gov`'s well-known UA-required behavior; the live-run
  403s are on `www.sec.gov/edgar/...` (the HTML site / SPA), which
  is a different rejection path (likely IP rate limit or bot
  detection on the website). Not a Session 45 regression. The right
  fix lives in the propose-URL prompt's guidance about which SEC
  endpoint family is fetchable — but **prompt edits to the
  propose-URL prompt are a separate piece**, do not bundle here.

## Hard rules carried over

Same as Sessions 41–45:

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

UI-specific (mostly already in the codebase, restated for clarity):

- Components compose via Svelte snippets / props; no global state
  outside `apps/desktop/src/stores/plans.svelte.ts`. New cross-cutting
  state belongs there or in a sibling runes store.
- IPC commands return DTOs, not domain types. The `crates/api`
  boundary is where the projection happens.
- Polling intervals are explicit constants in the component, not
  magic numbers in the call site.

## Things you will be tempted to do that are wrong

Same as Sessions 41–45, plus new ones surfaced by the live run:

- **Build a metric chart on Observations using whatever data
  exists.** Don't. Single-point series make the chart misleading,
  and the shape will change when promotion lands. Hold.
- **Add a "retry SEC with a different UA" knob.** Source-specific
  routing in disguise. The Session 45 default UA is the answer for
  every host; SEC's HTML-site 403 is a URL-family problem, not a
  UA problem.
- **Configure `[per_host."industry.gov.au"]` timeout overrides
  because the live run showed 300s timeouts.** No. Same principle
  as Session 45: parameters are uniform; runtime adapts on observed
  signals. If the operator wants faster failure on bad hosts,
  lower `SecureHttpConfig::total_timeout` globally.
- **Edit the propose-URL prompt to "stop suggesting SEC search
  pages."** That's source-specific routing in prompt text. The
  prompt's principle teaches the LLM about *fetchable endpoint
  shapes* — the operator-introspection UI should make those
  failures visible, not the prompt should embed source-specific
  rules.
- **Rebuild `RecipesPanel` instead of slotting alongside it.**
  The existing components are good; add new components, don't
  rewrite. Each new piece should be its own component file in
  `apps/desktop/src/components/`.
- **Add a chart library.** None of the recommended pieces (A, B,
  C, D) need a chart library — they're tables, strips, or
  cells-with-colors. When charts land (post-promotion) the choice
  should be deliberate; today's UI uses zero chart deps and
  shouldn't grow one for these surfaces.
- **Try to surface "live progress" of an in-flight fetch.** The
  current architecture is one synchronous `run_fetch_for_plan`
  call per click; mid-call streaming would need an event channel
  or polling shape. Not Session 46's scope. Per-host backoff state
  panel (B) is the closest principle-clean way to make in-flight
  state visible — read state asynchronously while a synchronous
  fetch is running.
- **Bundle the LLM-provider Anthropic flip with a UI piece.**
  Don't. The Anthropic provider is already wired; the env-var
  flip is a deployment change. If a Claude-specific failure mode
  surfaces, *that* is its own session.
- **Write a "Session 46 plan" document.** This file is the plan.
  Read it once, then code.
- **Ship a tarball / commit that "doesn't compile but is the right
  shape."** Each commit must compile.

## Files to read first

In order. Stop when you have enough to make the fix.

1. This file.
2. `SESSION_45_PATCH_1.md` (and `SESSION_44_PATCH_1.md` for the
   prior session's prefetch-format context — useful when a UI
   piece's data shape touches recipe-author behaviour).
3. The piece-specific files. For piece A (recommended):
   - `crates/storage/src/recipe_fetch_attempts.rs` — the table the
     query reads.
   - `crates/api/src/commands.rs` — pattern to follow for adding a
     new IPC command (look at `list_fetch_runs` as the closest
     existing analogue).
   - `crates/api/src/types_export.rs` — where the new DTO lands.
   - `apps/desktop/src/components/FetchReport.svelte` — the
     existing fetch-runs panel; the new strip should compose
     visually with this.
   - `apps/desktop/src/lib/api/client.ts` — TS wrapper for the
     new IPC command.
4. For piece B:
   - `crates/pipeline/src/fetch_backoff.rs` — Session 45 patch 1's
     `HostBackoff`. Add `snapshot()` here.
   - `crates/api/src/commands.rs` — `run_fetch_for_plan` shows
     how `state.host_backoff` is reached.
5. For piece C:
   - `crates/storage/src/sources_memory.rs` — the existing query.
     Check whether an IPC command already exists.
6. For piece D:
   - `crates/storage/src/recipe_fetch_attempts.rs` (same as piece
     A) — the rows you'll be aggregating.

## Live-run observations from end-of-Session-45

A real fetch run on a "lithium global supply chain" plan
(2026-05-09) surfaced:

- **5 of 7 nominations declined.** USGS PDF and IEA EV Outlook got
  recipes; SEC, World Bank, IEA Critical Minerals, and
  industry.gov.au all declined after exhausting the propose-URL
  retry budget.
- **SEC 403** on `www.sec.gov/edgar/search/` and
  `www.sec.gov/edgar/browse/?CIK=915912&owner=exclude` despite the
  Session 45 default UA. These are HTML/SPA URLs, not the
  UA-gated `data.sec.gov` JSON API. The 403 is likely IP rate
  limiting or bot detection on the website. **The fix is in the
  propose-URL prompt's URL-family guidance, not in the network
  layer** — and prompt edits are out of scope for Session 46.
- **`industry.gov.au` 300s timeout reported.** The per-host
  backoff layer (Session 45) recorded the timeout and pushed
  `next_allowed_at` out by 1s. The next attempt 13s later
  proceeded without observable wait — correct behavior, but the
  short schedule means the layer can't prevent the *first* timeout
  per session. If the operator wants faster failure on
  pathological hosts, lower the global `total_timeout` (currently
  300s). Not a Session 46 piece on its own.
- **The classify → accept → run cycle works end-to-end.** The
  failures are LLM-quality / source-picking issues, not pipeline
  bugs. The operator-introspection surfaces this session targets
  are exactly what would let the operator diagnose them in-UI
  rather than through log scraping.

These observations are what justifies the operator-introspection
scope: every failure above is data the system already has but the
UI doesn't show.

## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git remote
(or one they manage outside the agent loop), paranoid about security,
prefers honesty about uncertainty over false confidence.

**Workflow.** Direct in-place editing in the workspace folder
(`~/Documents/Claude/Projects/SituationRoom/`). Operator runs cargo
on their Mac with output teed into the repo root:

```
cd ~/Documents/Claude/Projects/SituationRoom && \
  (cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log && \
  (cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

For UI-touching pieces, also run the desktop build to catch
TypeScript / Svelte breakage:

```
cd ~/Documents/Claude/Projects/SituationRoom/apps/desktop && \
  npm run check 2>&1 | tee ui-check.log
```

(Operator's existing rsync block excludes `.idea/` and **as of
Session 46** also excludes `.env` so the testing-repo's `.env`
isn't deleted by `--delete`. The block is theirs; don't re-print
it.)

The agent reads `build.log`, `test.log`, and `ui-check.log`
directly; the sentinel `EXIT=0` lets the agent tell "done and
green" from "still streaming." Sandbox bash cannot reach `crates.io`
or `sh.rustup.rs` — there is no way to run `cargo` from inside the
agent's container, and that's fine because the operator's Mac is
the source of truth anyway.

After patch + green logs, agent says "rsync" or "ship it"; operator
pastes the saved rsync block (now with the `.env` exclusion) to
mirror the workspace folder to `~/RustRoverProjects/situation_room/`
for git/remote management.

Operator approves with terse signals — "go", "continue", a log
dump. Reciprocate. Don't pad responses with status preamble or
summary postamble; lead with the actual move. Resume mid-stream on
"continue", don't restart.

When operator pushes back, listen. They have caught architectural
drift more than once across these sessions and have been right
every time. The most important push-back to internalize: **the LLM
is the only specialist; do not hand-code commodity adapters or
source-specific routing.** Sessions 38–45 honor this rule by giving
the LLM better evidence (PDF framed tables, HTML scraper digest,
JSON shape outline, whole-document PDF coverage) and reacting at
the network layer (default UA, typed timeouts, per-host backoff)
without ever encoding source-specific knowledge anywhere.

After Session 46 ships its chosen operator-introspection piece, the
runway opens for the next architectural piece. The largest
remaining backbone work — promotion (ADR 0004) and iterator Phase 2
(ADR 0016) — should land before the chart-shaped UI pieces this
session deliberately defers.

End of handoff.
