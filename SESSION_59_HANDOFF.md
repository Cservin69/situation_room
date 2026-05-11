# Session 59 — Handoff

Session 59 set out to broaden the records dashboard by changing the
topic to one whose record-type distribution would populate Events
(per the Session 58 handoff: "five record types render as collapsed
pills" was the gap to close). Across two topic pivots (US Federal
Reserve monetary policy 2026, then 2025 Atlantic hurricane season)
the gate did not open. **No code shipped.** The finding is the
session deliverable: the typed-panel roadmap is bounded by the
**classifier**, not by topic choice.

## What was tried

### Stage 0 — topic #1: "US Federal Reserve monetary policy 2026"

Predicted in the kickoff to populate Events (FOMC decisions) +
Documents (statements, minutes) + Observations (federal funds
rate). High-fetchability hosts (federalreserve.gov,
fred.stlouisfed.org). Low operator-verification-cost.

Eval-harness, 5 trials. JSONL:
`eval-runs/us-federal-reserve-monetary-policy-2026-20260511T100401Z.jsonl`.
Narrative log:
`eval-runs/session59-fed-20260511T100401Z.log` (approximate; the
operator-side rsync writes one log per run).

| trial | records | succeeded | wall_clock |
|-------|---------|-----------|------------|
| 0 | 2 | fred.stlouisfed.org, www.federalreserve.gov | 163.2 s |
| 1 | 0 | — | 130.5 s |
| 2 | 2 | fred.stlouisfed.org, www.bea.gov | 167.3 s |
| 3 | 3 | fred.stlouisfed.org, www.imf.org, www.federalreserve.gov | 103.7 s |
| 4 | 1 | fred.stlouisfed.org | 105.2 s |

records mean **1.60** (range 0–3, σ 1.02), wall_clock mean 134.0 s.

Classifier emitted **24 `observation_metric:N` per-target
nominations across 5 trials** — covering `federal_funds_rate`,
`inflation_cpi`, `inflation_pce`, `unemployment_rate`,
`real_gdp_growth`, `fed_balance_sheet`. **Zero `event_type`
nominations**. The classifier read "Fed monetary policy" as a
quantitative-indicators topic rather than a decisions-and-statements
topic.

Both fall-through clauses (records < 3 mean, observation-only)
fired. Proceeded to topic #2 per kickoff.

### Stage 0 — topic #2: "2025 Atlantic hurricane season"

Predicted to populate Events densely (one per named storm) +
Observations (wind/pressure/category) + Entities (storms with
geometry) + Documents (NHC advisories). Government public hosts
(`nhc.noaa.gov`, `weather.gov`).

Eval-harness, 5 trials. JSONL:
`eval-runs/2025-atlantic-hurricane-season-20260511T101820Z.jsonl`.

| trial | records | succeeded |
|-------|---------|-----------|
| 0 | 0 | — |
| 1 | 1 | www.cpc.ncep.noaa.gov |
| 2 | 2 | www.nhc.noaa.gov ×2 |
| 3 | 1 | www.iii.org |
| 4 | 1 | www.nhc.noaa.gov |

records mean **1.00** (range 0–2, σ 0.63), wall_clock mean 153.6 s.

Classifier emitted 12 `observation_metric:N` nominations and **1
`event_type:0` nomination** — the only event-shaped expectation
across all three topics × 15 trials. That single event nomination
then **declined at the url-proposer stage**: the proposer landed on
the Climate Prediction Center's ENSO Diagnostic Discussion page,
which provides ENSO state observations but not individual storm
events. Outcome reason verbatim: *"The source provides ENSO
diagnostic discussions but contains no data on tropical storm
formations or individual storm events for the target expectation."*

Two `www.nhc.noaa.gov` recipes that did succeed at the URL stage
then **failed at the apply stage** with the iterator+inner selector
pattern: *"inner selector matched no elements within iterator match
… the inner selector is targeted at a sibling rather than a
descendant."* This is the recipe-author's closest-to-multi-field
extraction primitive (an iterator over list items, with inner
selectors per field), and on storm-list pages it broke. Suggestive
but not dispositive — the failed recipes could have been targeting
observations-on-list shape, not events.

Both fall-through clauses fired again. Kickoff did not specify a
topic-#3 fallthrough, and the cross-topic pattern (next section) is
clear enough that running a third pivot would burn budget without
moving the picture.

## The binding finding

Aggregate nomination shapes across all three eval runs (15 trials,
3 topics) — these are *what the classifier asks for*, not what
gets authored:

| topic | obs_metric | event_type | entity_kind | relation_kind | document_source |
|-------|-----------:|-----------:|------------:|--------------:|----------------:|
| Fed | 24 | 0 | 0 | 0 | 0 |
| Hurricanes | 12 | 1 | 0 | 0 | 0 |
| Lithium | 4 | 0 | 0 | 0 | 0 |
| **Σ** | **40** | **1** | **0** | **0** | **0** |

The classifier produces per-target nominations of essentially one
shape (`observation_metric:N`) across all three topics — including
two that are textbook event-shaped (Fed: "9-3 vote to cut 25 bp" is
an Event; Hurricanes: "Hurricane X made landfall" is an Event).
**This is not a topic-selection problem.** Even when the topic
strongly suggests Events, the classifier nominates indicator
metrics.

And on the single occasion the classifier did emit `event_type:0`,
the downstream chain (url proposer → recipe author → apply) did
not materialize it into a record. So the dashboard's five empty
typed panels are bounded by **two upstream gates in series**:

1. **Classifier emits almost no non-observation per-target
   nominations.** The classifier prompt and/or topic→record-type
   mapping is biased toward measurement framings.
2. **Recipe-author has no proven multi-field extraction primitive.**
   `css_select` / `regex_capture` are scalar-targeted; the
   iterator-with-inner-selectors pattern is the closest to multi-
   field and visibly broke on the one hurricanes Event-adjacent
   attempt. `EventContent` (event_type + headline + actors +
   direction + magnitude) and `RelationContent` (kind + from + to +
   magnitude) and `EntityAttributeContent` (entity_id + key +
   typed-value) each need multi-field extraction; none currently
   has it.

Either gate is sufficient to block typed-panel work. Both gates
are open questions, not one-line patches — neither has an ADR
yet.

**Hard recommendation:** typed panels for the five non-Observation
record types are not the next-session hill. Session 60 picks a
direction that either (a) sets the dashboard's value on what the
pipeline *can* already produce (Observations, in plurality and
depth), or (b) opens the upstream gate with a concrete ADR.

## What did NOT ship

A full speculative draft of `EventsTimeline.svelte` (horizontal-
dot timeline, direction-color routing off the closed
`EventDirection` enum, auto-selected-latest detail card with
magnitude rendered via the same magnitude-aware fmtValue
`MetricCard` uses) was authored against the schema during Stage 1's
wait time. The operator's post-eval rsync (RustRover →
SituationRoom with `--delete`) wiped it, which is the right
outcome — there's no data to validate the panel against, and
shipping unvalidated UI scaffolding would violate the kickoff's
"live observation only at the end" discipline.

The design is preserved in this handoff and in the Session 59
conversation transcript; rebuilding from the schema is a few hours
of focused work and should be trivial once Events actually
materialize.

### Design notes for a future Events panel (when Events populate)

- **Horizontal dot timeline** ordered by `envelope.valid_at`,
  fallback `observed_at`. Dot position: `((t − tMin) / range) ×
  100%`. Year-tick axis below, picking integer years with stride if
  N > 6.
- **Direction-color routing** off the closed `EventDirection`
  enum, bucketed: `supply_positive | demand_positive → positive`,
  `supply_negative | demand_negative → negative`, `context | null →
  info`. Supply/demand share a color because the operator's vantage
  is price-impact direction, not mechanism; the verbose label
  lives on the chip in the detail card.
- **Auto-selected-latest detail card** mirroring MetricCard's
  layout: title row (date + direction chip + event_type chip),
  headline as the big content, magnitude rendered via the same
  magnitude-aware fmtValue (since `EventContent.magnitude` IS an
  embedded `ObservationContent`), actors as inline list, source
  host in the footer. `$state<number | null>(null)` for selection;
  default = last index so the panel reads as "what just happened"
  without operator action.
- **Slot into `RecordsDashboard.svelte`** between the Observations
  panel and the pending-types pills, gated on
  `records.events.length > 0`. Add `'event'` to a `PANEL_KINDS`
  set so the pending-pills filter excludes it (the pattern
  generalizes: each future typed panel adds one entry to
  `PANEL_KINDS`).
- **What the design deliberately does NOT do.** No vertical
  separation by direction (color is enough until "ratio of cuts to
  hikes" becomes a binding question); no geometry rendering (that
  needs a map surface); no multi-event selection / brushing (one
  event at a time, dashboard not research surface).

## Candidate directions for Session 60

Framed as product questions, not infra-debt items (per Session 58's
"product not infra-debt" feedback). The operator picks; I'm
deliberately not ranking these because the right pick depends on
what the operator's currently using the dashboard *for*.

**(A) Deepen Observations.** The pipeline produces Observations
well; the dashboard's MetricCard renders them well at N=1; nothing
in between is built. Three sub-directions:

  - **Drill-into-metric detail view.** Click a MetricCard → opens a
    full chart (uplot, already in `package.json`) with axis labels,
    hover crosshairs, range brushing. Becomes meaningful when N≥~6
    on a single metric — not yet reached on any topic but
    approachable for a topic with longer history (e.g., "US CPI
    monthly 2020-2026" would route through FRED with high
    fetchability and ~70 observations of a single metric).
  - **Cross-plan canvas.** Pin records from multiple plans into a
    persistent dashboard view — the original "situation room"
    framing. Carries product weight beyond a single research
    session.
  - **Recipe-iteration loop on observations.** When an
    observation_metric nomination fails (40 nominations × 5 trials
    showed many failure modes — `invalid type: string … expected
    f64`, `inner selector matched no elements`, `extraction
    returned 71737 bytes`, `fetch failed: 404/403/401`), the
    operator-facing affordance to inspect why and re-author the
    failed selector lives on `RecipesPanel`/`FetchReport` today
    but isn't wired tightly. A "this recipe is failing here,
    edit it" loop could turn the 40-noms-yielding-17-records ratio
    into 40-noms-yielding-35-records by recovering selector
    bugs interactively.

**(B) Open the classifier-bias gate.** ADR-territory work. Why
does the classifier prefer `observation_metric:N` for topics that
have obvious Events? Is the classifier prompt naming the six record
types asymmetrically, or is the topic→record-type mapping baked in
elsewhere? Reading the classifier prompt and the per-target
nomination derivation paths in `crates/pipeline/src/
research_classifier.rs` is the entry point. If the fix turns out
to be a prompt clarification, that's a 1-trial validation; if it's
deeper (the topic→target_record_kind mapping logic), it earns an
ADR.

**(C) Open the recipe-author multi-field gate.** Also ADR-
territory. Add an extraction primitive that authors multi-field
records (Event, Relation, EntityAttribute) from a single page —
either by widening the `iterator + inner selectors` pattern that
broke on hurricanes, or by adding a per-record-type structured-
output mode to the recipe-author prompt. This is what would let
typed panels for Events / Relations / Entities ever populate.

**(D) Honest dashboard.** Current dashboard shows "0 events"
dimmed in the type-count strip and renders Events / Entities /
Relations / Documents / Assertions as collapsed pills when
non-zero — but per the Σ row above, those pills will essentially
never light up under the current classifier. The dashboard could
honestly say "this product authors Observations" and remove the
six-tile strip, OR keep the strip as aspirational (the schema is
six-typed; the strip reflects that). Operator-product decision.

## Discipline (carried from Session 54+)

- **Each commit is a reset target.** Session 59 has no commit.
  The handoff is the deliverable. Reverting nothing leaves nothing
  to revert; this is what "live observation only at the end"
  produces when the observations show the gate isn't open.
- **Schema-first.** The EventsTimeline design above is schema-
  grounded (closed `EventDirection`, embedded `ObservationContent`
  for magnitude). Whenever Events materialize, the rebuild reads
  off the same schema.
- **No-easy-wins; debug oversighted bugs.** The classifier-bias
  finding and the recipe-author multi-field gap are both ADR-
  territory observations. Neither gets a one-line prompt tweak in
  Session 60 unless the next session's investigation justifies one.
- **Eval-harness is the right tool for these questions.** Three
  topics × five trials each = fifteen data points; the cross-topic
  picture (Σ row) is more dispositive than any single trial.
  Future sessions investigating either upstream gate should keep
  using N=5 trials and read the Σ across multiple topics, not the
  single-topic mean.

## Cleanup / state

- **No uncommitted changes** in the workspace as of this handoff
  write (the operator's post-eval rsync wiped Stage 1's
  scaffolding; that wipe is the correct outcome).
- **JSONL files preserved** at:
  - `eval-runs/us-federal-reserve-monetary-policy-2026-20260511T100401Z.jsonl`
  - `eval-runs/2025-atlantic-hurricane-season-20260511T101820Z.jsonl`
  - (plus the existing lithium baseline)
- **Per-trial DBs** were NOT kept (`--keep-dbs` not passed); the
  succeeded recipes' record types can't be inspected post-hoc.
  Future sessions investigating classifier output should add
  `--keep-dbs` so the records themselves can be SELECTed from the
  per-trial DBs.

End of handoff.
