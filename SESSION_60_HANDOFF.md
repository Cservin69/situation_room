# Session 60 — Handoff

Session 60 took on Session 59's four-track recommendation (A
deepen Observations, B classifier-bias, C recipe-author multi-
field, D dashboard honesty) and worked all four as one
investigation. Two ADRs proposed (executor + closed-vocabulary
gates) and two frontend integrations shipped (D aspirational
note + A re-author-from-FetchReport affordance). Live screen on
"federal reserve rate policy" mid-session confirmed the
executor-truncation finding.

The headline finding revises Session 59: the "classifier-bias"
framing was downstream of a more proximate cause. The 40-to-1
observation-vs-event ratio Session 59 observed is consistent with
a classifier that emits a mixed bucket distribution but whose
non-Observation entries are dropped by the executor before
reaching the recipe author.

## Live-screen confirmation

Mid-session run on topic "federal reserve rate policy" produced
3/3 succeeded, 3 records (federal_funds_rate 3.64; unemployment_rate
×2 with +4.3 sparkline; FRED + federalreserve.gov on the wins).
Sits inside Session 59's Fed variance band (0–3 over 5 trials).

The decline log on the same run is the clincher: **12 decline
rows, every single one `:observation_metric:N`**. Zero
`:event_type:N`, zero `:entity_kind:N`, zero `:relation_kind:N`
dispatched on any nomination. Plan emitted four observation_metrics
(FFR, CPI, unemployment, GDP); executor's
`build_target_expectations(plan, 4)` filled the cap with metrics;
every event/entity/relation entry in the plan got truncated
before the recipe author saw it. Exactly the picture ADR 0018
predicts.

Nine of those twelve declines are content-type or shape-validator
rejections of LLM attempts to coerce FOMC calendar / GDP news-
release pages into observation_metric shape. Most of those pages
are *event-shaped*, not metric-shaped — the LLM keeps trying
because the dispatcher only offered it observation_metric slots.
ADR 0019's multi-field case lives directly inside this failure
mode.

## ADR 0018 — Target-bucket fairness (B)

[`docs/adr/0018-target-bucket-fairness.md`](docs/adr/0018-target-bucket-fairness.md).
Proposed, deferred to Session 61.

**Finding.** `fetch_executor::build_target_expectations` (
`crates/pipeline/src/fetch_executor.rs:991`) concatenates the four
record-typed expectation buckets in fixed declaration order —
observation_metrics, event_types, entity_kinds, relation_kinds —
and truncates the concatenation to
`MAX_AUTHORS_PER_NOMINATION = 4`. The constant's own doc-comment
is explicit:

> So for a plan with 4 obs metrics + 3 event types + 2 entity
> kinds, the executor authors against the first 4 entries of the
> concatenation (all four obs metrics in this case); the remainder
> is silently uncovered until the operator either re-classifies
> (which yields fresh nominations) or raises the cap.

The lithium worked example in `config/prompts/research_classifier.md`
emits four metrics. The Fed and hurricanes Session 59 plans likewise
emitted four metrics each, verified by the per-nomination decline
log: `:observation_metric:0/1/2/3` for every nomination that
reached the inner authoring loop, **zero `:event_type:N` entries
on any nomination across any Fed or lithium trial**. The single
`event_type:0` Session 59 attributed to one hurricanes trial
appears because that plan declared fewer than four metrics, so
the cap let one event slot through.

**Proposed fix.** Replace the declaration-order concatenation with
**round-robin bucket-fair dispatch**, and raise the cap to 6. The
worked example: a plan declaring 4 metrics + 3 events + 2 entity
kinds + 1 relation kind today emits `[obs:0..3]` per nomination;
under the ADR it emits `[obs:0, evt:0, ent:0, rel:0, obs:1,
evt:1]`. Every non-empty bucket gets at least one slot per
nomination, and across 5–7 nominations every bucket gets several
shots.

**Validation.** Re-run Session 59's hurricane 5-trial eval after
Session 61's implementation. Pre-fix: 1 `event_type:N` per-target
nomination across 5 trials. Post-fix expectation: ≥10. If the
count is still single-digit, the classifier-bias hypothesis
re-emerges and Session 62 picks up the prompt-side investigation
on a known-fair executor.

## ADR 0019 — Per-field extraction sub-specs (C)

[`docs/adr/0019-per-field-extraction-subspecs.md`](docs/adr/0019-per-field-extraction-subspecs.md).
Proposed, deferred to Session 61.

**Finding.** ADR 0016 (extraction iterator, Phase 1) explicitly
deferred multi-extracted-fields-per-match to "its own ADR after
Phase 1's contract has run in production for a few cycles." 22
sessions later, Phase 2 is the matching gate-opener to ADR 0018:
even with bucket-fair dispatch, the closed extraction vocabulary's
single-scalar-per-binding contract blocks magnitude-bearing
events, relations, and entity-attributes from being authored at
all. The Session 59 hurricane apply failures ("inner selector
matched no elements within iterator match … the inner selector is
targeted at a sibling rather than a descendant") are the LLM's
honest "this row carries multiple fields, I can only return one
scalar" signal.

**Proposed fix.** Extend `FieldValueSource` with a new variant
`ExtractedInner { spec: ExtractionSpec }` that carries a per-field
extraction sub-spec. The runtime evaluates each sub-spec against
the same per-match scope the binding's outer extraction operates
on, producing one leaf per FieldMap per match. The closed
extraction-mode enum stays at five; the change is in how FieldMap
references them — additive in shape, not in vocabulary.

Worked example schema in the ADR: an NHC storm-list recipe under
ADR 0019 emits one Event record per row, with `event_type` as
literal, `direction` as literal, `headline` from
`td.storm-name` (inner CSS), `valid_at` from `td.storm-date`
(inner CSS). Multiple extracted leaves per match, each authored
once, applied deterministically forever per ADR 0007.

**Two-phase rollout.** Phase 2A (Session 61) covers `css_select`
and `json_path` (the dominant share of listings — news cards,
arXiv listings, API responses returning arrays of objects). Phase
2B (Session 62+) covers `csv_cell`, `pdf_table`, `regex_capture`
once the fixture corpus is broader.

**Prompt revision.** Session 61 ships v1.19 of `recipe_author.md`
with a new "Multi-leaf records — when one row carries several
fields" subsection, plus a worked example pair (NHC storm rows;
arXiv recent listings).

**Validation.** Hurricane re-run with both ADR 0018 and ADR 0019:
expect ≥3 Event records per trial on NHC sources, each with
distinct `headline` (storm name) + `valid_at` (formation date)
extracted leaves. A secondary "global semiconductor exports 2024"
topic should produce Relation records (`supplies_to`,
`subject_to_sanction`) — Relations are the canonical multi-field
case (`from` + `to`).

## (A) Recipe-iteration loop on FetchReport — SHIPPED

Three sub-directions were on offer (Session 59 §"Candidate
directions"): drill-into-metric chart, cross-plan canvas,
recipe-iteration loop on observations. Session 60 picked the
**recipe-iteration loop** and shipped its first integration
this session.

**What shipped.**
[`apps/desktop/src/components/FetchReport.svelte`](apps/desktop/src/components/FetchReport.svelte)
now mounts the same `ReauthorDialog` the `RecipesPanel` uses,
opened by a `re-author` button on each `failed` outcome row. The
dialog pre-fills with:

- `sourceId` from `o.source_id`,
- `priorRecipeShortId` from `shortId(o.recipe_id)`,
- `failureMessage` from `o.message`,
- `bytesExcerpt` loaded asynchronously via
  `latestAttemptForRecipe(o.recipe_id)`,

writing through the same `reauthorRecipe` runes-store helper that
`RecipesPanel` uses. The new recipe's lineage chip surfaces in
`RecipesPanel` after submit with no separate refresh roundtrip —
same pattern as the flag-from-decline affordance (Session 30,
ADR 0013).

**Pre-Session-60 path:** notice failure in FetchReport → navigate
to RecipesPanel → scan for the matching recipe → click *its*
re-author button → re-type the failure context into the dialog
note. Four steps, context-losing.

**Post-Session-60 path:** click `re-author` on the failure row →
the dialog opens with everything already populated.

**Why this and not the others.** ADR 0018's bucket-fair dispatch
will widen the per-plan failure surface — non-Observation buckets
get extraction attempts and will frequently decline on topical
mismatch (a stats-agency PDF nominated for Observations is a
poor source for Events; bucket-fair dispatch will surface that as
declines rather than as silent skips). The operator's inspection-
and-recovery affordance becomes proportionally more valuable.
Drill-into-metric is bounded today by depth (N=1 on most metrics);
cross-plan canvas is multi-session product work. Recipe-iteration
delivers per-failure leverage that compounds with ADR 0018's
wider surface.

**Building blocks reused.** `RecipesPanel.svelte`,
`FetchReport.svelte`, `dialogs/ReauthorDialog.svelte`, the
`reauthor_recipe_with_feedback` IPC command, ADR 0013's
recipe-feedback channel — every piece already existed. The
patch is integration-only; no schema, no protocol, no closed-
vocabulary expansion.

**Why not on `declined` outcomes too.** A decline carries no
recipe — there's nothing to re-author. The flag-button affordance
remains the right surface for declines; the operator's note flows
into the next *initial* authoring attempt for the source via the
`{{RECIPE_FEEDBACK}}` channel (ADR 0013), not via re-authoring.
The two affordances stay distinct by failure shape.

**Carry-forward verification.** The Session-60 commit hasn't been
exercised against an apply-stage `failed` outcome on a live run
yet (the Fed rate-policy run in mid-session was 100% declines).
Session 61's first apply-stage failure exercises the button
organically; until then it's a code-review-only verification.

## (D) Dashboard honesty — SHIPPED

Session 59 framed the question: does `RecordsDashboard`'s six-tile
strip + collapsed-pills row stay aspirational, or shrink to what
the pipeline actually produces? With ADRs 0018 + 0019 proposed,
the answer is **keep the strip aspirational**, with one prose
honesty pass shipped this session.

**What shipped.**
[`apps/desktop/src/components/RecordsDashboard.svelte`](apps/desktop/src/components/RecordsDashboard.svelte)
gained an `.aspirational-note` block rendered directly under the
type-count strip when `totalRecords > 0 && pendingTypes.length
=== 0`. The note reads:

> Events, Entities, Relations, Documents, and Assertions become
> populatable once the executor's bucket-fair dispatch and per-
> field extraction land — see `docs/adr/0018` and `docs/adr/0019`
> (Session 61). Dimmed tiles above mean "not yet tried," not
> "tried and empty."

Moves the honesty from the layout (dimmed tiles reading as
"empty") to the prose ("never tried" is the actual state). The
note removes itself automatically once Session 61's ADRs land and
the typed panels populate, since at that point `pendingTypes.length
> 0` and the pill row takes over.

**Why aspirational.** Under the current executor + closed
vocabulary, five of six tiles will never light up. Once ADRs
0018 + 0019 land, three of the five become populatable directly
(Events with headline + date; Relations with from + to; Entity-
Attributes with key + value), and one more becomes populatable
when the EntityRegistry is fleshed out. Shrinking the strip now
would require expanding it again in two sessions; that churn is
worse than waiting.

**Pill-gating tightening pass** was already in place — the
existing `pendingTypes` derivation in `RecordsDashboard.svelte`
filters to `count > 0`, so collapsed pills only render for record
types the plan actually produced. No change needed beyond the
aspirational note.

## Sequencing

```
Session 61: implement ADR 0018 (bucket-fair dispatch) + ADR 0019
            Phase 2A (css_select/json_path multi-field).
            Run hurricanes 5-trial eval with --keep-dbs.
            Regression-check lithium 5-trial.
            Flip both ADRs to Status: Accepted on validated
            post-fix numbers.
Session 62: ADR 0019 Phase 2B (csv_cell/pdf_table/regex_capture)
            if Phase 2A settled cleanly. First live exercise of
            the FetchReport re-author button against an
            apply-stage failure if Session 61 surfaces one.
Session 63+: classifier-prompt investigation on a known-fair
             executor, if the post-ADR-0018 numbers show residual
             observation-bias.
```

This puts ADRs 0018 + 0019 on the critical path. A's re-author
button is shipped but unverified against an apply-stage failure;
Session 61's hurricane re-run will likely surface the first one
naturally.

## Discipline (carried forward)

- **Closed-vocabulary discipline preserved.** ADR 0019 extends
  the closed vocabulary by adding one FieldValueSource variant
  (`ExtractedInner`), not by adding extraction modes. The
  five-mode enum stays five.
- **No source-routing in the executor.** ADR 0018 explicitly
  rejects source-priority-weighted dispatch in its alternatives
  section — the executor must not encode heuristics like "this
  is a news host so prefer events."
- **Schema-first.** Both ADRs frame their proposals in terms of
  the existing `crates/core/src/schema/content.rs` types
  (`EventContent`, `RelationContent`, `EntityAttributeContent`)
  and the existing `crates/pipeline/src/recipes.rs` types
  (`ProductionBinding`, `FieldMap`, `FieldValueSource`).
- **`--keep-dbs` on every Session 61 eval run.** Session 60's
  investigation was bottlenecked by per-trial DBs not being kept;
  future investigations want records inspectable directly rather
  than inferred from outcome messages.
- **Memory updated.** Session 60's findings live in
  `spaces/c19dac53-…/memory/project_sr_session_60_two_gates.md`;
  MEMORY.md index updated to mark Session 59's classifier-bias
  framing as superseded.

## Cleanup / state

- **5 files committed** in the rsync'd `before session 61`
  snapshot (commit `f2f68ad`):
  - `docs/adr/0018-target-bucket-fairness.md` (new)
  - `docs/adr/0019-per-field-extraction-subspecs.md` (new)
  - `SESSION_60_HANDOFF.md` (this file)
  - `apps/desktop/src/components/RecordsDashboard.svelte`
    (aspirational note + supporting CSS)
  - `apps/desktop/src/components/FetchReport.svelte`
    (re-author-from-failure integration + supporting CSS)
- **`cargo test --workspace` green** post-rsync: 786 tests
  passed, 0 failed, 13 ignored (live-network tests, unchanged).
  No Rust files were touched this session, so the green run is a
  no-regression check rather than coverage of new code.
- **`npm run check` not run.** The FetchReport + RecordsDashboard
  edits added a new type alias (`FailedOutcome` via
  `Extract<RecipeOutcomeDto, { kind: 'failed' }>`), new imports,
  four new `$state` rune declarations, and a dialog mount. No
  ts-rs types were touched. Two minutes of `npm run check` in
  `apps/desktop` before Session 61's first live test is the right
  hygiene step.
- **No new eval runs.** Session 59's three JSONL files remain the
  baseline (lithium, Fed, hurricanes); they are the dataset
  against which Session 61's fixes will be validated.

End of handoff.
