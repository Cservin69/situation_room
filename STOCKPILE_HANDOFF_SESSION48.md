# STOCKPILE — Session 48 handoff

You are starting Session 48. Session 47 shipped the multi-recipe-
per-nomination architectural piece bundled as `SESSION_47_PATCH_1.md`.
Build + test green across the workspace (335/335 pipeline, 61/61
secure, all other crates green; 12 ignored remain the existing
`#[ignore]` live tests). UI check green.

**Read this file. Read `SESSION_47_PATCH_1.md`. Do not start by
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

## What works today (post-Session-47)

Carrying forward from Session 46's "What works today" + Session 47's
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
- `fetch_run_outcomes` table (migration 0016) — per-(run, recipe-
  or-source, outcome_kind) rows persisted by the executor at run
  completion (Session 46 patch 1).
- `recipe_outcomes_history(plan_id, run_limit)` IPC — the recipe-
  success heatmap's primary read (Session 46 patch 1).
- `RecipeOutcomesHeatmap.svelte` slotted between `FetchReport` and
  `RecipesPanel` (Session 46 patch 1).
- `expectation_coverage(plan_id)` IPC — pure walk over
  `produces[].expectation` references (Session 46 patch 1).
- `ExpectationCoverage.svelte` matrix slotted in `PlanReview.svelte`
  (Session 46 patch 1).
- `HostBackoff::snapshot()` accessor + `HostBackoffSnapshotDto`
  exist; no IPC command yet — drive-by left ready for piece B
  (Session 46 patch 1).
- **Multi-recipe per nomination** (Session 47 patch 1). One
  nomination drives up to `MAX_AUTHORS_PER_NOMINATION = 4`
  authoring calls — one per target expectation against the same
  prefetched bytes. The recipe-author prompt is v1.15: every
  authoring call carries a `{{TARGET_EXPECTATION}}` section and
  the validator rejects bindings that target a different
  expectation. The executor's authoring loop is now one-propose-
  per-attempt / N-author-per-attempt, with the first authored
  recipe locking the URL for siblings. Per-recipe `dedup_key`
  widened to `{plan_id}:{nomination_id}:{bucket}:{index}`.
- **Manual re-author preserves free-choice authoring.** The
  `reauthor_recipe` path passes `None` for the new
  `target_expectation` parameter; the LLM continues to choose the
  expectation as before. Existing recipes' lineage chains are
  unaffected.

## Session 48 scope — three candidates, pick one

### Piece A — observe Session 47 in a live run, document the failure modes (RECOMMENDED first)

The Session 47 architectural piece needs a live run before any
follow-on work. The lithium plan from end-of-Session-45 is the
motivating fixture: USGS PDF prefetch carries production AND
reserves; with Session 47's flow, the executor authors against
both in one nomination. The expectation-coverage matrix and the
heatmap make the result visible per-recipe and per-fetch-run.

Run the lithium plan, observe what the matrix and heatmap look
like under the new flow, and write up two artefacts:

1. **A `failure_cases/recipe_author/` entry** if any decline
   surfaces that previously didn't (e.g., the LLM declines for
   `reserves` but authors for `production` against the same
   bytes — that's an honest narrow-coverage signal, not a bug).
   The entry names the expectation, the prefetch shape, and the
   decline reason.

2. **A short `docs/observations/2026-XX-XX-session-47-live-run.md`**
   describing what the matrix looked like before vs after, what
   the heatmap shows about per-target outcomes per run, and any
   surprises (e.g., the LLM authoring for an expectation we
   didn't expect would land).

Why it's the right Session 48 piece:

- The architectural piece landed; the next-session discipline is
  to *evaluate* it against the ground truth before stacking
  another piece on top.
- The fixture exists (the Session 45 lithium plan); no new
  classification round is needed.
- Both artefacts are <200 lines and the operator's terse-handoff
  preference aligns with prose, not code.

What to watch out for:

- **Don't tune the prompt based on one live run.** v1.15's
  contract is intentional; if the live run reveals a systematic
  decline pattern (e.g., the LLM struggles with the
  target-expectation framing on event_type buckets), that's a
  failure_case entry, not an immediate prompt edit. Prompt edits
  earn their own session and want >1 data point.
- **Don't extend `MAX_AUTHORS_PER_NOMINATION`** without
  observing the cost on a real run. The cap protects token bills;
  raising it should be motivated by an observed coverage gap the
  cap caused, not by speculation.

### Piece B — per-host backoff status panel (still drive-by-ready)

`HostBackoff::snapshot()` and `HostBackoffSnapshotDto` ship in
Session 46. Mounting the IPC command and the Svelte panel is one
screen of work; the Session 47 handoff carried the full design
notes verbatim from Session 46 for this piece, and they still
apply.

Where it lives:

- New IPC command: `host_backoff_state()` →
  `Vec<HostBackoffSnapshotDto>`. No parameters; pure read over
  `state.host_backoff.snapshot()`. Lives in
  `crates/api/src/commands.rs` next to `recipe_outcomes_history`.
- New Svelte component: `HostBackoffStatus.svelte`, slotted at
  the top of `PlanReview.svelte` (or in a sibling status strip).
  Auto-refresh on a 5s polling interval — the right fit because
  backoff state changes during a fetch run, not in lockstep with
  Run Fetch clicks.
- `apps/desktop/src/lib/api/client.ts` — `hostBackoffState()`
  wrapper. `apps/desktop/src/stores/plans.svelte.ts` — new
  `hostBackoff` state field, `refreshHostBackoff` helper, polling
  setup/teardown around plan selection.

What to watch out for:

- **Don't surface "wait_seconds_remaining" as the only signal.**
  A host with `consecutive_failures = 0, wait_remaining = 0` is
  clean; a host with `consecutive_failures > 0, wait_remaining = 0`
  is *recovering* (the schedule expired but the failure history
  is still in effect for the next signal). Render both.
- **Don't poll when the panel is hidden.** `selectPlan` mounts
  the panel; `clearSelection` should stop the timer.
- **Don't widen the snapshot accessor's surface** to add fields
  the IPC command would otherwise want. The current shape is the
  contract; if the panel needs more (e.g., last_signal_kind), add
  it as its own well-motivated field on the snapshot first.

### Piece C — sources-memory panel (carried over)

`Store::sources_memory` already returns the recency-sorted list of
URLs that have succeeded at least once for any plan. The classifier
sees this as `{{SOURCES_MEMORY}}`; the operator doesn't see it at
all. A panel that mirrors what the classifier sees ("here are the
30 URLs your prior plans have learned to fetch from") makes the
classifier's grounding visible.

The Session 46 and 47 handoffs both carried the design notes for
this piece; they still apply unchanged.

## Out of scope for Session 48

- **Promotion pipeline (ADR 0004).** Substantial piece. Its own
  handoff. The handoff before that one will need to spend time on
  the authoritative-vs-consensus design choice.
- **Iterator Phase 2 (ADR 0016).** Its own session.
- **Charts on Observations / Events.** Hold until promotion lands.
- **xAI Responses API migration.** Only if a live `grok-4.3` run
  shows chat/completions silently ignoring `reasoning_effort`.
- **Cross-plan recipe-failure aggregation (Session 46 piece D).**
  The expectation-coverage matrix is per-plan; cross-plan
  aggregation is a different surface. Pick it later.
- **Tightening `reauthor_recipe` to require a target.** Session
  47 left the manual re-author path on free-choice authoring
  intentionally. Tightening it would touch the dialog UX (the
  operator picks an expectation alongside the failure note) and
  earn its own session.
- **Raising `MAX_AUTHORS_PER_NOMINATION`.** Wait for live-run
  evidence per piece A.

## Hard rules carried over

Same as Sessions 41–47:

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
  source-specific routing rules. ADR 0007's golden rule applies
  to prompt text as much as code.
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
- ts-rs emits Rust `u64` as TS `bigint` since v8. Coerce to
  `Number(...)` before string interpolation.

Multi-recipe-per-nomination-specific (Session 47):

- **One recipe targets exactly one expectation.** The validator
  enforces this when `target_expectation = Some`. The legacy
  free-choice path (`None`) preserves Track A's manual re-author
  behaviour and is the only surface where `produces[].expectation`
  values can vary across bindings of one recipe.
- **The cap protects token bills.** `MAX_AUTHORS_PER_NOMINATION`
  is a load-bearing constant; raising it should be backed by
  observed coverage data, not speculation.
- **`build_target_expectations` excludes `document_source`** by
  construction (the nomination *is* the source). Don't add
  document_source to the list — recipes targeting their own
  source bucket would be circular.
- **dedup_key shape is `{plan_id}:{nomination_id}:{bucket}:{index}`.**
  Pre-Session-47 recipes still carry the legacy
  `{plan_id}:{nomination_id}` shape; both shapes coexist in the
  `dedup_key` column without collision because their byte-length
  prefixes differ.

## Things you will be tempted to do that are wrong

Same as Sessions 41–47, plus new ones surfaced by Session 47:

- **Edit the recipe-author prompt v1.15 to "be more confident
  authoring."** Don't. The narrow-honest-coverage rule and the
  decline path's "don't stretch" framing are intentional. If a
  live run shows declines that surprise you, that's a
  failure_cases entry, not a prompt edit.
- **Call `author_recipe` with `None` from the executor's main
  authoring path.** That bypasses the constraint and is exactly
  the regression Session 47 closed. The free-choice surface is
  the manual re-author flow alone.
- **Tighten the executor's lock-on-first-success behaviour.** A
  declined target against the locked URL surfaces as an
  `ExpectationDecline`; do *not* re-fetch a different URL for
  that target alone. Re-fetching per-target would multiply the
  fetch bill and risk stamping siblings of the same nomination
  with mismatched `source_id`s.
- **Add a "skip empty buckets" cleverness in
  `build_target_expectations`.** Empty buckets simply contribute
  zero entries to the concatenation; skipping is what the for-
  loop already does. Adding an explicit branch would be
  source-specific routing in disguise (the LLM would lose
  context about what the plan structurally asked for).
- **Migrate pre-Session-47 recipes to the wider dedup_key shape.**
  Don't. Old recipes keep their old keys and continue to apply;
  the wider shape only governs new authoring. Migration would
  introduce a behavior change without a motivating bug.
- **Add a "retry SEC with a different UA" knob.** Source-specific
  routing in disguise. The Session 45 default UA is the answer
  for every host; SEC's HTML-site 403 is a URL-family problem,
  not a UA problem.
- **Configure `[per_host."<hostname>"]` timeout overrides.** No.
  Parameters are uniform; runtime adapts on observed signals.
- **Edit the propose-URL prompt to "stop suggesting SEC search
  pages."** Source-specific routing in prompt text.
- **Rebuild `RecipesPanel` instead of slotting alongside it.**
  The existing components are good; add new components, don't
  rewrite.
- **Add a chart library.** Charts land post-promotion.
- **Try to surface "live progress" of an in-flight fetch.** The
  per-host backoff state panel (piece B) is the closest
  principle-clean way; it reads state asynchronously while a
  synchronous fetch is running.
- **Bundle multiple pieces.** Don't. Session 46 used the budget;
  Session 47 spent it on the architectural piece alone. Session
  48 returns to the discipline: one piece, one tarball, one
  handoff.
- **Write a "Session 48 plan" document.** This file is the plan.
  Read it once, then code.
- **Ship a tarball / commit that "doesn't compile but is the
  right shape."** Each commit must compile.

## Files to read first

In order. Stop when you have enough to make the fix.

1. This file.
2. `SESSION_47_PATCH_1.md` (and `SESSION_46_PATCH_1.md` for
   matrix/heatmap context if piece A is the chosen scope).
3. The piece-specific files. For piece A (RECOMMENDED):
   - `apps/desktop/src/components/ExpectationCoverage.svelte` —
     the matrix surface; what changes per nomination after the
     Session 47 patch.
   - `apps/desktop/src/components/RecipeOutcomesHeatmap.svelte` —
     per-target outcomes appear as new rows in the heatmap with
     widened source_id strings.
   - `crates/pipeline/src/fetch_executor.rs::author_for_nomination`
     — the orchestrator; rustdoc names every observable shape.
   - `crates/pipeline/src/recipe_author.rs::render_target_expectation`
     — the placeholder rendering the LLM sees.
4. For piece B:
   - `crates/pipeline/src/fetch_backoff.rs::snapshot` — Session
     46's accessor.
   - `crates/api/src/commands.rs::recipe_outcomes_history` — the
     pattern to follow for the new IPC command.
   - `apps/desktop/src/components/RecipeOutcomesHeatmap.svelte` —
     the slot pattern + tone-reuse-from-`outcomes.ts` pattern.
5. For piece C:
   - `crates/storage/src/sources_memory.rs` — the existing query
     and DTO surface.

## Live-run observations from end-of-Session-47

No new live run yet — Session 47 was a code-only session. The
expected change against the Session 45 lithium fixture:

- **The USGS MCS PDF nomination should now produce up to 4
  recipes** (one per target expectation in declaration order:
  `production`, `reserves`, `mine_opened`, `mine`). The
  expectation-coverage matrix should show these as four chips
  on four different rows (one per bucket index).
- **Per-target declines surface as new heatmap rows.** Targets
  that decline against the locked URL+bytes appear with widened
  source_id strings (e.g.,
  `nom:<uuid>:observation_metric:1`). The heatmap's per-(recipe-
  or-source, source) grouping renders them as distinct rows
  rather than collapsing onto the authored siblings.
- **Nomination-level declines look unchanged.** When URL discovery
  fails (e.g., Reuters' propose-URL declining on first attempt),
  the outcome surfaces with the legacy `nom:<uuid>` source_id
  exactly as before Session 47. Session 40's keyed-each
  uniqueness invariant is preserved.

These predictions are what piece A's live run should validate.

## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git remote
they want the agent involved with, paranoid about security,
prefers honesty about uncertainty over false confidence.

**Workflow.** Direct in-place editing in the workspace folder
(`~/Documents/Claude/Projects/SituationRoom/`). Operator runs
cargo on their Mac with output teed into the repo root:

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
green" from "still streaming." Sandbox bash cannot reach
`crates.io` or `sh.rustup.rs` — there is no way to run `cargo`
from inside the agent's container, and that's fine because the
operator's Mac is the source of truth anyway.

After patch + green logs, agent says "rsync" or "ship it";
operator pastes the saved rsync block (with the `.env` exclusion)
to mirror the workspace folder to
`~/RustRoverProjects/situation_room/` for git/remote management.

Operator approves with terse signals — "go", "continue", a log
dump. Reciprocate. Don't pad responses with status preamble or
summary postamble; lead with the actual move. Resume mid-stream
on "continue", don't restart.

When operator pushes back, listen. They have caught architectural
drift more than once across these sessions and have been right
every time. The most important push-back to internalize: **the
LLM is the only specialist; do not hand-code commodity adapters
or source-specific routing.** Sessions 38–47 honor this rule by
giving the LLM better evidence (PDF framed tables, HTML scraper
digest, JSON shape outline, whole-document PDF coverage),
reacting at the network layer (default UA, typed timeouts, per-
host backoff) without source-specific knowledge, surfacing state
legibly (recipe-success heatmap, expectation coverage matrix),
and now allowing the LLM to author N narrow recipes per
nomination — each constrained to one expectation by the prompt
and the validator — never encoding source-specific knowledge
anywhere.

After Session 48 ships its chosen piece, the runway opens for
promotion (ADR 0004) — the largest remaining backbone piece.
That session's handoff should spend time on the authoritative-
vs-consensus design choice before committing to a shape.

End of handoff.
