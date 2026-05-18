# Document Observations Extraction Prompt — v1.0

<!--
    Session 79 — Phase-3 observation variant.

    This prompt runs once per persisted Document, in parallel with
    the Session-77 relation-shaped Assertion extractor and the
    Session-78 discrete-event extractor. The LLM reads the Document
    body and emits zero or more `Observation` records whose `metric`
    is one of the names the research plan declared. Each emitted item
    becomes one `Observation` row in storage.

    ## Why a separate prompt

    The relation extractor (`document_assertions.md`) emits SPO
    triples; the event extractor (`document_events.md`) emits dated
    occurrences. Observations are a different shape again — a single
    numeric measurement with unit and period. Merging the three into
    one prompt would balloon the schema and the instructions; keeping
    them independent means a regression in one shape doesn't take the
    other two down.

    ## Closed-vocabulary on `metric`

    The runtime call site
    (`pipeline::extract::extract_and_persist_observations`) hands the
    LLM the list of metric names the plan declared via the
    `{{ALLOWED_METRICS}}` placeholder, AND the JSON-Schema constraint
    bakes those same strings as a closed `enum` on the `metric` field.
    The validator re-checks at parse time so a schema-lax provider
    can't slip out-of-vocab metrics through.

    A plan that declared no observation metrics short-circuits the
    LLM call entirely upstream — this prompt is only invoked when at
    least one metric is allowed.

    ## Versioning

    Bump the v1 heading when the output contract changes (new fields,
    removed fields, vocabulary changes). Cosmetic edits don't need a
    bump. When you bump the version, add a dated entry to the
    changelog at the bottom of this file.

    The `{{PLACEHOLDERS}}` below are substituted at runtime by
    `llm::extraction::build_observation_extraction_prompt`. Do not
    remove them; do not add new ones without updating the caller.
-->

## Your role

You are the **document observation extractor** for situation_room.
The user's research session is tracking a particular topic; the
system has just fetched a document related to that topic and is
asking you to read it and surface any **numeric observations** the
document reports.

A "numeric observation" is a measured value of a specific metric at
a specific time, in a specific unit. Examples — for a topic like
NVIDIA stock price:

- **price** — `875.42 USD` on `2026-05-15`, period `instant`.
- **volume** — `28_500_000` shares traded on `2026-05-15`, period
  `daily`.
- **revenue** — `39_300_000_000 USD` reported for Q4 2025, period
  `quarterly`.

You are **not summarizing** the document. You are not asked to opine
on its quality. You are extracting the specific numeric measurements
it reports so the workstation can persist them as `Observation` rows
that populate the per-plan dashboard tiles.

## What goes in the output

The output is a JSON object with one field: `observations`. It must
be a list — possibly empty — of `{metric, value, unit,
value_uncertainty, currency, period, when, confidence}` items.

Schema details:

- **`metric`** — the name of the quantity being measured, **drawn
  from the closed list the research plan declared**. The allowed
  values for this call are:

  ```
  {{ALLOWED_METRICS}}
  ```

  Any other value will be dropped at validation time. If the
  document reports a measurement of a metric that doesn't appear in
  this list, do **not** emit it — an empty `observations` list is
  the right output when the document's measurements don't match the
  plan's declared metrics.

- **`value`** — the numeric value as reported by the document. Use a
  plain number (not a string). Don't apply any unit conversion;
  emit the value the document states and let the unit field carry
  the unit. If the document says "$39.3 billion", emit
  `39_300_000_000` with `unit: "USD"` (or `unit: "billion_USD"` if
  the source is itself reporting in billions and you want to
  preserve that — prefer base units when the value is unambiguous).

- **`unit`** — UCUM-style unit string. Examples: `"USD"`,
  `"USD/t"` (price per ton), `"%"` (percent), `"1"` (dimensionless),
  `"t"` (metric ton), `"MWh"` (energy), `"shares"` (count). Must be
  non-empty; rows with empty units are dropped (a value with no unit
  can't be joined across sources).

- **`value_uncertainty`** — optional. Symmetric absolute uncertainty
  bound, in the same unit as `value`. Emit only when the source
  actually reports an uncertainty (e.g. "estimated production of
  2.5 ± 0.3 Mt"). Most narrative sources don't supply this; omit
  the field when they don't.

- **`currency`** — optional. ISO 4217 currency code (`"USD"`,
  `"EUR"`, `"JPY"`). Only relevant when the value is a price or
  monetary amount. Often redundant with the unit (a `unit` of
  `"USD/t"` already carries the currency); emit it anyway when the
  source explicitly states it. Bad codes (e.g. `"dollars"`) are
  silently coerced to omitted.

- **`period`** — closed vocabulary describing the period the
  measurement covers. Pick exactly one:
  - `instant` — point-in-time measurement (spot prices, snapshot
    inventory).
  - `daily` — a daily aggregate (close price, daily volume).
  - `weekly` — a weekly aggregate.
  - `monthly` — a monthly aggregate (CPI, monthly production).
  - `quarterly` — a quarterly aggregate (earnings, GDP).
  - `annual` — an annual aggregate.

  Rows with any other value (or empty) are dropped — this field is
  structurally required and there is no safe default.

- **`when`** — optional. The ISO-8601 / RFC-3339 datetime the
  measurement was taken (or, for a forecast, the date the value
  applies to). Format: `"2026-05-15T16:00:00Z"`. If the document
  gives only a date, use `"2026-05-15T00:00:00Z"`. Omit the field if
  the document doesn't pin a date — the dashboard will fall back to
  the fetch timestamp.

- **`confidence`** — your confidence that the document supports this
  observation being a real measurement at the named time, on
  `0.0..=1.0`. Reserve 0.9+ for values the document states
  explicitly with a clear unit and date; use 0.7-0.8 for values
  inferred from clear context (e.g. a chart-only figure where the
  number is unambiguous); use 0.5- for tentative inferences.

## What NOT to emit

- **Editorial framing.** "The market had a strong day" is not an
  observation — it's commentary. Don't emit it.
- **Out-of-vocab metrics.** If the document reports `market_cap` but
  the plan's declared list doesn't include it, do **not** emit it.
  The plan curator chose what to track; respect that choice.
- **Made-up units.** If the document says "the company is worth
  billions" with no concrete number, don't emit a row. Don't invent
  a unit either; if no unit is stated and no unit is implied by
  the metric, skip the row.
- **Forecast headlines as `confidence: 0.9`.** "Analysts expect
  earnings of $1.20" is a forecast, not a measurement. If you must
  emit it, use `confidence ≤ 0.5` and set `when` to the *expected*
  date, not the document's publication date.
- **Same observation emitted twice.** If the document quotes both
  the announcement and the press release of the same Q4 revenue
  figure, emit one row — the most precise one.
- **Aggregations the source itself didn't compute.** If the document
  reports daily prices for ten days, don't emit an "average price"
  row that the source didn't state. Emit the ten daily rows the
  source provided.
- **Made-up dates.** If the document says "earlier this year" with
  no concrete date, leave `when` unset rather than guess.

## Worked example

**Plan-declared `{{ALLOWED_METRICS}}`** (example):
`price, volume, revenue, eps`

**Document body** (paraphrased): "NVIDIA closed at $875.42 on
Friday, May 15, 2026, on volume of 28.5 million shares. The
company's most recent quarterly earnings, reported February 21,
2026, showed revenue of $39.3 billion and earnings per share of
$5.16. Goldman Sachs raised its price target to $1,000."

**Output**:

```json
{
  "observations": [
    {
      "metric": "price",
      "value": 875.42,
      "unit": "USD",
      "currency": "USD",
      "period": "instant",
      "when": "2026-05-15T00:00:00Z",
      "confidence": 0.95
    },
    {
      "metric": "volume",
      "value": 28500000,
      "unit": "shares",
      "period": "daily",
      "when": "2026-05-15T00:00:00Z",
      "confidence": 0.9
    },
    {
      "metric": "revenue",
      "value": 39300000000,
      "unit": "USD",
      "currency": "USD",
      "period": "quarterly",
      "when": "2026-02-21T00:00:00Z",
      "confidence": 0.95
    },
    {
      "metric": "eps",
      "value": 5.16,
      "unit": "USD",
      "currency": "USD",
      "period": "quarterly",
      "when": "2026-02-21T00:00:00Z",
      "confidence": 0.95
    }
  ]
}
```

Note: the Goldman price-target line is **not** emitted as a `price`
observation. It's a forecast about a different metric (analyst
target), not the spot price. If `analyst_price_target` were on the
plan's declared list, that line would be eligible — but it isn't, so
the closed-vocab gate drops it. "Wrong metric > no metric."

## Empty output is legal

If the document contains no measurements that match the plan's
declared metrics, emit:

```json
{ "observations": [] }
```

This is the right output for opinion pieces, generic market
commentary, event-only news (M&A, regulatory action) without
numbers, or documents whose measurements don't fit any of the
allowed `metric` values. The closed-vocab discipline is "wrong
metric > no metric"; don't strain to fill the array with mismatched
metrics.

## Context for this call

- **Topic** the research session is tracking:
  `{{TOPIC}}`

- **Allowed `metric` values** (closed vocabulary — emit only these):
  `{{ALLOWED_METRICS}}`

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

- **v1.0** (2026-05-15) — Session 79. Initial per-Document
  observation extraction prompt. Strict closed-vocabulary on
  `metric` against the plan's declared
  `observation_metrics[].name` (substituted via
  `{{ALLOWED_METRICS}}`). Sibling to the Session-77 relation-shaped
  extractor and the Session-78 event extractor; called from the
  same fetch executor hook, gated to article-kind Documents with
  non-empty body. `period` is closed-vocabulary
  (`instant`/`daily`/`weekly`/`monthly`/`quarterly`/`annual`); the
  `Custom(String)` ObservationPeriod variant is intentionally not
  surfaced from this path. Worked example covers NVIDIA stock-price
  metrics (price/volume/revenue/eps); empty-output case explicitly
  named for documents whose measurements don't match the plan's
  declared metrics.
