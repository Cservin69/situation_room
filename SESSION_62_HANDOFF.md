# Session 62 — Handoff

Session 62 picked up where Session 61 left ADR 0019: the type +
validator + runtime + v1.19 prompt all shipped in Session 61 but
across 10 trials the LLM authored zero `extracted_inner` recipes.
This session combined all three candidate directions from Session
61's handoff (A — v1.20 prompt iteration; B — fixture-based
integration test; C — recipe-author source-shape inspection) into
one push. Everything except the live-data validation landed.

## What Session 62 changed

### A + C — v1.20 recipe-author prompt

[`config/prompts/recipe_author.md`](config/prompts/recipe_author.md)
v1.20. Four sub-pieces, prompt-only, no schema or Rust changes:

- **20A — multi-leaf section reframing.** The "Multi-leaf records"
  section gains an opening paragraph that names single-leaf-vs-
  multi-leaf as "the single most consequential decision in
  iterator-bearing recipes." The v1.19 placement (between mode-
  selection and the decline path) was visible but unsignposted;
  the v1.20 opener forces the LLM to consider multi-leaf as a
  decision point rather than an obscure option. Path A item 1
  from the Session 61 handoff.
- **20B — "Is this row multi-leaf?" recognition checklist.** New
  subsection above the shape description that walks the LLM
  through four explicit questions (does the listing have N rows?,
  per row how many extractable leaves?, does the record need
  more than one?, is there a single concatenated leaf that
  would lose structure?). Attacks the Session 61 hurricane
  decline pattern where the LLM looked at structured pages and
  concluded "no per-storm events" because no leaf carried a
  complete English-sentence headline. Path C from the Session 61
  handoff, integrated rather than orthogonal.
- **20C — worked example with positional selectors.** Third
  worked example added: a `tr.row` iterator + `td:nth-child(N)`
  per-leaf selectors. The v1.19 worked examples used synthetic
  semantic class names (`tr.ownership-row`, `td.from-slug`); the
  new example states "a class-bearing iterator + positional
  inner selectors covers the common shape where the listing has
  table-level identification but no cell-level semantics" and
  names positional selectors as first-class. Path A item 2 from
  the Session 61 handoff, framed principle-only per the closed-
  vocabulary discipline (no host strings, no source names).
- **20D — apply-time signals that meant you should have authored
  multi-leaf.** New subsection after the worked examples that
  names three specific validator/runtime error messages — "inner
  selector matched no elements within iterator match," "selector
  matches a container element instead of a leaf" with iterator
  present, and validator rule (iii)'s all-literal-binding
  rejection — as retry signals that indicate the previous
  attempt was single-leaf when it should have been multi-leaf.
  Path A item 3 from the Session 61 handoff.

The full Session 62 changelog entry sits at the top of the
prompt's Changelog section with sub-piece-level motivation and
empirical hypothesis for Session 63.

### B — fixture-based integration test for ExtractedInner

[`crates/pipeline/src/normalize.rs`](crates/pipeline/src/normalize.rs)
gains two new tests in the `tests` module that exercise `apply`
composed with `finalize` — the same call chain the production
pipeline uses. Extends ADR 0019 coverage from the apply()
boundary (already covered by Session 61's runtime tests) to the
apply-stage normalize layer:

- `adr_0019_multi_leaf_position_only_table_applies_and_finalizes_end_to_end`
  hand-authors a multi-leaf css_select recipe with positional
  inner selectors (`td:nth-child(N)`) against a three-row HTML
  fixture. Asserts (a) three records produced, (b) per-row
  headline + direction extracted leaves, (c) topic tags from the
  plan reach every record's envelope via finalize's
  `merge_topics`, (d) per-row dedup_key resolves to
  `{recipe.id}:{headline}` through the ExtractedInner path.
- `adr_0019_multi_leaf_preserves_record_shape_after_finalize`
  pins that a multi-leaf Event recipe finalises to an Event,
  catching accidental shape regressions in finalize's
  envelope-mut match when a future change touches it.

The integration test gives ADR 0019 a regression guard
independent of LLM behaviour: if a future prompt iteration
causes the recipe-author to stop authoring `extracted_inner`,
the type+validator+runtime+normalize composition still has CI
proof of correctness on the multi-leaf path.

### ADR 0019 update

[`docs/adr/0019-per-field-extraction-subspecs.md`](docs/adr/0019-per-field-extraction-subspecs.md)
adds a "What Session 62 added" subsection documenting the v1.20
prompt + fixture-integration test landing, and revises the
path-to-Accepted from "Session 62" to "Session 63" — the live
hurricane re-run with the v1.20 prompt is the empirical gate.
The two follow-on directions (reasoning-block-before-JSON,
recipe-iteration-on-FetchReport) stay pre-staged in case v1.20
doesn't move the rate.

## Path to Accepted — Session 63's gate

Promotion of ADR 0019 to Accepted requires the live re-run with
the v1.20 prompt to produce:

| Signal      | Threshold                                         | Status as of Session 62 |
|-------------|---------------------------------------------------|-------------------------|
| Sufficient  | ≥1 `extracted_inner` recipe across 5 trials       | unmeasured              |
| Stronger    | ≥3 Event records/trial with distinct headline+date| unmeasured              |

Session 61 produced 0 `extracted_inner` recipes across 10 trials
under v1.19. The v1.20 hypothesis is that the recognition
checklist + positional-selector worked example + apply-time-
signal subsection together shift the rate from 0/10 to ≥1/5 on
the hurricane re-run.

If v1.20 doesn't move the rate, the prompt-engineering ceiling
on classifier output shape may be closer than the v1.20
hypothesis suggests. Two pre-staged follow-ons in the ADR:

1. **Reasoning-block-before-JSON experiment** — let the LLM
   write a freeform analysis of the prefetch before the
   structured-output recipe JSON. The recognition checklist
   would run as visible reasoning rather than as latent
   token-distribution shifting.
2. **Recipe-iteration-on-FetchReport loop** (Session 60's
   candidate A) — when a single-leaf recipe fails at apply
   with "inner selector matched no elements," automatically
   re-author against the retry excerpt with the failure message
   inline as a multi-leaf signal.

## Discipline preserved

- **Closed-vocabulary preserved.** The v1.20 worked example
  teaches positional inner selectors (`td:nth-child(N)`) without
  naming any host or source. The Session 61 handoff's note about
  the TCR page's class structure was treated as a *pattern* (real
  HTML tables often lack semantic per-cell classes) rather than
  a routing rule — no source-specific recipe authoring guidance
  in the prompt.
- **No easy wins.** The temptation after Session 61 was to roll
  back bucket fairness because lithium records regressed
  (2.0 → 1.4 records/trial). Session 62 stayed on the
  ADR-predicted trade-off — the regression is the documented
  cost of opening the non-Observation buckets — and put effort
  into apply-stage success rate (v1.20 prompt + fixture test)
  rather than dispatch rollback.
- **Schema-first.** v1.20 is prompt-only: no schema change, no
  Rust change, no new validator rules. The Session 61 schema
  surface (`FieldValueSource::ExtractedInner`, four validator
  rules, Phase 2A runtime) is unchanged.
- **Memory updated.** Session 62's findings live in
  `spaces/c19dac53-…/memory/project_sr_session_62_*.md`.
  MEMORY.md index updated to mark Session 61's verification
  pointer as superseded (the v1.20 prompt + fixture test
  landed) and to point at Session 63's hurricane re-run as the
  empirical gate.

## Cleanup / state

- **3 files committed** in the rsync'd "before session 63"
  snapshot:
  - `config/prompts/recipe_author.md` (v1.20 multi-leaf
    reframing + recognition checklist + positional-selector
    worked example + apply-time-signal subsection + changelog)
  - `crates/pipeline/src/normalize.rs` (two new fixture-
    integration tests + supporting imports)
  - `docs/adr/0019-per-field-extraction-subspecs.md`
    (Session 62 "What we added" subsection + Session 63
    "path to Accepted" gate + status date updated)
- **`cargo test --workspace` green**: 794 tests passed, 0 failed,
  14 ignored (live-network tests, unchanged). Up from 792 in
  Session 61 — net +2 tests from the two new normalize fixture-
  integration tests for ExtractedInner.
- **`npm run check` clean**: 0 errors, 0 warnings.

## Sequencing

```
Session 63: run the hurricane 5-trial with the v1.20 prompt.
            If ≥1 extracted_inner recipe is authored → ADR 0019
            flips to Accepted, dashboard events/relations panels
            populate live. If 0/5, escalate to reasoning-block
            experiment or recipe-iteration-on-FetchReport loop.
Session 64+: Phase 2B (csv_cell / pdf_table / regex_capture)
             once Phase 2A has a settled live baseline.
```

End of handoff.
