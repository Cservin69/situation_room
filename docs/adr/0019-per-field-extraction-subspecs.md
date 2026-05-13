# ADR 0019 — Per-field extraction sub-specs: ADR 0016 Phase 2 for multi-leaf record types

**Status**: Accepted (Session 64). Sufficient condition met on the
2026-05-12 v1.20 hurricane re-run — 2 of 5 trials authored a recipe
with `FieldValueSource::ExtractedInner` (vs Session 61's 0/10). The
stronger signal (≥3 Event records per trial via the multi-leaf path)
remains unmet and is tracked as a follow-on improvement target
rather than a blocker on acceptance.
**Date**: 2026-05-11 (proposed); 2026-05-11 (Phase 2A implementation landed); 2026-05-11 (v1.20 prompt + fixture-integration test landed); 2026-05-12 (Accepted on Session 64 hurricane eval); 2026-05-13 (Session 67 closed structural-validator coherence gap on json_path × json_path)
**Related**: ADR 0003 (six record types as governance boundary), ADR
0007 (research function: two-level LLM architecture and the
closed-extraction-vocabulary discipline), ADR 0016 (extraction
iterator: Phase 1, single extracted field per match — explicitly
defers multi-field-per-match to its own ADR), ADR 0018 (target-
bucket fairness)

---

## Context

The closed-vocabulary discipline (ADR 0007 §"closed extraction
vocabulary") fixes the recipe shape at:

- one extraction mode per recipe (`css_select`, `json_path`,
  `csv_cell`, `pdf_table`, `regex_capture`),
- one extracted scalar per recipe, possibly iterated (ADR 0016 added
  an `iterator` field that selects N nodes; the existing
  `extraction` field still returns one leaf per node).

Per the closed-vocabulary discipline, `ProductionBinding.field_mappings`
is a `Vec<FieldMap>`, but every `FieldMap` whose source is
`FieldValueSource::Extracted` receives **the same single scalar** —
the one value produced by the recipe's `extraction`. Other
`FieldValueSource` variants — `Literal` and `FromPlan` — fill the
remaining fields from static recipe content or from the plan, not
from the source bytes.

This shape carries the four record types differently:

| Record type        | Required extracted fields                                | Authorable today? |
|--------------------|----------------------------------------------------------|-------------------|
| `Observation`      | `value` (numeric)                                        | Yes — `metric` and `unit` are typically Literal or FromPlan |
| `Event`, headline-only | `headline` (string)                                  | Yes — `event_type` and `direction` are Literal |
| `Event`, with magnitude | `headline`, `magnitude.value`, `magnitude.unit`     | **No** — two extracted leaves per record |
| `Relation`         | `from` (EntityId), `to` (EntityId)                       | **No** — two extracted leaves per record |
| `EntityAttribute`  | `entity_id`, `value` (when typed-AttributeValue varies)  | **No** — two extracted leaves per record |

ADR 0016 §"Phase 2 (deferred — separate ADR)" called this out
explicitly:

> When real listings need multiple extracted leaves per record
> (headline + date + author from one news card, title + abstract +
> arxiv-id from one paper card), the field_mappings need per-field
> extractor sub-specs. This is a richer change that touches recipe
> author validation, the runtime, the prompt, and the UI's recipe
> inspection panel. It deserves its own ADR after Phase 1's contract
> has run in production for a few cycles.

Phase 1 has now run in production for 22 sessions (38 → 59). ADR
0018 (target-bucket fairness) opens dispatch to the non-Observation
buckets; the dispatched authoring calls against Relations and
magnitude-bearing Events will hit the multi-leaf gate on every
non-trivial source. This ADR is the matching gate-opening.

### Why the iterator-with-inner-selector pattern is not enough

Session 59's hurricane run included two `www.nhc.noaa.gov` recipes
that reached the apply stage and failed with:

> inner selector matched no elements within iterator match … the
> inner selector is targeted at a sibling rather than a descendant.

The recipes were authored under ADR 0016 Phase 1: one `iterator`
(`css_select` over storm-list rows) and one `extraction` (the
inner selector for one leaf — presumably `.storm-name a` or
similar). The pattern works when each iterated card contains
exactly one leaf the binding wants. It fails on a storm-list page
because the LLM tried to encode a multi-field event into a single-
leaf binding and the validator forced an awkward shape.

Phase 1's prompt currently teaches the LLM to author *one*
extracted field per binding under iterator mode, with all other
per-record fields as literals. For Events with a headline-only
shape (Nature subjects, qt.eu newsroom — ADR 0016's worked
examples) this is fine. For Events whose row carries date +
headline + magnitude, the constraint forces the LLM into one of
three bad shapes:

1. **Drop the per-row variation.** Encode the date as a literal
   (always wrong) or omit it (loses the timeline).
2. **Concatenate at extraction.** Use a `css_select` that returns
   the row's text content as one long string and rely on
   downstream parsing (no downstream parsing exists; the apply
   layer's 2048-byte scalar cap rejects).
3. **Decline.** The honest outcome under v1.18's discipline, and
   the one Session 59 observed twice on hurricanes.

The closed vocabulary is doing its job — declining shapes it can't
represent — but the vocabulary is genuinely under-expressive for
the topic shapes that drove the operator to broaden away from
lithium in the first place.

### What this is *not*

This ADR does not relax ADR 0007's golden rule (runtime is LLM-
free once recipes exist). Per-field sub-specs are still authored
once and applied deterministically; there is no per-record LLM
call.

This ADR does not expand the closed extraction-mode enum. The five
modes stay five. The change is in how `FieldMap` references them.

This ADR does not address pagination, anti-bot/WAF, or cross-page
joining. Each remains a separate concern, deferred to other ADRs.

This ADR does not change the recipe-authoring boundary. One LLM
call per (plan × nomination × target_expectation) — the Session 47
contract — remains the unit of authoring.

## Decision

Extend `FieldValueSource` with a fourth variant,
**`ExtractedInner { spec: ExtractionSpec }`**, that carries a
per-field extraction sub-spec. The runtime evaluates the sub-spec
against the same per-match scope the binding's outer extraction
operates on, producing one leaf per FieldMap per match.

### The shape

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldValueSource {
    /// Existing. The recipe's outer extraction result.
    Extracted,
    /// Existing. A literal value baked into the recipe.
    Literal { value: serde_json::Value },
    /// Existing. A value taken from the session's ResearchPlan.
    FromPlan { pointer: String },
    /// NEW (ADR 0019). A per-field extraction sub-spec evaluated
    /// against the same scope as the binding's outer extraction.
    /// The sub-spec's mode must be congruent with the binding's
    /// outer mode (CSS pairs with CSS, JSONPath with JSONPath,
    /// etc. — same congruence rule as ADR 0016).
    ExtractedInner { spec: ExtractionSpec },
}
```

The fourth variant is purely additive. Existing recipes deserialize
unchanged because no existing `FieldMap` carries the new variant;
the validator's exhaustiveness check picks it up where the four
match arms exist today.

### Semantics, by recipe shape

**Scalar recipe, single-leaf record.** `iterator = None`, one
binding with one `Extracted` FieldMap. Today's contract,
unchanged.

**Scalar recipe, multi-leaf record (the new case).** `iterator =
None`, one binding with N `ExtractedInner` FieldMaps. The outer
`extraction` field is still required (validator rejects null) and
provides a "scope" for the inner sub-specs: in `css_select` mode
the outer selector resolves to a DOM node, and each
`ExtractedInner` sub-selector applies within that node's sub-tree.
In `json_path` mode the outer path resolves to a JSON value and
each inner path applies relative to it.

A worked example. The NHC storm-list page has one row per named
storm with date, name, and category fields. The recipe under this
ADR:

```json
{
  "extraction": { "mode": "css_select", "selector": "table.storms" },
  "iterator":   { "mode": "css_select", "selector": "tr.storm-row" },
  "produces": [{
    "expectation": { "list": "event_type", "index": 0 },
    "record_type": "event",
    "dedup_key_field": "headline",
    "field_mappings": [
      {
        "path": "event_type",
        "source": { "kind": "literal", "value": "storm_formed" }
      },
      {
        "path": "headline",
        "source": { "kind": "extracted_inner",
                    "spec": { "mode": "css_select",
                              "selector": "td.storm-name" } }
      },
      {
        "path": "valid_at",
        "source": { "kind": "extracted_inner",
                    "spec": { "mode": "css_select",
                              "selector": "td.storm-date" } }
      },
      {
        "path": "direction",
        "source": { "kind": "literal", "value": "supply_negative" }
      }
    ]
  }]
}
```

The outer `extraction` (`table.storms`) selects the table; the
`iterator` selects each row; per row, the headline sub-selector
extracts the storm name and the valid_at sub-selector extracts the
date. The recipe produces N Event records, one per row, each with
multiple extracted leaves.

**Iterator recipe, multi-leaf record (the dominant motivating case).**
`iterator = Some(_)`, one binding with N `ExtractedInner` FieldMaps.
Per match of the iterator, each inner sub-spec applies within the
match's scope. This is the storm-list case above.

**Mixed sources (typical).** Bindings can freely combine `Literal`,
`FromPlan`, `Extracted`, and `ExtractedInner` per field. The
NHC example above mixes Literal (event_type, direction) with
ExtractedInner (headline, valid_at). A Relation binding might use
ExtractedInner for `from` and `to` and Literal for `kind`.

### Validation rules

The recipe-author validator (`build_validated_recipe` in
`crates/pipeline/src/recipe_author.rs`) gains four new rules:

1. **Mode congruence.** Every `ExtractedInner.spec.mode` must equal
   the recipe's outer `extraction.mode`. Cross-mode (a `css_select`
   inner inside a `json_path` outer) is rejected — same congruence
   rule as ADR 0016's iterator-vs-inner-selector check.

2. **`Extracted` and `ExtractedInner` are mutually exclusive per
   binding.** A binding either uses the legacy single-scalar
   `Extracted` (with literals/plan vars for the rest) or uses N
   `ExtractedInner` sub-specs (with literals/plan vars for the
   rest). Mixing the two in one binding is rejected — the runtime
   would have to pick whether `Extracted` means "the outer extraction
   result" or "an inner sub-spec," and the v1.20 prompt teaches the
   LLM to commit to one shape per binding.

3. **At least one extraction reaches the bytes.** A binding with
   zero `Extracted` and zero `ExtractedInner` FieldMaps (all
   Literal + FromPlan) was a pre-existing degenerate shape that
   the v1.18 prompt's required-field discipline already declines
   for record types whose required fields are extraction-only. The
   validator made this rule implicit; this ADR makes it explicit
   so the multi-field case has a clean contract: a binding must
   bind at least one field from the source.

4. **`ExtractedInner` requires an outer `extraction`.** When any
   FieldMap in any binding uses `ExtractedInner`, the recipe's
   outer `extraction` field must be present and meaningful (not a
   degenerate body-selector). The outer extraction defines the
   per-match scope the inner sub-specs apply within. Today's
   validator already requires `extraction` to be non-null; this
   rule reinforces that the value must be a real selector when
   inner sub-specs are present.

### Runtime semantics

`recipe_apply` gains one helper: `apply_inner_extraction(spec,
scope_bytes)` that runs an `ExtractionSpec` against a scope (a DOM
sub-tree, a JSON value, etc.) and returns one scalar. The existing
`apply_extraction` is unchanged for legacy recipes; the
multi-field path calls `apply_inner_extraction` once per
`ExtractedInner` FieldMap, accumulates the leaves into the record
content, and dispatches to the existing `RecordContent` validator.

The runtime stays LLM-free. The inner sub-spec is a pre-authored
deterministic spec like the outer extraction.

### Dedup-key derivation

Multi-field iterator recipes still require
`ProductionBinding::dedup_key_field` (per ADR 0016). The named
path now resolves to one of the extracted leaves (typically
`headline` or a stable identifier field), with the same
`{recipe.id}:{field_value}` dedup-key shape.

For non-iterator multi-field recipes (one record per fetch with
multiple extracted leaves), dedup_key_field stays optional, same
as the scalar non-iterator path.

## Two-phase rollout

### Phase 2A (Session 61) — `css_select` + `json_path`

Implement `ExtractedInner` end-to-end for the two extraction modes
that produce the dominant share of listings:

- `css_select`: per-DOM-subtree inner extraction. The cold-start
  motivating case (NHC storm rows, Reuters article cards with
  date+headline, arXiv listings with title+abstract).
- `json_path`: per-JSON-value inner extraction. APIs that return
  arrays of objects (FRED observation series, World Bank API
  responses, FEMA disaster declarations) where each object's
  fields map to different record fields.

The other three modes (`csv_cell`, `pdf_table`, `regex_capture`)
defer to Phase 2B. PDF tables in particular need a richer per-row
column-mapping concept that doesn't slot cleanly into
`ExtractedInner { spec }` — the spec's `pdf_table.row` field is
already the per-iteration selector, leaving only `col` for inner
variation. A first-cut Phase-2A `pdf_table` `ExtractedInner` would
just be a per-FieldMap column index; that's tractable but worth
its own validation pass.

### Phase 2B (Session 62+, separate session) — remaining modes

`csv_cell`, `pdf_table`, `regex_capture`. The CSV case generalizes
the existing `row_filter` mechanism (one filter selects rows; per
row, each FieldMap names a column). The PDF case adds per-FieldMap
column indices to the existing `pdf_table.row` iterator. The regex
case uses per-FieldMap capture group indices against a shared
outer pattern.

These are mechanically simpler than the CSS / JSON cases but the
fixture corpus is thinner, so the validation surface needs more
empirical input before locking the contract.

### Prompt revision (Session 61, v1.19)

The recipe-author prompt's v1.19 revision adds:

- A new subsection "Multi-leaf records — when one row carries
  several fields" between v1.18's "Selecting the mode that fits…"
  and the decline-path section. Teaches the LLM to recognise the
  multi-leaf case (event with date + headline; relation with
  from + to entity slugs) and to author `ExtractedInner` sub-specs
  rather than concatenating or declining.
- A worked example pair: NHC storm-row → multi-field event;
  arXiv recent-listings → multi-field document with title +
  abstract + arxiv-id.
- The "What NOT to produce" list gains entries for the new
  failure modes the v1.19 validator catches (cross-mode
  congruence, Extracted-and-ExtractedInner-mixed-in-one-binding,
  ExtractedInner-without-outer-extraction).

## Consequences

### Positive

- Magnitude-bearing events, relations, and entity-attributes
  become authorable. Combined with ADR 0018 (target-bucket
  fairness), the path from plan to records for the five non-
  Observation typed panels is structurally complete.
- The closed extraction-mode enum stays at five. The change is
  in how `FieldMap` references them — additive in shape, not in
  vocabulary.
- ADR 0007's two-level LLM architecture is preserved. One LLM
  call per (plan × nomination × target) authors the recipe with
  N inner sub-specs; the runtime applies all inner sub-specs
  deterministically per match.
- Existing recipes are unaffected. `Extracted | Literal |
  FromPlan` continues to deserialise and apply identically. New
  recipes that don't need multi-leaf extraction don't use
  `ExtractedInner`.

### Negative / costs

- Validator surface widens by four rules (mode congruence,
  mutual exclusion, extraction-bytes-required,
  outer-extraction-required-with-inner). Each rule is a unit-
  testable structural check; the test count rises by ~12 across
  these four invariants.
- Recipe-author prompt grows a section. v1.19 carries the new
  guidance plus a worked example pair, raising the prompt's
  bound-relevant byte count by ~3 KiB. Still well inside
  `Bounds::LLM_PROMPT_BODY` (256 KiB).
- The LLM gains a new authoring shape to learn. The Session 59
  hurricane decline was the LLM's honest "I can't represent
  this" signal under the v1.18 contract; under v1.19 it learns
  to reach for `ExtractedInner`. Empirically the rate at which
  the LLM picks the right shape is unknown until Session 61's
  first multi-field eval — anticipated to need 1–2 prompt
  iterations to settle.
- Inner sub-specs are unbounded in count per binding. A binding
  could in principle declare 50 `ExtractedInner` FieldMaps. The
  validator imposes a per-binding cap (suggested: 8) consistent
  with the existing `MAX_BINDINGS` discipline; surfacing the cap
  in the prompt as the maximum reasonable shape.
- Failure-shape attribution becomes more nuanced. Today an apply
  failure on a binding is "this single extraction returned X."
  Under multi-field, an apply failure could be "this inner
  sub-spec for `valid_at` returned X while the others succeeded."
  The failure-stage record needs a per-FieldMap dimension; the
  existing `apply_failures_for_nomination` storage row supports
  the addition without a migration (the message column is text).

### Alternatives considered

**(a) Per-binding extraction.** Make `extraction` per-binding
rather than per-recipe; each binding carries its own outer
extraction and per-field-map literals. Equivalent in expressive
power but heavier validator surface: now the binding has two
extraction surfaces (outer + inner) instead of one (FieldMap-level
sub-specs sharing the recipe's outer scope). Rejected for
asymmetry — keeping `extraction` per-recipe as a shared scope
preserves the iterator's compose-with-extraction story.

**(b) A new "multi-record" extraction mode.** Add a sixth mode
that returns `Vec<HashMap<String, Value>>` (one entry per row,
one map per row, keyed by field name). Cleaner shape at the
recipe level, but breaks ADR 0007's closed-vocabulary
discipline at the type level: the mode would have a
fundamentally different return type from the other five
(scalar). Rejected as a violation of the vocabulary uniformity
principle.

**(c) Encode multi-field as multiple recipes per nomination.**
One recipe per FieldMap (one for headline, one for date, etc.),
all sharing a nomination and joined post-fetch by row index.
Multiplies recipe count by per-record-field count and forces
post-fetch joining into the runtime. Rejected for compositional
complexity — the joining logic is essentially "iterate the
shared scope and zip the extractions," which is what
`ExtractedInner` does in a single recipe.

**(d) Defer multi-field to a third extraction layer.** Add a
new abstraction between recipe-authoring and record-emission:
"row schemas" that map per-row positions to record fields.
Heavier than the proposal. Worth revisiting if Phase 2A surfaces
authoring patterns the per-FieldMap sub-spec shape can't
capture, but Phase 2A's CSS and JSON cases fit `ExtractedInner`
cleanly.

### Carry-forward dependencies

Two storage/observability surfaces should pick up the multi-field
shape in Session 61's implementation pass:

1. **`fetch_run_outcomes`'s decline messages** currently quote a
   single extraction's failure. Under multi-field, the message
   needs to identify which FieldMap failed: `"observation_metric:1
   field 'headline' (ExtractedInner css_select): selector matched
   no elements"`. Text-column-level change; no migration.
2. **The RecipesPanel UI** (`apps/web/src/lib/components/
   RecipesPanel.svelte`) renders one extraction line per recipe.
   Multi-field recipes need a per-FieldMap row in the inspection
   view. Pure UI change; the API surface already returns the full
   recipe JSON.

## Validation

This ADR is empirically falsifiable on the Session 59 hurricane
re-run. Pre-ADR-0019: hurricane NHC storm-list recipes apply-fail
with "inner selector matched no elements within iterator match."
Post-ADR-0019 with ADR 0018 also landed: a hurricane plan should
produce ≥3 Event records per trial on NHC sources, each with
distinct `headline` (storm name) and `valid_at` (formation date)
extracted leaves — and the dashboard's `events` panel should
populate.

A secondary validation: a fresh "global semiconductor exports
2024" topic, classified to populate `event_types`
(`export_control_enacted`, `contract_signed`, etc.) and
`relation_kinds` (`supplies_to`, `subject_to_sanction`), should
produce Relation records when ADR 0019 lands. Relations are the
canonical multi-field case (`from` + `to`); a populated Relations
panel post-implementation is the strongest signal that the gate
is open.

## Status

**Proposed** (2026-05-11, Session 60). **Phase 2A implementation
landed in Session 61** — type, validator, runtime, and prompt are
all in place and tested. The ADR remains **Proposed** rather than
Accepted because the live-data validation criterion ("≥3 Event
records per trial on NHC sources, each with distinct `headline`
and `valid_at` extracted leaves") is **unmet**: across 10 trials
(5 hurricanes + 5 lithium) under the v1.19 prompt, the LLM
authored zero recipes using `extracted_inner`. Promotion to
Accepted is gated on a future session's live-validated multi-leaf
record extraction.

### What Phase 2A shipped (Session 61)

- **Type layer** (`crates/pipeline/src/recipes.rs`):
  `FieldValueSource::ExtractedInner { spec: ExtractionSpec }`
  variant added, serde-tagged as `"kind":"extracted_inner"` with
  the nested spec serialising as the same five-mode closed enum
  used everywhere else. Round-trip tests pin the wire shape.
- **Validator layer** (`crates/pipeline/src/recipe_author.rs`):
  Four rules added to `build_validated_recipe` — (i) inner-spec
  mode congruence with outer extraction, (ii) Extracted /
  ExtractedInner mutual exclusion per binding, (iii) at-least-one
  extraction per binding, (iv) Phase-2A runtime-support gate
  (css_select / json_path only; csv_cell / pdf_table /
  regex_capture remain Phase 2B). Five validator tests cover the
  happy path and each rejection path.
- **Runtime layer** (`crates/pipeline/src/recipe_apply.rs`):
  `compute_inner_extractions_css/json` helpers, `extract_json_within`
  for per-scope JSONPath evaluation, `apply_json_iterator` for the
  new (JsonPath, JsonPath) iterator pair, scalar+multi-leaf
  resolution via `scalar_inner_extractions`. The shape validator
  (`validate_recipe_shape_against_bytes`) extended to evaluate
  inner sub-specs against the real per-match scope rather than a
  stub. Three runtime tests cover css iterator + multi-leaf, json
  iterator + multi-leaf, and legacy iterator backwards compat.
- **Prompt layer** (`config/prompts/recipe_author.md` v1.19):
  New "Multi-leaf records — when one row carries several fields"
  section between "Selecting the mode" and the decline-path
  section. Two worked examples — relations from an ownership-table
  listing (CSS) and events from a JSON listing API (JSONPath).
  Five new "What NOT to produce" entries flag the four validator
  rejection modes plus the all-literal binding case.

### Why the LLM didn't reach for `extracted_inner` yet

The Session 61 hurricane / lithium re-run is the first real
contact between the v1.19 prompt and live sources. Observed
patterns across 10 trials:

- **The recipe-author rejected the multi-leaf framing on every
  source touched.** On NHC sources, the LLM either declined the
  EventType target outright ("source provides aggregate statistics
  not extractable per-storm events") or authored a legacy
  single-leaf iterator+inner-selector recipe that subsequently
  failed at apply ("inner selector matched no elements within
  iterator match" — twice across the hurricane trials, same shape
  Session 60 documented). The v1.19 prompt's multi-leaf worked
  example uses synthetic class names (`tr.storm-row`,
  `td.storm-name`) but no live source the LLM encountered carried
  cleanly identifiable per-row classes.
- **Validator rule (iii) caught three separate all-literal
  bindings** — the LLM authored bindings whose every FieldMap was
  Literal or FromPlan (a degenerate shape that would emit a
  constant record on every fetch). These would have run forever
  pre-fix; ADR 0019's validator now rejects them at authoring
  time. So Phase 2A has a real safety effect even before the
  positive case lands.
- **Phase 2B modes (csv_cell, pdf_table, regex_capture) are
  closed off cleanly.** The validator's rule (iv) explicitly
  rejects ExtractedInner with these modes, so the LLM doesn't
  silently land on unsupported shapes — the gate is consistent
  with what the runtime implements.

### What Session 62 added (the path-to-Accepted infrastructure)

Session 62 landed both prompt iteration and the
fixture-integration test described in the previous revision of
this section. Live re-run with the v1.20 prompt is deferred to
Session 63.

**v1.20 prompt** (`config/prompts/recipe_author.md`). Four
sub-pieces, all prompt-only — no schema change, no Rust change:

- **20A — multi-leaf section moves to the front of its
  subject area.** The "Multi-leaf records" section gains an
  opening framing that names single-leaf-vs-multi-leaf as "the
  single most consequential decision in iterator-bearing
  recipes." The v1.19 placement (between mode-selection and the
  decline path) was visible but unsignposted; the v1.20 opener
  forces the LLM to consider multi-leaf as a decision point
  rather than an obscure option.
- **20B — "Is this row multi-leaf?" recognition checklist.**
  New subsection above the shape description that walks the LLM
  through four explicit questions: does the listing have N
  rows?, per row how many extractable leaves?, does the record
  need more than one?, is there a single concatenated leaf that
  would lose structure? Designed to attack the Session 61
  failure mode where the LLM looked at structured pages and
  concluded "no per-storm events" because no leaf was a
  complete English-sentence headline — the pages did carry
  extractable per-row data, just not in headline-shaped leaves.
- **20C — worked example with positional selectors.** Third
  worked example added: a `tr.row` iterator + `td:nth-child(N)`
  per-leaf selectors. The v1.19 worked examples used
  synthetic semantic class names (`tr.ownership-row`,
  `td.from-slug`) that don't transfer to listings with no per-
  cell classes — the dominant real-world shape. The new
  example states "a class-bearing iterator + positional inner
  selectors covers the common shape where the listing has
  table-level identification but no cell-level semantics" and
  explicitly names positional selectors as first-class. Closed-
  vocabulary discipline preserved: no host strings, no source
  names; the pattern is general.
- **20D — apply-time signals that meant you should have
  authored multi-leaf.** New subsection naming three specific
  validator/runtime error messages as retry signals that
  indicate the previous attempt was single-leaf when it should
  have been multi-leaf: "inner selector matched no elements
  within iterator match," "selector matches a container element
  instead of a leaf" with iterator present, and validator rule
  (iii)'s all-literal-binding rejection. Session 60's NHC apply
  failures (twice in Session 60, twice more in Session 61)
  surfaced the first message in retry excerpts and the LLM
  re-authored single-leaf rather than reading the message as a
  multi-leaf signal. v1.20 names the signal explicitly.

**Fixture-based integration test**
(`crates/pipeline/src/normalize.rs`). Two new tests in the
normalize-stage test module exercise `apply` composed with
`finalize` on a hand-authored multi-leaf css_select recipe
(positional inner selectors, three-row HTML fixture):

- `adr_0019_multi_leaf_position_only_table_applies_and_finalizes_end_to_end`
  asserts (a) three records produced (one per row),
  (b) per-row `headline` + `direction` extracted leaves,
  (c) topic tags from the plan reach every record's envelope
  via finalize's merge_topics, (d) per-row `dedup_key` resolves
  to `{recipe.id}:{headline}` through the ExtractedInner path.
  This is the apply→normalize integration the recipe_apply
  unit tests don't cover.
- `adr_0019_multi_leaf_preserves_record_shape_after_finalize`
  pins that a multi-leaf Event recipe finalises to an Event,
  catching accidental shape regressions in finalize's
  envelope-mut match when a future change touches it.

The integration test gives ADR 0019 a regression guard
independent of LLM behaviour: if a future prompt iteration
causes the recipe-author to stop authoring `extracted_inner`,
the type+validator+runtime+normalize composition still has CI
proof of correctness on the multi-leaf path.

### Session 63's gate to Accepted

Run the hurricane 5-trial with the v1.20 prompt. Promotion to
Accepted requires:

- **Sufficient condition** — ≥1 recipe authored with
  `FieldValueSource::ExtractedInner` across the 5 trials. This
  is the empirical falsification the v1.19 attempt couldn't
  produce.
- **Stronger signal** — ≥3 Event records per trial on the
  hurricane plan, each with distinct `headline` and `valid_at`
  extracted leaves, populating the dashboard's events panel.

If v1.20 doesn't shift the rate from 0/10 (Session 61) to
≥1/5 (Session 63), the prompt-engineering ceiling on
shape selection may be closer than the v1.20 hypothesis
suggests. Two follow-on directions are pre-staged:

1. **Reasoning-block-before-JSON experiment** — let the LLM
   write a freeform analysis of the prefetch before the
   structured-output recipe JSON, so the recognition checklist
   runs as visible reasoning rather than as latent
   token-distribution shifting.
2. **Recipe-iteration-on-FetchReport loop** (Session 60's
   candidate A) — when a single-leaf recipe fails at apply with
   "inner selector matched no elements," automatically
   re-author against the retry excerpt with the failure
   message inline as a multi-leaf signal.

### Session 64 verification (2026-05-12) — sufficient condition met

The Session 63 hurricane re-run wasn't run inside Session 63 (the
session pivoted to the cross-plan dashboard product gap). Session 64
ran it: 5 trials of the v1.20 prompt against the 2025 Atlantic
hurricane season plan via the eval-harness, with the
`recipes_with_extracted_inner` counter newly instrumented in
`TrialReport`. The JSONL lives at
`eval-runs/2025-atlantic-hurricane-season-20260512T153257Z.jsonl`.

Headline numbers:

| Trial | wall_s | recipes_persisted | with_extracted_inner | records |
|-------|--------|-------------------|----------------------|---------|
| 0     | 135.3  | 1                 | 1                    | 0       |
| 1     | 156.6  | 1                 | 0                    | 30      |
| 2     | 164.6  | 0                 | 0                    | 0       |
| 3     | 171.1  | 0                 | 0                    | 0       |
| 4     | 180.1  | 2                 | 1                    | 1       |

`recipes_with_extracted_inner` across the run: 2/5 trials (trials 0
and 4). Session 61 baseline: 0/10. **Sufficient condition met.** The
v1.20 prompt's combined effect — multi-leaf-as-first-class section
opener (20A), recognition checklist (20B), positional-selector
worked example (20C), apply-time-signal subsection (20D) — produced
the empirical falsification the v1.19 attempt couldn't.

**Stronger signal not met.** Both `extracted_inner`-bearing recipes
authored against `www.nhc.noaa.gov` failed at apply with the same
error: `extraction [css_select]: inner selector matched no elements
within iterator match`. The LLM picked the right shape (iterator +
inner) but the inner selector didn't land on a descendant of the
iterator's matched element. The acceptance gate moves from
**shape recognition** (which v1.20 carries) to **selector quality
at authoring time** — a different axis.

**Why this is Accepted and not Proposed-with-stronger-signal-gate.**
The Phase 2A implementation, the v1.20 prompt, and the validator
rules are not what's blocking records — the recipe-author is reaching
for the right shape. The remaining gap is the selector quality the
LLM produces against a prefetch excerpt, which is upstream of ADR
0019's scope. Pinning ADR 0019 as Proposed pending selector quality
would conflate two unrelated bottlenecks.

**Follow-on direction realignment.** The recipe-iteration-on-
FetchReport loop (Session 60's candidate A) is **explicitly gated by
ADR 0012** — the automated re-author loop must not be implemented
until ≥10 documented Class B failures exist across ≥3 extraction
modes, the predicate strings are evidence-grounded, and migration v7
is in place. Today's eval contributes the first such documented
cases (`docs/failure_cases/class_b/`), and Session 64 lands migration
v7's `prior_recipe_id` substrate. The loop lands in a later session
when the gate is met. The reasoning-block-before-JSON experiment
remains unblocked but is now a refinement target rather than a
fallback — v1.20 already cleared the falsification gate.

### Session 67 verification (2026-05-13) — Phase 2A validator-gate closed

**Discovered.** Across 5 FEMA trials (`eval-runs/fema-disaster-
declarations-2025-20260513T113806Z.jsonl`) the LLM authored
`json_path × json_path` iterator-bearing recipes against
`api.fema.gov/.../DisasterDeclarationsSummaries` on every trial.
Every one was intercepted at authoring with
`extraction mode not implemented: iterator (iterator runtime is
wired for css_select × css_select only in Phase 1 (ADR 0016))`. No
JSON iterator recipe persisted across the 5 trials. FEMA trial 2
came one validator-branch away from a strict Class B JsonPath case:
the LLM authored `$.DisasterDeclarationsSummaries[*]` outer +
`$.femaDeclarationString` inner, the inner would have failed at
apply with "matched no nodes within scope", and the recipe would
have been a strict ADR 0012 Class B JsonPath case at the apply
boundary.

**Cause.** The `apply_iterator` runtime (recipe_apply.rs:564) has
supported `json_path × json_path` since Session 61 — see "What
Phase 2A shipped" above, which names `apply_json_iterator` and the
"new (JsonPath, JsonPath) iterator pair" runtime tests. The
**structural validator** `validate_recipe_against_bytes`
(recipe_apply.rs:2313) was missing the matching match-arm; its
fallthrough returned `NotImplemented` with a stale "css_select ×
css_select only in Phase 1 (ADR 0016)" message that contradicted
the runtime. The **shape validator**
`validate_recipe_shape_against_bytes` had its own (JsonPath,
JsonPath) arm at line 2581, but the arm was unreachable because
the structural validator gates it at line 2490.

**Repair (not a new principle).** Session 67 added
`validate_json_iterator` (mirror of `validate_css_iterator`) and
wired the `(JsonPath, JsonPath)` arm into
`validate_recipe_against_bytes`. The fallthrough's
`NotImplemented` message now names both supported pairings,
matching the runtime's own text (recipe_apply.rs:601-609). This
closes the coherence gap; it does not introduce a new principle.
The Phase-2A scope ADR 0019 already declared (css_select +
json_path outer modes) is now uniformly enforced from prompt →
runtime → structural validator → shape validator.

Five new unit tests cover the validator path: happy path,
outer-no-match (predicate "iterator path … matched no nodes"),
inner-no-match (predicate "inner path … matched no nodes
within …"), inner-all-null (mirrors runtime's null-skip
rejection), bytes-not-json (category-error predicate). A sixth
test pins the updated `NotImplemented` message names both pairs.

**Carry-forward for ADR 0012.** Persisted `json_path × json_path`
recipes that fail at apply with "iterator path … matched no
nodes" or "inner path … matched no nodes within scope" are
strict Class B JsonPath cases. Until Session 67 these could not
exist on disk because the validator declined them at authoring.
The Session 67 FEMA-hunt re-run is expected to land at least one
such case, which would raise ADR 0012 Condition 2 (mode
diversity) from 2 to 3.

End of ADR.
