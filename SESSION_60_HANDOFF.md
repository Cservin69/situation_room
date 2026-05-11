# Session 60 — Handoff

Session 60 took on Session 59's four-track recommendation (A
deepen Observations, B classifier-bias, C recipe-author multi-
field, D dashboard honesty) and worked all four as one
investigation. The session deliverable is two ADRs plus a
direction pick for A and a decision for D. **No code shipped**,
per the kickoff discipline that this session would only widen the
problem statement, not narrow it.

The headline finding revises Session 59: the "classifier-bias"
framing was downstream of a more proximate cause. The 40-to-1
observation-vs-event ratio Session 59 observed is consistent with
a classifier that emits a mixed bucket distribution but whose
non-Observation entries are dropped by the executor before
reaching the recipe author.

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

## (A) Direction pick — recipe-iteration loop on observations

Three sub-directions were on offer (Session 59 §"Candidate
directions"): drill-into-metric chart, cross-plan canvas,
recipe-iteration loop on observations. Session 60 picks the
**recipe-iteration loop** as A's next move.

**Why this one, not the others.** ADR 0018's bucket-fair dispatch
widens the per-plan failure surface — non-Observation buckets get
extraction attempts and will frequently decline on topical
mismatch (a stats-agency PDF nominated for Observations is a
poor source for Events; bucket-fair dispatch will surface that as
declines rather than as silent skips). The operator's
inspection-and-recovery affordance becomes proportionally more
valuable. Drill-into-metric is bounded today by depth (N=1 on
most metrics); cross-plan canvas is multi-session product work.
Recipe-iteration delivers per-failure leverage that compounds
with ADR 0018's wider surface.

**Building blocks already exist.** `apps/desktop/src/components/`
already has `RecipesPanel.svelte`, `FetchReport.svelte`, and
`dialogs/ReauthorDialog.svelte`. The api crate has reauthor
plumbing (`crates/api/src/commands.rs` exposes a reauthor
command; `recipe_author::reauthor_recipe_with_feedback` carries
ADR 0013's feedback channel). The gap is the *integration*:
connecting failed extraction in the FetchReport to a one-click
"inspect bytes → edit selector → re-author with this guidance"
flow that doesn't require the operator to manually copy state
between three panels.

**Sketch.** A "from this failure" affordance attached to each
`RecipeOutcome::Failed` row in the FetchReport that opens a
ReauthorDialog pre-populated with: the recipe's current
selector, the apply-stage failure message head (already in
storage per Session 53 Piece C), and a textarea bound to ADR
0013's persistent feedback channel. Submit re-authors the recipe
with both the failure context and the operator's note as
combined feedback (the existing `compose_reauthor_feedback`
already handles the join).

**Why this is product-shaped, not infra-debt-shaped.** The
operator's current path on a failed extraction is multi-step:
notice the failure in FetchReport, navigate to RecipesPanel,
manually identify which recipe failed, open ReauthorDialog,
re-type the failure context as a free-text note, submit. The
proposed integration collapses that to one click and pre-fills
the note. It's a UI feature on top of plumbing that already
landed; no schema, no protocol, no closed-vocabulary expansion.
Lands in one Session.

**Sequencing.** A waits for ADR 0018 + 0019 (Session 61) so the
failure surface it operates on is the post-fix surface, not the
pre-fix one. Session 62 is the candidate slot.

## (D) Dashboard-honesty decision — keep the strip aspirational

Session 59 framed the question: does `RecordsDashboard`'s six-tile
strip + collapsed-pills row stay aspirational, or shrink to what
the pipeline actually produces? With ADRs 0018 + 0019 proposed,
the answer is **keep the strip aspirational**, with two small
tightening passes.

**Why aspirational.** Under the current executor + closed
vocabulary, five of six tiles will never light up. Once ADRs
0018 + 0019 land, three of the five become populatable directly
(Events with headline + date; Relations with from + to; Entity-
Attributes with key + value), and one more becomes populatable
when the EntityRegistry is fleshed out. Shrinking the strip now
would require expanding it again in two sessions; that churn is
worse than waiting.

**Tightening pass 1 — make zeros honest in the meantime.** The
strip currently shows `0 events`, `0 entities`, etc. dimmed. The
dimming reads as "empty" but the truth is "never tried" under
Session 60's finding. A single line under the strip — *"five of
six panels are pending the Session 61 dispatch fix"* — moves the
honesty from the layout to the prose.

**Tightening pass 2 — gate the pills on >0, not on schema
presence.** The five collapsed-pill panels (Events, Entities,
Relations, Documents, Assertions) currently render unconditionally
when the type-count is non-zero. Under bucket-fair dispatch they
will produce declines as well as records; the pill should still
render only on >0 records (today's contract), and the *declines*
should be visible in the FetchReport, not in the dashboard
pill row. The dashboard is the records view; the FetchReport is
the operator's diagnostic surface. Keep them separate.

Both passes are one-line UI changes in `RecordsDashboard.svelte`
and `PlanReview.svelte`; not a Session 60 deliverable, picked up
opportunistically in Session 61 or 62.

## Sequencing

```
Session 61: implement ADR 0018 (bucket-fair dispatch) + ADR 0019
            Phase 2A (css_select/json_path multi-field). Re-run
            hurricanes 5-trial eval. Re-run lithium baseline as
            regression check.
Session 62: A — recipe-iteration loop integration. Probably also
            ADR 0019 Phase 2B (remaining modes) if Phase 2A
            settled cleanly.
Session 63+: classifier-prompt investigation on a known-fair
             executor, if the post-ADR-0018 numbers show residual
             observation-bias.
```

This puts ADRs 0018 + 0019 on the critical path. A is
deliberately downstream — the recipe-iteration loop's value is
proportional to the failure surface it operates on, and that
surface widens after Session 61.

## Discipline (carried forward)

- **No code this session.** Two ADRs proposed; both deferred.
  Session 59's "no code shipped" precedent extends; the
  deliverable is two structurally-grounded design documents and
  a sequencing plan, not a half-implemented patch.
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
- **Memory updated.** Session 60's findings about the executor
  truncation and the per-field extraction gate are written up
  in the project-memory file under
  `spaces/c19dac53-…/memory/project_sr_session_60_two_gates.md`.

## Cleanup / state

- **No uncommitted code changes** as of this handoff write.
- **Two new ADR files** at `docs/adr/0018-target-bucket-fairness.md`
  and `docs/adr/0019-per-field-extraction-subspecs.md`.
- **No new eval runs.** Session 59's three JSONL files remain the
  baseline (lithium, Fed, hurricanes); they are the dataset
  against which Session 61's fixes will be validated.
- **Per-trial DBs from Session 59 were not kept** (`--keep-dbs`
  not passed). Session 61's first eval run should use
  `--keep-dbs` so per-bucket record types can be SELECTed
  directly rather than inferred from outcome messages.

End of handoff.
