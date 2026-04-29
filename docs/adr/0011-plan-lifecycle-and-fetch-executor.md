# ADR 0011: Plan lifecycle and the fetch executor

## Status

Accepted. Session 7 introduced the lifecycle states and the
accept/reject commands; Session 8 wired the runtime path that turns
an accepted plan into stored records. This ADR retroactively names
both decisions in one place so future sessions don't have to
reconstruct the rationale by reading two handoffs.

## Context

ADR 0007 split the research function into two LLM-touching levels
(classifier, recipe-author) and a deterministic runtime. Session 6
shipped Level 1; Session 7 shipped the human-in-the-loop boundary
between Level 1 and the runtime — the accept/reject decision that
gates fetching. Session 8 needed to:

- Decide what data the runtime records about each invocation.
- Decide where Level-2 authoring runs in the runtime path.
- Decide what `accepted` actually permits, and what `pending` and
  `rejected` actually prevent.

These three decisions interact. The accept gate exists because
fetching is the first action with externally-observable consequences
(network requests, recipe authoring spend). The runtime path needs
to be cheap on re-runs, which means recipes get authored once and
applied many times — the LLM-free invariant from ADR 0007. And the
audit trail needs to show which plans were fetched, when, and how
they fared, which means a per-run record distinct from the plan.

## Decision

### Lifecycle

A `ResearchPlan` has exactly three states: `pending`, `accepted`,
`rejected`. New plans are `pending`. The user accepts or rejects in
the review UI. Both transitions are durable; neither is reversible
through the UI. Re-classifying the same topic produces a fresh plan
with a fresh id.

The status is stored on the `research_plans` row, populated by
migration v5 (added 2026-04 in Session 7). The default is `pending`
to keep the migration sympathetic to the DuckDB ALTER trap (see
the comment block in `migrations/0005_research_plan_status.sql`).

### What `accepted` permits

The fetch executor refuses to run against any plan not in `accepted`
status. The error is `PlanNotAccepted { current }` carrying the
current status; the api layer maps it to `InvalidInput` with a
message naming the source of the problem (handoff §"Why InvalidInput
reads odd here").

`pending` plans are visible for review only. `rejected` plans are
soft-deleted: the row stays for audit but they're filtered from the
default listing.

### Fetch executor

One synchronous entry point: `pipeline::fetch_executor::run_fetch_for_plan`.
Given a plan id, it:

1. Loads and status-validates the plan.
2. Loads recipes for the plan via `Store::recipes_for_plan`.
3. If empty, runs Level-2 authoring once per plan-bound source
   (`pipeline::recipe_author::author_recipe`) and persists each
   resulting recipe via `recipes_store::save_recipe`. The
   `source_id` and `dedup_key` (`{plan_id}:{source_id}`) are stamped
   by the executor — `build_validated_recipe` leaves them blank for
   the caller per ADR 0007.
4. For each recipe, dispatches on the extraction mode. CSV is the
   only mode wired through to fetch+apply+insert in Session 8;
   other modes report as `Skipped { reason }`. The skipping is
   deliberate phasing, not a failure mode — Session 9+ promotes
   each mode as it comes online.
5. Returns a `FetchReport` summarising what happened, with one
   `RecipeOutcome` per recipe (`Succeeded`, `Skipped`, or
   `Failed { stage, message }`).

Per-recipe failures don't abort the run. A run that produces no
records but doesn't error wholesale closes its `fetch_runs` row
normally — the report tells the user what happened.

### `fetch_runs` table

Migration v6 adds `fetch_runs(id, plan_id, started_at, finished_at,
recipes_attempted, recipes_succeeded, records_produced,
error_summary)`. One row per executor invocation. The row opens
with `finished_at = NULL` and counters at zero; the executor
updates the row to close it when work completes. A wholesale failure
(plan vanished, plan not accepted, authoring blew up entirely)
closes the row with an `error_summary` populated.

Per-recipe outcome detail rides on the synchronously-returned
`FetchReportDto` and is **not** persisted at this granularity in
Session 8. A `recipe_outcomes` child table can earn its weight in
a later session when the failure-mode taxonomy is well enough
understood to design schema for.

### LLM-free runtime invariant

The executor calls the LLM exactly once per `(plan, bound source)`
pair, the first time the plan is run. After that, the recipes are
on disk and runs are deterministic and cheap. The two test
guarantees that enforce this:

- `UnreachableProvider` panics on any `complete` call. Every offline
  test in `fetch_executor` constructs the executor with this
  provider and pre-authored recipes; if the test passes, the
  invariant held.
- The live ignored test does the same: pre-authored recipe, real
  network, `UnreachableProvider`.

A re-author-on-failure flow would cross this line and is
**explicitly out of scope** until the failure-mode taxonomy is
designed. A failed recipe surfaces in the report; the user decides
what to do.

## Consequences

### Good

- The cost model is legible: per plan, the LLM bill is bounded by
  Level-1 (one call) plus Level-2 (one call per bound source). The
  user can re-run a plan at zero LLM cost.
- The audit trail (`fetch_runs`) gives the UI a "did I already
  fetch this and what happened" answer in one indexed query.
- Status validation localises the "is this plan ready to fetch?"
  check inside the executor; the api command doesn't have to know
  the lifecycle rules.

### Tradeoffs

- A plan that's accepted with stale recipes won't re-author them
  on its own. When the source's shape drifts under the recipe, the
  user sees a `Failed { stage: Apply | Fetch }` outcome and has to
  decide. A future session that adds re-authoring will need a
  policy here (when to re-author? when to refuse?).
- Per-recipe outcomes only live in the synchronous report. If the
  user closes the app between runs, only the aggregate counters
  survive on disk. This is a deliberate bet that the synchronous
  display + the run-summary timeline are enough for now; the
  bet may not hold and future schema work is anticipated.

## Alternatives considered

### "Accepted means the plan is ready, not that fetch is gated"

Rejected. If `accepted` doesn't gate fetch, then `pending` is just
a UI toggle, not a guard. The whole point of the human-in-the-loop
boundary is that the user *decides* before any external action; a
soft gate doesn't deliver that.

### Per-recipe outcome rows in the same session

Rejected. The shape of a `recipe_outcomes` row depends on what
queries the UI actually wants to run against it (per-source
failure rate? failure-stage distribution? since-when-broken?). We
don't have those queries yet. A premature schema choice here is
harder to undo than a missing one.

### Background scheduler (run all accepted plans nightly)

Out of scope. One plan, one call, user-initiated — the simplest
shape that lets the user see the runtime path work. Scheduling is
a strictly additive feature that lands later.
