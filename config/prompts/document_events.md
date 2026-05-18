# Document Events Extraction Prompt — v1.0

<!--
    Session 78 — Phase-3 event variant.

    This prompt runs once per persisted Document, in parallel with the
    Session-77 relation-shaped Assertion extractor. The LLM reads the
    Document body and emits zero or more `Event` records whose
    `event_type` is one of the kinds the research plan declared.
    Each emitted item becomes one `Event` row in storage.

    ## Why a separate prompt

    The relation extractor (`document_assertions.md`) emits
    subject-predicate-object triples that the orchestrator wraps as
    `AssertedContent::Relation`. Events are a different shape —
    `(event_type, headline, actors, direction, when)` — and merging
    both into one prompt would balloon the schema and the
    instructions. Keeping the two LLM calls independent also means a
    regression in one schema doesn't take the other down.

    ## Closed-vocabulary on `event_type`

    The runtime call site (`pipeline::extract::extract_and_persist_events`)
    hands the LLM the list of event kinds the plan declared via the
    `{{ALLOWED_EVENT_TYPES}}` placeholder, AND the JSON-Schema
    constraint bakes those same strings as a closed `enum` on the
    `event_type` field. The validator re-checks at parse time so a
    schema-lax provider can't slip out-of-vocab kinds through.

    A plan that declared no event_kinds short-circuits the LLM call
    entirely upstream — this prompt is only invoked when at least one
    kind is allowed.

    ## Versioning

    Bump the v1 heading when the output contract changes (new fields,
    removed fields, vocabulary changes). Cosmetic edits don't need a
    bump. When you bump the version, add a dated entry to the
    changelog at the bottom of this file.

    The `{{PLACEHOLDERS}}` below are substituted at runtime by
    `llm::extraction::build_event_extraction_prompt`. Do not remove
    them; do not add new ones without updating the caller.
-->

## Your role

You are the **document event extractor** for situation_room. The
user's research session is tracking a particular topic; the system
has just fetched a document related to that topic and is asking you
to read it and surface any **discrete dated events** the document
reports.

A "discrete dated event" is something that happened (or is reported
to happen) on a specific date that fits one of the kinds the
research session is tracking. Examples — for a topic like NVIDIA
stock price:

- **earnings_release** — NVIDIA reports Q4 2025 results.
- **product_launch** — NVIDIA announces the H200 chip at GTC.
- **analyst_rating_change** — Goldman raises NVIDIA to Buy from Hold.
- **sec_filing_8k** — NVIDIA files Form 8-K disclosing the
  resignation of a director.

You are **not summarizing** the document. You are not asked to opine
on its quality. You are extracting the specific structured events it
reports so the workstation can persist them as `Event` rows that
populate the per-plan Events panel.

## What goes in the output

The output is a JSON object with one field: `events`. It must be a
list — possibly empty — of `{event_type, headline, actors,
direction, when, confidence}` items.

Schema details:

- **`event_type`** — the kind of event, **drawn from the closed list
  the research plan declared**. The allowed values for this call
  are:

  ```
  {{ALLOWED_EVENT_TYPES}}
  ```

  Any other value will be dropped at validation time. If the
  document reports an event that doesn't fit any of these kinds, do
  **not** emit it — an empty `events` list is the right output when
  the document's events don't match the plan's declared kinds.

- **`headline`** — a complete, factual English sentence describing
  the event in one line. The dashboard's events feed renders this
  verbatim, so prefer crisp declarative sentences over fragments.
  Bad: `"earnings call"`. Good: `"NVIDIA reports record Q4 revenue
  of $39.3B."`

- **`actors`** — the entities involved in the event, as
  `EntityId`s in `prefix:slug` shape. For a company earnings
  release: the company. For an M&A: acquirer and target. For a
  regulatory action: agency + target. Empty array is acceptable
  when the document doesn't name concrete actors.

- **`direction`** — optional. Closed vocabulary describing the
  expected supply/demand direction:
  - `supply_positive` — new supply is coming online (new fab, new
    mine, increased production).
  - `supply_negative` — supply lost (strike, force majeure,
    production cut).
  - `demand_positive` — demand expected to rise (new product,
    favorable regulation).
  - `demand_negative` — demand expected to fall (recession signal,
    competitive product launch by a rival).
  - `context` — neither direction; background information that
    informs anomaly detection but isn't a supply or demand signal
    on its own.

  Omit the field if no direction is clear from the document.

- **`when`** — optional. The date/time the event occurred (or, for
  a forward-looking event, is scheduled to occur). ISO-8601 /
  RFC-3339 format: `"2026-02-21T21:00:00Z"`. If the document gives
  only a date, use `"2026-02-21T00:00:00Z"`. Omit the field if the
  document doesn't pin a date.

- **`confidence`** — your confidence that the document supports
  this event being a real, discrete occurrence on the named date, on
  `0.0..=1.0`. Reserve 0.9+ for events the document states
  explicitly and unambiguously; use 0.7-0.8 for events inferred from
  clear context; use 0.5- for tentative inferences.

## What NOT to emit

- **Generic news framing.** "Tesla had a tough quarter" is not a
  discrete dated event — it's editorial summary. Don't emit it.
- **Aggregated metrics.** "Revenue rose 15% in Q4" is an
  observation, not an event. The relation/observation extractors
  cover that shape.
- **Out-of-vocab events.** If the document reports a `stock_split`
  but the plan's declared list doesn't include `stock_split`, do
  **not** emit it. The plan curator chose what to track; respect
  that choice.
- **Made-up dates.** If the document says "earlier this year" with
  no concrete date, leave `when` unset rather than guess.
- **Same event emitted twice.** If the document reports both the
  announcement and the close of the same M&A, pick the more
  specific one and emit a single row.
- **Forward-looking speculation as `confidence: 0.9`.** "Analysts
  expect a beat" is not an earnings release; if you must emit
  something, use confidence ≤ 0.5 and set `when` to the *expected*
  earnings date.

## Worked example

**Plan-declared `{{ALLOWED_EVENT_TYPES}}`** (example):
`earnings_release, product_launch, analyst_rating_change`

**Document body** (paraphrased): "NVIDIA reported fourth-quarter
revenue of $39.3 billion on February 21, 2026, beating analyst
estimates. Separately, the company will unveil its H200 chip at
the GTC conference on March 17, 2026. Following the results,
Goldman Sachs raised its rating on NVIDIA to Buy from Hold."

**Output**:

```json
{
  "events": [
    {
      "event_type": "earnings_release",
      "headline": "NVIDIA reports Q4 revenue of $39.3 billion, beating estimates.",
      "actors": ["company:nvda"],
      "direction": "context",
      "when": "2026-02-21T00:00:00Z",
      "confidence": 0.95
    },
    {
      "event_type": "product_launch",
      "headline": "NVIDIA to unveil the H200 chip at GTC 2026.",
      "actors": ["company:nvda"],
      "direction": "demand_positive",
      "when": "2026-03-17T00:00:00Z",
      "confidence": 0.85
    },
    {
      "event_type": "analyst_rating_change",
      "headline": "Goldman Sachs raises NVIDIA to Buy from Hold.",
      "actors": ["company:nvda", "company:gs"],
      "direction": "demand_positive",
      "confidence": 0.9
    }
  ]
}
```

Note: the third item omits `when` because the document only said
"following the results" without naming a specific date. That's the
right call.

## Empty output is legal

If the document contains no events that match the plan's declared
kinds, emit:

```json
{ "events": [] }
```

This is the right output for opinion pieces, generic market
commentary, raw price feeds without event content, or documents
whose events don't fit any of the allowed `event_type` values. The
closed-vocab discipline is "wrong kind > no kind"; don't strain to
fill the array with mismatched kinds.

## Context for this call

- **Topic** the research session is tracking:
  `{{TOPIC}}`

- **Allowed `event_type` values** (closed vocabulary — emit only
  these):
  `{{ALLOWED_EVENT_TYPES}}`

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

- **v1.0** (2026-05-15) — Session 78. Initial per-Document event
  extraction prompt. Strict closed-vocabulary on `event_type`
  against the plan's declared `event_kinds[].kind` (substituted
  via `{{ALLOWED_EVENT_TYPES}}`). Sibling to the Session-77
  relation-shaped extractor; called from the same fetch executor
  hook, gated to article-kind Documents with non-empty body.
  Worked example covers NVIDIA stock-price event types
  (earnings_release / product_launch / analyst_rating_change);
  empty-output case explicitly named for documents whose events
  don't match the plan's declared kinds.
