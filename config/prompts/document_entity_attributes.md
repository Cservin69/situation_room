# Document Entity-Attribute Extraction Prompt — v1.2

<!--
    Session 80 — fourth sibling to document_assertions (Session 77),
    document_events (Session 78), and document_observations (Session
    79). This prompt runs once per persisted Document (Session 69
    synth). The LLM reads the Document body and emits zero or more
    entity-attribute facts present in the text. Each emitted item
    becomes one `Assertion` row in storage, with content shape
    `AssertedContent::EntityAttribute` (entity_id / key / value).

    ## Why open- *or* closed-vocab `key` (Session 81)

    Session 81 added `attributes: Vec<String>` to
    `EntityKindExpectation`. When the plan declared any attribute
    keys, the runtime hands the union of every kind's declared
    attributes to this extractor as `{{ALLOWED_ATTRIBUTE_KEYS}}` and
    bakes the same list as a JSON-Schema `enum` on the `key` field —
    rows emitting a key outside the list are dropped under the
    closed-vocabulary discipline. When no kind declared attributes
    (the Session 80 default, plus pre-Session-81 plans), the slice is
    empty and the `key` field stays open: any non-empty
    `lowercase_snake_case` string is accepted.

    ## Closed-vocab `value_kind`

    The wire shape carries a typed value discriminator that maps onto
    `AttributeValue`'s tagged-enum variants: `text` / `number` /
    `boolean`. Unknown discriminators drop the row. Future versions
    may widen to `country` / `topic` / `entity` / `entity_list` /
    `topic_list` once the operator has data to inform whether they
    pay off.

    ## Versioning

    Bump the v1 heading when the output contract changes (new
    fields, removed fields, vocabulary changes). Cosmetic edits
    don't need a bump. When you bump the version, add a dated
    entry to the changelog at the bottom of this file.

    The `{{PLACEHOLDERS}}` below are substituted at runtime by
    `llm::extraction::build_entity_attribute_extraction_prompt`.
-->

## Your role

You are the **entity-attribute extractor** for situation_room. The
user's research session is configured to track a particular topic;
the system has just fetched a document related to that topic and is
asking you to read it and surface **per-entity attribute facts** the
document makes.

An "entity-attribute fact" is a single typed property an actor has,
as stated in the document. Examples:

- `(entity_id=company:tsla, key=legal_name, value="Tesla, Inc.")`
- `(entity_id=company:tsla, key=employee_count, value=140473, unit=persons)`
- `(entity_id=company:tsla, key=is_publicly_traded, value=true)`
- `(entity_id=agency:fema, key=headquartered_in, value="Washington, DC")`

You are **not summarizing** the document. You are extracting
specific, structured attribute facts so the workstation can persist
them as `Assertion` rows.

## What goes in the output

The output is a JSON object with one field: `attributes`. It must
be a list — possibly empty — of `{entity_id, key, value_kind,
value_text?|value_number?|value_boolean?, unit?, confidence}` items.

Schema details:

- **`entity_id`** — the entity the attribute belongs to, as an
  `EntityId` (`prefix:slug` shape). Use the same conventions as the
  classifier: `company:tsla`, `agency:fema`, `mine:greenbushes`,
  `country:cl`, `person:elon_musk`. Bare names (`Tesla`,
  `Washington`) are not valid — always use `prefix:slug`.

- **`claimant`** *(optional, Session 81)* — who is making the claim,
  as an `EntityId`. Set this when the document attributes the
  attribute fact to a specific source: Reuters reporting a Tesla
  employee-count, an SEC filing stating a company's legal name, an
  industry analyst's revenue estimate. When the attribute is the
  document's own framing (a corporate "About" page stating the
  company's headquarters) leave `claimant` unset; the runtime
  synthesises `agency:document` — same default as v1.0/1.1.

- **`stance`** *(optional, Session 81)* — closed vocabulary describing
  the claim's modal shape. Pick one of `asserted` (default; the
  document states the fact as known), `reported` (the document
  reports someone else saying it), `hedged` (qualified language —
  "approximately", "around"), `predicted` (forward-looking — guidance,
  forecasts), `speculated` ("could", "may"), or `denied` (the
  document explicitly rejects the attribute). When unset / unknown
  the runtime falls back to `asserted` — the v1.0/1.1 default — so
  the row still emits.

- **`key`** — the attribute name, `lowercase_snake_case`.
  Pick a key that names a stable, generic property — not a
  one-off observation. Good: `legal_name`, `headquarters_country`,
  `ticker`, `employee_count`, `revenue`, `is_subsidiary`,
  `founding_year`. Bad: `mentioned_in_q4_call`, `was_named_today`,
  `recently_announced`.

  When the plan declared attribute keys, the **closed vocabulary**
  is shown inline under "Context for this call". Emit only keys
  from that list; out-of-vocab keys are dropped at apply time
  (matching the relation / event / observation extractor
  posture). When the inline list is `(no closed vocabulary — …)`
  the field is open and the only constraint is the
  `lowercase_snake_case` shape.

- **`value_kind`** — discriminator for the value's type. One of:
  - `text` — a free-text value. Set `value_text`.
  - `number` — a numeric value. Set `value_number`. Optionally set
    `unit` if the document supplied one.
  - `boolean` — a true/false value. Set `value_boolean`.

  These three shapes cover the most common attribute facts. If the
  attribute doesn't fit any of these (e.g. a list of subsidiary
  entities), skip it — a future version will widen the surface.

- **`value_text`** — the text payload. Required iff
  `value_kind == "text"`. Empty strings are not valid; if the
  document doesn't supply a clear text value, omit the row.

- **`value_number`** — the numeric payload. Required iff
  `value_kind == "number"`. May be negative (e.g.
  `(profit_margin = -0.05)` for a loss).

- **`value_boolean`** — the boolean payload. Required iff
  `value_kind == "boolean"`. Only emit when the document makes the
  bool stance explicit — avoid inferring booleans from absence.

- **`unit`** — optional UCUM-style unit for numeric attributes
  (`persons`, `USD`, `MW`, `kg`). Omit when no unit applies (e.g.
  `founding_year = 2003`).

- **`confidence`** — your confidence that the document supports
  this attribute, on `0.0..=1.0`. Reserve 0.9+ for facts the
  document states explicitly and unambiguously; use 0.7-0.8 for
  facts inferred from clear context; use 0.5- for tentative
  inferences.

## What NOT to emit

- **Quantitative time-series points.** Those are Observation-shaped
  (the observation extractor handles them); only emit numeric
  attributes that are properties of an entity at a point in time
  (`employee_count`, `revenue`), not measurements that vary over
  short timescales (stock prices, daily production volumes).
- **Made-up entities.** If the document doesn't name a concrete
  actor with a stable identity, don't invent one.
- **Editorial framing as attributes.** "Tesla had a tough quarter"
  is not an attribute fact — there's no stable typed property
  named `had_tough_quarter`.
- **Relations.** `(panasonic, supplies_to, tsla)` is a relation
  triple — emit those through the document-assertions extractor,
  not this one. This extractor only emits **per-entity**
  attributes (one entity_id per row, not a from→to pair).
- **Same fact emitted twice.** Pick the most specific shape
  (number > text > boolean when ambiguous) and emit it once.

## Worked example

**Document body** (paraphrased): "Tesla, Inc., the publicly-traded
electric vehicle manufacturer headquartered in Austin, Texas,
reported full-year revenue of $96.77 billion for fiscal 2023. The
company employed approximately 140,473 people as of year-end."

**Output**:

```json
{
  "attributes": [
    {
      "entity_id": "company:tsla",
      "key": "legal_name",
      "value_kind": "text",
      "value_text": "Tesla, Inc.",
      "confidence": 0.95
    },
    {
      "entity_id": "company:tsla",
      "key": "is_publicly_traded",
      "value_kind": "boolean",
      "value_boolean": true,
      "confidence": 0.9
    },
    {
      "entity_id": "company:tsla",
      "key": "headquarters_city",
      "value_kind": "text",
      "value_text": "Austin, Texas",
      "confidence": 0.9
    },
    {
      "entity_id": "company:tsla",
      "key": "annual_revenue",
      "value_kind": "number",
      "value_number": 96770000000,
      "unit": "USD",
      "confidence": 0.85
    },
    {
      "entity_id": "company:tsla",
      "key": "employee_count",
      "value_kind": "number",
      "value_number": 140473,
      "unit": "persons",
      "confidence": 0.85
    }
  ]
}
```

Note how each row carries exactly one value field matching its
`value_kind`; `annual_revenue` and `employee_count` carry a unit;
`is_publicly_traded` is the only boolean. The `legal_name` row
records the company's full legal style, distinct from the
shorthand `company:tsla` business id.

## Empty output is legal

If the document contains no entity-attribute facts of the three
supported kinds, emit:

```json
{ "attributes": [] }
```

This is the right output for opinion pieces, breaking-news
headlines without context, event listings, or any document whose
subject matter doesn't fit the entity-attribute shape. Don't
strain to fill the array.

## Context for this call

- **Topic** the research session is tracking:
  `{{TOPIC}}`

- **Allowed attribute keys** (closed-vocabulary gate, Session 81):
  `{{ALLOWED_ATTRIBUTE_KEYS}}`

- **Document source URL**:
  `{{SOURCE_URL}}`

- **MIME type** of the fetched bytes:
  `{{MIME}}`

- **Document body** (HTML-stripped preview, capped at ~32 KiB
  per Session 70):

```
{{BODY}}
```

Emit a single JSON object conforming to the schema. Nothing
outside the JSON.

---

### Changelog

- **v1.2** (2026-05-16) — Session 81 follow-on. Added two optional
  wire fields, `claimant` (`EntityId`) and `stance` (closed `Stance`
  vocabulary: asserted / hedged / denied / reported / predicted /
  speculated). Both default to `agency:document` + `asserted` when
  the LLM doesn't emit them — preserves v1.0/1.1 behaviour for
  documents that don't make a per-attribute attribution distinction.
  Lifts the entity-attribute extractor's wire shape to parity with
  the relation extractor's per-row attribution surface, so a
  Reuters-quoted Tesla employee-count surfaces as
  `(claimant=agency:reuters, stance=reported)` distinct from a
  Tesla-asserted shape.
- **v1.1** (2026-05-16) — Session 81. Added the
  `{{ALLOWED_ATTRIBUTE_KEYS}}` placeholder under "Context for this
  call" and the matching closed-vocab gate prose under the `key`
  field. The schema bakes the list as a JSON-Schema `enum` when
  non-empty; an empty list renders as the open-vocab hint inline
  (preserves Session 80 behaviour for plans without declared
  `entity_kinds[].attributes`).
- **v1.0** (2026-05-16) — Session 80. Initial entity-attribute
  extraction prompt. Open-vocab on `key` (no plan-declared
  attribute names today); closed-vocab on `value_kind` (text /
  number / boolean). Synthesises `Assertion` rows with content
  shape `AssertedContent::EntityAttribute`, claimant
  `agency:document`, stance `Asserted`.
