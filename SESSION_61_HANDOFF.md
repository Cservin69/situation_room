# Session 61 — Handoff

Session 61 implemented both ADRs Session 60 proposed (0018 bucket-
fair dispatch + 0019 per-field extraction sub-specs Phase 2A),
shipped the v1.19 recipe-author prompt with the multi-leaf
records section, re-ran the hurricane 5-trial eval, and
regression-checked lithium. The result: ADR 0018 validates
cleanly and lands as Accepted; ADR 0019's infrastructure is in
place and tested but the LLM did not exercise `extracted_inner` in
any of 10 live trials, so the ADR remains Proposed-with-
implementation-landed pending a Session 62 prompt iteration.

## Headline finding

ADR 0018's bucket-fair dispatch is doing exactly what the ADR
predicted. Hurricane per-target nominations summed across 5
trials:

| bucket            | Session 59 | Session 61 | delta   |
|-------------------|-----------:|-----------:|--------:|
| ObservationMetric | 12         | 73         | +6.1×   |
| EventType         | 1          | 63         | +63×    |
| EntityKind        | 0          | 33         | from 0  |
| RelationKind      | 0          | 24         | from 0  |

The ADR's `≥10 event_type per-target nominations across 5 trials`
post-fix expectation is met with substantial headroom (63 ≫ 10).
Every non-Observation bucket lit up from 0–1 to 24–63 per-target
authorings. The starvation mode Session 60's ADR was designed to
close is closed.

Records produced rose from 0 to 2 across the same 5 hurricane
trials. The records-per-trial improvement is small because the
bottleneck moved downstream — most newly-dispatched non-Observation
expectations decline at the recipe-author stage on topical-
mismatch grounds. AP News, FEMA's DisasterDeclarationsSummaries
endpoint, NCEI's billion-dollar-disasters landing page, the
Munich Re corporate media listing — these were nominated by the
classifier as plausible storm-formation sources but the
recipe-author correctly declined on inspection. That's the right
outcome: declines are honest, and they're now decorating per-
expectation slots that previously weren't even tried.

The remaining work to convert dispatched expectations into apply-
stage records lives in two adjacent surfaces: multi-leaf record
shapes (ADR 0019) and the recipe-author's source-shape recognition
prompt (Session 62+ candidate direction).

## ADR 0018 — Accepted

[`docs/adr/0018-target-bucket-fairness.md`](docs/adr/0018-target-bucket-fairness.md)
flipped to Accepted with the validated post-fix numbers inline.

**Implementation.** `crates/pipeline/src/fetch_executor.rs`:

- `build_target_expectations` rewritten as a round-robin over the
  four record-typed buckets in declaration order. Each non-empty
  bucket gets at least one slot per pass; subsequent passes
  refill the densest bucket from the same declaration order
  until the cap is reached or every bucket is exhausted.
- `MAX_AUTHORS_PER_NOMINATION` raised from 4 to 6. Six covers
  one slot per bucket plus two more in the densest bucket — under
  the ADR worked example of 4 metrics + 3 events + 2 entities +
  1 relation, dispatch becomes `[obs:0, evt:0, ent:0, rel:0,
  obs:1, evt:1]` per nomination.
- The Stage 2 concurrency comment near `author_recipe` updated:
  7 nominations × 6 targets = up to 42 simultaneous calls in
  principle, throttled by the cap-8 LLM semaphore.

**Tests.** Five unit tests in the fetch_executor test module:
one-each-bucket (cap ≥ 4), does-not-starve-non-obs-buckets (the
ADR's headline guarantee under cap=6), four-buckets-full worked
example (the canonical ADR shape), empty-plan termination, and
document_source-still-excluded. The Session 47 declaration-order
tests were renamed to ADR-0018 framing and their assertions
updated.

**Lithium regression — the predicted trade-off.** Lithium records-
per-trial dropped from 2.0 (Session 58's variance-collapsed
baseline) to 1.4 (records per trial: 1, 2, 2, 1, 1). This is the
explicit trade-off the ADR documents in its Consequences section:
observation_metric slots per nomination drop from 4 to 2 under
bucket-fair dispatch (obs shares the cap with three other
buckets rather than monopolising it). The 1.4 mean sits inside
the Session 56 historical variance band (0/1/1/2/3, mean 1.4), so
the regression vs. Session 58 may be partially variance rather
than purely the dispatch change. Session 62 investigations should
target apply-stage success-rate improvements rather than rolling
back the dispatch fairness.

## ADR 0019 — Proposed (implementation landed, unexercised live)

[`docs/adr/0019-per-field-extraction-subspecs.md`](docs/adr/0019-per-field-extraction-subspecs.md)
keeps the **Proposed** status because the live-data validation
criterion is unmet: across 10 trials the LLM authored zero
recipes using `extracted_inner`. The Status section in the ADR
documents what Phase 2A shipped and what Session 62 needs to
deliver to promote it to Accepted.

### What shipped

- **Type** (`recipes.rs`):
  `FieldValueSource::ExtractedInner { spec: ExtractionSpec }`,
  serde-tagged `extracted_inner`, round-trip-tested.
- **Validator** (`recipe_author.rs`): four rules in step 5b of
  `build_validated_recipe` —
  - (i) inner-spec mode congruence with outer extraction;
  - (ii) Extracted and ExtractedInner mutually exclusive per
        binding;
  - (iii) at-least-one extraction reaches the bytes per binding;
  - (iv) Phase-2A supports `css_select` and `json_path` only;
        `csv_cell` / `pdf_table` / `regex_capture` defer to Phase
        2B. Five validator tests.
- **Runtime** (`recipe_apply.rs`):
  `compute_inner_extractions_css/json` (per-binding inner-leaf
  resolution), `extract_json_within` (scoped JSONPath
  evaluation), `apply_json_iterator` (new (JsonPath, JsonPath)
  iterator path), `scalar_inner_extractions` (scope resolution
  for scalar+multi-leaf recipes). The shape validator
  (`validate_recipe_shape_against_bytes`) extended to evaluate
  inner sub-specs against the real per-match scope rather than a
  placeholder stub. Three runtime tests pin css iterator multi-
  leaf, json iterator multi-leaf, and legacy iterator backwards
  compat.
- **Prompt** (`config/prompts/recipe_author.md` v1.19): new
  "Multi-leaf records — when one row carries several fields"
  section between mode-selection and decline-path; two worked
  examples (relations from an ownership-table CSS listing;
  events from a JSON listing API); five new "What NOT to
  produce" entries; v1.19 changelog entry.

### What's working — validator rule (iii) catches real LLM bugs

Across the 10 hurricane + lithium trials, the ADR 0019 validator
rejected three separate LLM attempts to author all-literal-or-
plan bindings (zero FieldMaps reading the source bytes). These
recipes would have emitted a constant record on every fetch
forever pre-fix; the validator now catches them at authoring time
with the precise error
*"binding[N]: no FieldMap has source `extracted` or `extracted_inner`
— every field is `literal` or `from_plan` (ADR 0019 §Validation
rules rule 3)"*. The decline routes to the operator with an
honest framing and the recipe is never persisted. Phase 2A has a
real safety effect even before the positive case lands.

### What's not working — LLM not reaching for `extracted_inner`

Direct evidence across 10 trials:

- **Zero recipes authored** with `extracted_inner` (grep over the
  full trial log).
- **Two NHC iterator+inner-selector apply failures** with the
  Session 60 signature *"inner selector matched no elements
  within iterator match (the iterator's selector matched a card,
  but the inner selector found nothing inside it)"*. The recipe-
  author authored a legacy single-leaf iterator recipe rather
  than reaching for the multi-leaf shape v1.19 was supposed to
  teach.
- **Hurricane EventType targets declined at recipe-author stage**
  with reasoning like "source provides aggregate statistics, not
  per-storm events" or "no structured per-storm landfall dates."
  These are honest declines, but they're declines that *should
  have prompted the multi-leaf inspection* on sources like the
  NHC TCR index (which has 17 rows per the trial 4 log, each
  carrying storm name + date + statistics). The v1.19 prompt's
  multi-leaf section either isn't prominent enough at the
  binding-shape decision point or the worked-example class
  shapes (`tr.storm-row`, `td.storm-name`) don't align with the
  TCR page's actual markup.

## Session 62 — candidate directions

Three coherent product directions, listed in order of expected
leverage. Pick one as the session's primary; the others stay
candidates.

### A. Prompt iteration (v1.20) to surface `extracted_inner`

The most direct path to flipping ADR 0019 to Accepted. v1.20
candidates:

1. **Elevate the multi-leaf section.** Currently between
   "Selecting the mode" and "decline path"; promote to a top-
   level subsection and add a "Is this row multi-leaf?" decision
   rubric near the FieldMap-shape decision point.
2. **Replace the worked-example class shapes** with shapes the
   LLM actually encounters. The TCR page's storm-summary table
   has class structures like `table.storms` / `tr` (with no
   class) / `td:nth-child(2)`. A worked example that matches
   the live shape will land harder than one with synthetic
   class names.
3. **Add an "if this fails" decline-vs-multi-leaf rubric** that
   names the v1.18-era apply failure ("inner selector matched
   no elements") as a signal the binding should be multi-leaf
   rather than single-leaf.
4. **Re-run the hurricane 5-trial** after v1.20 lands. Promotion
   to Accepted is gated on ≥1 `extracted_inner` recipe authored
   across the 5 trials.

This is the lowest-cost path with the highest expected leverage.
Estimated 1 session of work.

### B. Fixture-based integration test for ADR 0019

A complementary direction that doesn't depend on LLM behaviour.
Hand-author a multi-leaf recipe against a known multi-field
fixture page (e.g. a synthetic HTML storm-list page matching the
v1.19 worked example), apply it through the full pipeline, and
confirm the runtime produces multi-leaf records end-to-end with
correct dedup keys, normalized envelopes, and per-FieldMap
extracted leaves. The runtime tests added in Session 61 cover
this at the unit level; a fixture-based integration test
extends the coverage to the apply-stage normalize layer and
storage write-through.

Useful even if A succeeds: gives Session 62+ a regression test
for multi-leaf at the integration level.

### C. Recipe-author source-shape inspection

The hurricane decline reasons reveal a recurring pattern: the
recipe-author looks at a structured page and concludes "no
per-storm events" because no individual leaf carries a complete
English sentence headline. But the page often *does* carry
extractable per-row data; the LLM just can't see it as Events
because it's looking for headline-shaped leaves.

A v1.20 prompt revision could add a "structured-data recognition
checklist" that walks the LLM through "does this page have a
table or list with N rows? Does each row carry several extractable
fields? If yes, this is a multi-leaf opportunity even if no row
has a single 'headline' leaf — author Events with a synthesized
headline from name + date."

This is adjacent to A but goes further: it changes the LLM's
default reading of structured pages, not just its authoring
shape when it recognises multi-leaf. Higher cost, higher upside.

## Sequencing

```
Session 62: pick A, B, or C. A is the lowest-cost path to ADR
            0019 Accepted; B is the orthogonal-coverage
            complement; C is the higher-leverage but higher-cost
            prompt overhaul.
Session 63+: ADR 0019 Phase 2B (csv_cell / pdf_table /
             regex_capture) once Phase 2A has a settled live
             baseline. Recipe-author classifier prompt
             investigation if post-A residual obs-bias persists.
```

ADR 0018 is on the validated path forward; ADR 0019 needs one
more prompt-iteration cycle to flip to Accepted.

## Discipline (carried forward)

- **Closed-vocabulary discipline preserved.** ADR 0019 added one
  `FieldValueSource` variant; the five extraction modes stay five.
  v1.19 prompt teaches the new shape principle-only, no
  source-specific routing.
- **No easy wins.** The temptation after the Session 61 hurricane
  re-run is to roll back bucket fairness because lithium records
  regressed. The handoff names this as the ADR-predicted trade-
  off and points Session 62 at the apply-stage success rate, not
  at the dispatch-order rollback.
- **Schema-first.** ADR 0019's runtime additions
  (`ExtractedInner`, `compute_inner_extractions_*`,
  `extract_json_within`, `apply_json_iterator`) compose with the
  existing types — no new content types, no new extraction
  modes, no new RecordType variants.
- **Memory updated.** Session 61's findings live in
  `spaces/c19dac53-…/memory/project_sr_session_61_*.md`. MEMORY.md
  index updated to mark Session 60's two-gates framing as
  superseded (the ADRs landed) and to point at Session 62's
  prompt-iteration direction.
- **`--keep-dbs` retained.** Per-trial DBs are at
  `/var/folders/rs/…/situation_room-eval-019e17…/trial-N.duckdb`
  for both the hurricane and lithium runs; the JSONL files
  record the exact paths so Session 62 can inspect any outlier
  trial's records or recipes directly.

## Cleanup / state

- **8 files committed** in the rsync'd "before session 62"
  snapshot:
  - `crates/pipeline/src/fetch_executor.rs` (ADR 0018 round-
    robin + cap=6 + 5 tests)
  - `crates/pipeline/src/recipes.rs` (`ExtractedInner` variant +
    round-trip test)
  - `crates/pipeline/src/recipe_author.rs` (`AuthoredFieldValueSource::ExtractedInner`,
    `convert_field_map`, 4 validator rules, 5 validator tests,
    `extraction_mode_name` helper)
  - `crates/pipeline/src/recipe_apply.rs` (ExtractedInner runtime
    end-to-end: helpers, scalar+multi-leaf, json iterator,
    extended shape validator, 3 runtime tests)
  - `config/prompts/recipe_author.md` (v1.19 multi-leaf section
    + worked examples + new "What NOT" entries + changelog)
  - `apps/desktop/src/components/RecordsDashboard.svelte`
    (aspirational note refreshed to "gates open" framing)
  - `docs/adr/0018-target-bucket-fairness.md` (Accepted + post-
    fix validation section + lithium-regression honesty)
  - `docs/adr/0019-per-field-extraction-subspecs.md` (Proposed-
    with-Phase-2A-landed + what-shipped + what's-not-working +
    Session 62 path-to-Accepted)
- **`cargo test --workspace` green**: 792 tests passed, 0 failed,
  13 ignored (live-network tests, unchanged). Up from 786 in
  Session 60 — net +6 tests from this session's additions.
- **`npm run check` clean**: 0 errors, 0 warnings (one stray
  CSS selector removed after the dashboard note refresh).
- **2 new eval JSONLs** in `apps/desktop/eval-runs/`:
  - `2025-atlantic-hurricane-season-20260511T160839Z.jsonl` —
    5 trials, 2 records produced, 193 per-target authorings
    dispatched (vs. Session 59's 13 dispatched).
  - `lithium-production-20260511T162844Z.jsonl` — 5 trials, 7
    records produced (vs. Session 58's 10), mean 1.4 records/
    trial.

End of handoff.
