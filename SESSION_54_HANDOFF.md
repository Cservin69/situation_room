# Session 54 — Handoff (parallelism work, scoped with explicit rollback)

This handoff scopes the next session to one piece of work: making
the fetch-run faster by parallelising the two layers of the author
loop that are currently sequential by accident, not by necessity.
Two stages, two commits, one explicit rollback path on each.

The operator's instruction at handoff: **prepare the work cleanly,
land it as two reviewable commits, and accept a `git reset` over
multi-session debugging if the result is unhappy.** No sunk-cost
fallacy. If Stage 1 runs cleanly but Stage 2 destabilises the run
log or trips xAI rate limits, reset to Stage 1 and stop. If Stage 1
itself misbehaves, reset to the pre-Session-54 head and stop.

## What the loops look like today

`crates/pipeline/src/fetch_executor.rs`. Three nested sequential
loops in the authoring half of `run_fetch_for_plan`:

```
load_or_author_recipes
└── for nomination in &nominations:                ← line 691  (Stage 2)
    └── author_for_nomination(nomination):
        └── for attempt in 1..=3:                  ← line 1304 (NOT TOUCHED)
            ├── propose_url    (1 LLM call)
            ├── prefetch       (1 HTTP fetch)
            └── for &target in &targets:           ← line 1409 (Stage 1)
                └── author_recipe (1 LLM call, Workhorse)
```

The 2026-05-10 06:14 lithium re-run took ~9m42s. Roughly:

- **Workhorse author_recipe** dominates (~10–25s per call).
- Per accepted URL: 4 sequential author_recipe calls × 7
  nominations = 28 author calls in series (not all reach 4 — many
  decline or short-circuit, but the upper bound is real).
- The `for &target in &targets` and the `for nomination` loops are
  the two layers that are sequential without a data dependency.

## Stage 1 — per-target parallelism inside the accepted-URL block

**File:** `crates/pipeline/src/fetch_executor.rs`, around line 1409
inside `author_for_nomination`'s attempt loop.

**The change.** Replace the `for &target in &targets` block with
`futures::future::join_all` over `targets.iter().map(|t|
author_recipe(...))`, then split the resulting `Vec<Result<...>>`
into `authored_this_attempt` and `declined_this_attempt` exactly
the way the sequential code does today.

**Why this is the easy stage.**

- The four author calls share immutable inputs: same prefetched
  bytes (`&[u8]`), same proposed URL, same plan, same recipe-
  feedback note, same target-agnostic context. Nothing one call
  produces is read by another.
- No DB writes happen during the call — recipes are persisted
  only after the loop completes via the existing `save_recipe`
  path in `load_or_author_recipes`.
- The aggregation pattern (`authored_this_attempt`,
  `declined_this_attempt`) is two `Vec` extends after all calls
  return — order-independent because subsequent code only iterates
  the vectors as sets.
- 4 simultaneous Workhorse-tier requests is well within typical
  xAI concurrency caps. No semaphore needed at this stage.
- The `Err(other)` arm (hard LLM error) bubbles the same way it
  does today; `join_all` returns all results, we early-return on
  the first `Err(other)` after the join.

**Expected speedup.** ~3–4× on the recipe-authoring time per
accepted URL. On the lithium plan, that turns the per-nomination
authoring window from ~60–100s into ~25–35s.

**Sketch of the code shape** (not literal — adapt to the actual
borrows; `auth_ctx` needs to be cloned per future or built
inside the closure):

```rust
let auth_futures = targets.iter().map(|&target| {
    let auth_ctx = AuthoringContext { /* same fields as today */ };
    async move {
        let res = author_recipe(
            ctx.provider, ModelTier::Workhorse, ctx.recipe_author_prompt,
            plan, &auth_ctx, Some(&prefetched_bytes), Some(target),
        ).await;
        (target, res)
    }
});
let results: Vec<(ExpectationRef, Result<FetchRecipe, AuthoringError>)>
    = futures::future::join_all(auth_futures).await;

let mut authored_this_attempt = Vec::new();
let mut declined_this_attempt = Vec::new();
for (target, auth_result) in results {
    match auth_result {
        Ok(mut recipe) => { /* same stamping as today */
            authored_this_attempt.push(recipe);
        }
        Err(AuthoringError::Declined { reason }) => {
            declined_this_attempt.push((target, reason));
        }
        Err(other) => return Err(FetchExecutorError::Authoring(other)),
    }
}
```

**Acceptance criteria for Stage 1.**

1. `cargo build` and `cargo test` pass on Mac per the cargo-on-Mac
   workflow. Particularly the existing test
   `run_fetch_for_plan_*` suite — none of those should change
   behaviour, only timing.
2. Live-test re-run on the lithium plan: same record count and
   same per-target decline rationales as a sequential baseline,
   measured wall-clock time per nomination drops by 2–3×.
3. Run-log lines per nomination still come out in target-index
   order (the aggregation is sorted by target order before
   logging, OR — acceptable — log lines come out in completion
   order with `target=` field intact so the operator can read
   them). The `target=ObservationMetric { index: N }` field is
   the only stable disambiguator; using it is fine.
4. No new `tokio::spawn` — `join_all` keeps tasks on the same
   runtime thread, so no `Send`-bound surprises with the
   `&dyn LlmProvider` borrow.

**Stage 1 rollback.** One commit. If Stage 1 misbehaves:
`git reset --hard <pre-Session-54 head>`. The pre-Session-54 head
is whatever commit lands SESSION_53_PATCH_2.md and the prompt
edits — operator names it explicitly when committing
("Patch 2 + handoff" or similar). After reset, the prompt-side
changes (Patch 2) survive; only the parallelism work is undone.

## Stage 2 — semaphore-gated cross-nomination parallelism

**File:** `crates/pipeline/src/fetch_executor.rs`, around line 691
inside `load_or_author_recipes`.

**The change.** Replace the outer `for (idx, nomination) in
nominations.iter().enumerate()` with a `FuturesUnordered` over
`author_for_nomination(nomination)` futures, and gate every
Workhorse-tier call inside `author_recipe` (and every Cheap-tier
call inside `propose_source_url`) behind a
`tokio::sync::Semaphore` shared via `Arc<Semaphore>` on the
`ExecutorContext`.

**Why a semaphore is non-negotiable for this stage.**

7 nominations × up to 4 parallel target authors (Stage 1) = up to
28 simultaneous Workhorse calls. xAI's published concurrency
caps are not in our docs, but observed behaviour in past sessions
suggests 12–16 simultaneous calls is comfortable, beyond that we
start seeing 429s. The semaphore's `.acquire_owned()` pattern
gates calls without changing the call sites' return types; the
permit is held for the duration of the LLM call and dropped on
return.

**Concurrency cap (proposed default).** 8. Conservative on day
one; can be raised to 12 once we have a clean run at 8. Configurable
via `SR_LLM_CONCURRENCY` env var (read once at executor
construction, defaulted to 8). Two reasons to plumb it as a knob:
operator can dial it down when xAI is having a bad day, and the
test suite can pin it to 1 for deterministic ordering.

**What stays sequential by necessity.**

- The `for attempt in 1..=3` loop inside `author_for_nomination`.
  Attempt 2 needs attempt 1's failure shape in `prior_attempts`;
  parallelising attempts would defeat the proposer's learning
  loop entirely. Untouched.
- `propose_url → prefetch → author_recipe` chain inside one
  attempt. Strict data dependency. Untouched.
- Phase 2 (`for recipe in &recipes` at line 456) — out of scope
  for Session 54. Today's authoring success rate makes Phase 2
  ~3s; not the bottleneck. Becomes interesting when the Patch 2
  prompt edits start producing more authored recipes.

**What about host-backoff?** Already concurrency-safe. The
`Arc<HostBackoff>` (`crates/pipeline/src/fetch_backoff.rs:287`)
holds its state in `Mutex<HashMap<String, HostState>>` — two
nominations both wanting `mining.com` will serialise via the
backoff layer without our needing to do anything. The live-test
already showed mining.com appearing twice across nominations in
the sequential run; concurrent calls will hit the same code path,
just with the second one waiting on the first.

**What about DuckDB writes?** Serialise via `Mutex<Connection>` in
`Store`. The reads
(`apply_failures_for_nomination`, `decline_count_for_nomination`)
happen once per nomination at the start of authoring; the writes
(`save_recipe`, `insert_fetch_run_outcome`) happen after the whole
authoring loop completes, in the post-loop iteration in
`load_or_author_recipes`. Lock contention is bounded and brief.

**What changes for the operator's mental model.**

- The run log lines stop being chronological "nomination 1, 2, 3...".
  They interleave by completion time. The `nomination_id =` field
  on every log line is the disambiguator.
- The "position N of M" log line stops being meaningful as
  "where am I in the queue" — it becomes "this is nomination N
  by source-priority order, regardless of when it finished." The
  field can stay; the meaning shifts.
- The FetchReport composes correctly regardless of order — outcomes
  are keyed by `nomination_id`, not insertion order. Verified by
  reading `crates/pipeline/src/fetch_executor.rs:439–524`: the
  outcomes Vec is built from `decline_outcomes` (already
  source-priority-ordered) followed by per-recipe outcomes; both
  surfaces are insertion-order in the FetchReport but the UI
  groups them by source.

**Expected speedup.** ~5–7× on top of Stage 1. Combined Stage 1 +
Stage 2 should turn the 9m42s baseline into roughly 1m20s–1m45s
on the lithium plan.

**Acceptance criteria for Stage 2.**

1. `cargo build` and `cargo test` pass. Test suite runs with
   `SR_LLM_CONCURRENCY=1` to preserve deterministic ordering in
   tests that assert on log content or outcome order.
2. Live-test re-run on the lithium plan: same record count and
   same per-target decline rationales as the Stage 1 baseline.
   Wall-clock time drops to under 2 minutes.
3. xAI 429s observed: 0 (or, if any, the existing retry path
   absorbs them and the run still completes).
4. Run-log readability: at first glance the log will look chaotic
   (lines from different nominations interleave). Acceptable as
   long as every line carries `nomination_id =` so the operator
   can `grep` for one nomination's full story.
5. UI: the FetchReport's source-grouped layout reads identically
   to a sequential run. Heatmap / coverage matrix unchanged.

**Stage 2 rollback.** Second commit on top of Stage 1. If Stage 2
misbehaves, two reset targets to choose from:

- `git reset --hard HEAD~1` → keep Stage 1, drop Stage 2 only.
- `git reset --hard <pre-Session-54 head>` → drop both.

Operator picks based on Stage 1's behaviour. If Stage 1 was clean
and Stage 2 destabilised, keep Stage 1. If both feel wrong, full
reset.

## What's intentionally not in this scope

- **Phase 2 parallelism** (the `for recipe in &recipes` at line
  456). Deferred until authoring success rate is high enough that
  Phase 2 becomes a measurable cost.
- **Streaming the FetchReport to the UI as nominations complete.**
  Tempting once nominations finish out of order (the operator
  could see "nomination 5 done while 2 is still running"), but
  this is a UI plumbing change, not an executor change. Out of
  scope for Session 54.
- **Provider-aware adaptive concurrency** (start at 4, ramp to 12
  if no 429s observed). Worth doing eventually; needs a feedback
  channel from the LLM client into the semaphore. Hand-set
  cap is fine for now.
- **Per-tier semaphores** (separate caps for Cheap and Workhorse).
  One shared cap is simpler and matches xAI's documented behaviour
  of accounting all model tiers against the same concurrency
  budget. Revisit if cost monitoring suggests otherwise.

## Patch 3 candidates surfaced by the 2026-05-10 06:36 live-test

The Patch 2 prompt edits moved record count from 1 → 2 on the
lithium plan and demonstrably fired (D.2 unblocked USGS
production; A.4 produced coverage-publisher tag URLs on SEC and
Reuters; A.5 produced focused on-host paths on IEA and World
Bank). The remaining failure modes are different in kind from
Patch 1/Patch 2's targets and want their own next-session
treatment. Three of them are prompt edits in the same shape as
Patch 2; one is a structural question worth holding for an ADR
conversation.

These are scoped as **Patch 3** — to be considered alongside or
after the parallelism work, depending on operator preference.
Parallelism (Stages 1 and 2 above) and Patch 3 are independent;
either can ship first or be skipped.

### Patch 3 candidate A — recipe-author "leaf, not container" rule

**File:** `config/prompts/recipe_author.md`. New paragraph in
the closed-vocabulary section, near the `css_select` /
`json_path` mode descriptions.

**Observed failure** (IEA `obs_metric:1` attempt 2, 06:42:23):
the LLM authored a `css_select` selector that returned
**112002 bytes** — the runtime's apply layer caps individual
field values at 2048 bytes and rejected with the verbatim
message *"selector matches a container element (body, div,
table) instead of a leaf"*. Piece B's shape validator caught
it at authoring time, which is the right outcome — but the
LLM should not be authoring container selectors in the first
place.

**The rule.** Recipes produce single scalar values per field.
A selector that returns more than ~2 KB of text is structurally
wrong: it has matched a wrapper (`<body>`, `<main>`, a
`<table>` element), not a leaf cell. Author the selector
against the inner-most element whose `textContent` is the
value you want — typically a `<td>`, `<span>`, `<a>`, or a
specific `[data-attr]`-bearing element. Never an outer
container.

Worked-example pair (principle-only; class shapes, not host
strings):

- *Wrong*: `css_select: "main"` against a report landing
  page → returns the entire main column, ~50–500 KB.
- *Right*: `css_select: "table.production-by-country tr td:nth-child(3)"`
  against the same page → returns the production cell of one row.

Pre-Patch-3 the LLM treats "the page contains the figure
somewhere" as license to author a coarse selector; the apply
layer's 2048-byte cap then triggers a validator decline. The
prompt should head this off explicitly.

### Patch 3 candidate B — recipe-author content-type-vs-mode coherence rule

**File:** `config/prompts/recipe_author.md`. New paragraph in
the closed-vocabulary section, near the `json_path` mode
descriptor.

**Observed failure** (IEA `obs_metric:3` attempt 2, 06:42:40):
the LLM authored a `json_path` extraction against a page that
the prefetch returned as HTML. The runtime's apply layer
declined: *"json_path: bytes did not parse as JSON: expected
value at line 1 column 1"*. Piece B caught it at authoring
time.

**The rule.** Each extraction mode requires a specific
content-type from the fetched bytes:

- `css_select` requires HTML (or XML with HTML-compatible
  parsing).
- `json_path` requires JSON.
- `regex_capture` requires text-shaped bytes (HTML, JSON, or
  plain text).
- `pdf_table` requires a PDF.
- `csv_cell` requires CSV.

The prefetch excerpt header names the content-type the bytes
arrived as. **Mode and content-type must agree at authoring
time.** A `json_path` recipe against an HTML page is not a
selector typo; it is a category error the prompt must forbid.

### Patch 3 candidate C — recipe-author required-field discipline

**File:** `config/prompts/recipe_author.md`. Strengthen the
existing schema-awareness section.

**Observed failure** (USGS `obs_metric:2` and World Bank
`obs_metric:0`, both 06:38–06:40): two distinct nominations
authored recipes whose `produces` bindings did not bind the
required `value` field on the observation content type. Piece
B caught both: *"content assembly failed: observation content:
missing field `value`"*.

**The rule.** Every binding's `field_mappings` must include
every field the target content type's schema marks as
required. Today the prompt names the schema (via
`{{TARGET_RECORD_SCHEMA}}`) but treats binding-completeness
as implicit. Add an explicit pre-flight bullet in the
schema-awareness section: *before submitting, walk the target
content type's required field list and confirm each one
has a `field_mapping` entry.* If a required field cannot be
sourced from the prefetched bytes, the recipe is honestly
not authorable for that target — decline.

### Patch 3 candidate D — proposer "L1 source identity is provenance hint, not contract"

**File:** `config/prompts/propose_source_url.md`. Refinement
of the reasonable-shot disposition (Patch 2's A.4).

**Observed failure** (Fastmarkets nomination, 06:45:41):
after two `404`s on `fastmarkets.com`, the proposer declined
with *"No alternative coverage publisher is appropriate
because the L1 description names this exact source."* The
nomination description was *"Fastmarkets battery-raw-materials
price assessments — daily lithium hydroxide and carbonate spot
pricing"*. The proposer treated *"Fastmarkets"* as a binding
identity rather than as a provenance hint about the
*data class* (commodities trade-press spot pricing).

**The rule.** The L1 nomination's named source is a hint about
the *kind* of source the L1 had in mind — a provenance class
anchor, not a contract that the URL must come from that
exact host. When the named host is unreachable (403 / 401 /
404 / timeout), the data class is still served by the same
class of publisher (commodities trade press, financial
portal, etc.), and the reasonable-shot disposition applies
unchanged. Decline rationales that read "the L1 names this
exact source" misread the nomination contract; the contract
is the *data*, not the *URL*.

This refines the Patch 2 reasonable-shot section: the
disposition applies even when the L1 nomination explicitly
names the unreachable host.

### Patch 3 candidate E (out-of-scope, for ADR conversation) — target-vs-nomination routing

**Observed failure** (USGS `obs_metric:3` spot_price, every
run): the L1 classifier asked for `spot_price` from USGS MCS
PDF, but USGS MCS doesn't carry spot pricing — it carries
production and reserves. The recipe-author honestly declines
each run; the per-nomination 4-target loop wastes one
Workhorse call per attempt on a target that was misrouted to
this source.

This is a Level-1 classifier issue, not a Level-2
proposer/author issue. Two architectural directions worth
discussing in the next session before any code change:

1. **Per-nomination target whitelisting** — the L1 classifier
   could emit, alongside the source nomination, the set of
   record buckets this source plausibly serves. The executor
   would skip targets outside the whitelist. Saves Workhorse
   calls; pushes the routing concern back to the L1 (where it
   belongs by ADR 0007).
2. **Per-target descriptor on the nomination** — symmetric
   to (1) but inverted: each nomination carries an explicit
   `targets: [observation_metric, event_type, ...]` set. The
   executor iterates only those.

Both have schema migration implications (`DocumentSourceNomination`
shape change). Both want an ADR before code. Out of scope for
Session 54's parallelism + Patch 3 prompt work; explicit
agenda item for the session after.

### Patch 3 sequencing recommendation

If Patch 3 lands at all, the four prompt candidates (A, B, C, D)
are independent and can ship as one combined patch — same idiom
as Patch 1 and Patch 2 (one commit, four marked sub-pieces).
None of them are blocked on the parallelism work; the
dependency goes the other way (Patch 3 prompt edits make
Stages 1+2 more valuable because each parallel author call
returns useful bytes more often).

Recommended order if both Patch 3 and parallelism are in scope:

1. Patch 3 prompt edits first (low risk, immediate live-test
   validation possible).
2. Stage 1 parallelism (per-target).
3. Live-test re-run; observe whether wall-clock drops as
   predicted and Patch 3 shows up in the success rate.
4. Stage 2 parallelism (across-nomination, semaphore-gated).
5. Live-test re-run; final observation on combined effect.

Each numbered step is a separate commit. Each has its own
rollback target. The discipline from the parallelism section
(no multi-session forensics; reset over fight) applies to
Patch 3 as well, though prompt edits are easier to roll back —
just revert the file.

## The discipline this handoff is structured around

- **Two commits, never one.** Stage 1 must land green and be
  observable before Stage 2 is written. The two-commit shape is
  what makes the rollback options meaningful.
- **No "while we're at it" extras.** The temptation to clean up
  the `author_for_nomination` function while editing it should be
  resisted. Cosmetic cleanup belongs in a separate session.
- **Reset is a first-class option.** If a session opens, lands
  Stage 1, and Stage 1 produces nondeterministic test failures
  or breaks the run-log mental model badly, the right answer is
  `git reset --hard <pre-Session-54 head>`, write a one-line
  note in this handoff explaining what went wrong, and move on.
  Multi-session forensics on parallelism bugs is a tar pit.
- **The Patch 2 prompt edits stay regardless.** They are
  orthogonal to this work; even a full Session 54 reset preserves
  them. Verify `config/prompts/recipe_author.md` reads `v1.16`
  and `config/prompts/propose_source_url.md` reads `v1.3` after
  any reset.

End of handoff.
