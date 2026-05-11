# ADR 0018 — Target-bucket fairness: the executor's silent truncation of non-Observation expectations

**Status**: Proposed
**Date**: 2026-05-11
**Related**: ADR 0003 (six record types as governance boundary), ADR
0007 (research function: two-level LLM architecture), ADR 0011 (plan
lifecycle and fetch executor), ADR 0015 (LLM-emitted source
nominations), ADR 0016 (extraction iterator: Phase 1)

---

## Context

Session 59 set out to populate the records-dashboard's five non-
Observation typed panels by changing the topic from lithium to a
topic whose record-type distribution would surface Events. Two
topics were tried: "US Federal Reserve monetary policy 2026" and
"2025 Atlantic hurricane season". Across 15 trials (3 topics × 5
trials) the aggregate per-target nomination counts were:

| topic       | obs_metric | event_type | entity_kind | relation_kind |
|-------------|-----------:|-----------:|------------:|--------------:|
| Lithium     | 4          | 0          | 0           | 0             |
| Fed         | 24         | 0          | 0           | 0             |
| Hurricanes  | 12         | 1          | 0           | 0             |
| **Σ**       | **40**     | **1**      | **0**       | **0**         |

Session 59's handoff framed this as a classifier-bias finding: the
classifier reads every topic as a quantitative-indicators topic and
emits per-target nominations of essentially one shape. The hard
recommendation was that the typed-panel roadmap is bounded by the
classifier, not by topic choice.

Session 60 re-investigated and found that the classifier story is
**downstream of a more proximate structural cause in the fetch
executor**: every non-Observation expectation is silently dropped
before the LLM ever sees it, on every plan with four or more
`observation_metrics`. The Session 59 aggregate is consistent with
a classifier that emits a mix of bucket types but whose
non-Observation entries never reach the recipe author.

### The structural cause

[`crates/pipeline/src/fetch_executor.rs`](../../crates/pipeline/src/fetch_executor.rs)
constructs the per-nomination target list via
`build_target_expectations(plan, MAX_AUTHORS_PER_NOMINATION)`:

```rust
fn build_target_expectations(plan: &ResearchPlan, max: usize) -> Vec<ExpectationRef> {
    let mut out: Vec<ExpectationRef> = Vec::new();
    for i in 0..plan.expectations.observation_metrics.len() {
        if out.len() >= max { return out; }
        out.push(ExpectationRef::ObservationMetric { index: i as u32 });
    }
    for i in 0..plan.expectations.event_types.len() {
        if out.len() >= max { return out; }
        out.push(ExpectationRef::EventType { index: i as u32 });
    }
    for i in 0..plan.expectations.entity_kinds.len() {
        if out.len() >= max { return out; }
        out.push(ExpectationRef::EntityKind { index: i as u32 });
    }
    for i in 0..plan.expectations.relation_kinds.len() {
        if out.len() >= max { return out; }
        out.push(ExpectationRef::RelationKind { index: i as u32 });
    }
    out
}
```

The function concatenates the four record-typed buckets in fixed
declaration order — observation_metrics, event_types, entity_kinds,
relation_kinds — and truncates the concatenation to
`MAX_AUTHORS_PER_NOMINATION` (currently 4). The constant's
doc-comment is explicit about the consequence:

> So for a plan with 4 obs metrics + 3 event types + 2 entity kinds,
> the executor authors against the first 4 entries of the
> concatenation (all four obs metrics in this case); the remainder
> is silently uncovered until the operator either re-classifies
> (which yields fresh nominations) or raises the cap.

The lithium worked example in `config/prompts/research_classifier.md`
declares four `observation_metrics` (production, reserves,
refining_capacity, spot_price). The Fed and hurricanes plans likewise
declared four metrics each — verified by the per-nomination decline
log in `eval-runs/us-federal-reserve-monetary-policy-2026-…jsonl`,
which lists `:observation_metric:0`, `:1`, `:2`, `:3` for every
nomination that reached the inner authoring loop and **zero
`:event_type:N` entries on any nomination across any trial of Fed
or lithium**. The single `event_type:0` nomination on hurricanes
that Session 59 attributed to one trial appears only because that
particular plan had fewer than four metrics declared, so the cap
allowed one event-type slot through.

### Why the classifier-bias framing missed this

Session 59's aggregate count is the count of *per-target nominations
that reached the recipe-author stage*, not the count of expectations
the classifier emitted. Truncation happens between plan-emission
and per-target dispatch, and the eval-harness summary log doesn't
distinguish the two. The classifier may very well also be biased
toward observation framings (the v2.0 prompt's lithium worked
example fills four metrics and a fully-populated `event_types`
bucket; the OFAC and quantum examples each populate `event_types`
with two entries) — but the empirical signal Session 59 saw is
dominated by the truncation. Even a classifier that emits
event_types perfectly on every plan would yield Session 59's table
under the current executor.

This is also why the Session 56 lithium 5-trial run, the Session 57
ADR-0017-A run, and the Session 58 dashboard-population run all
showed Observations populating and the other five panels empty. The
empty panels were never being tried.

### What this is *not*

This ADR is not about how rich a plan's classifier output should be.
The classifier's prompt and its bucket-fill discipline are evaluated
separately, and any future revision of `research_classifier.md` to
encourage broader bucket fill should still encounter a fair
executor.

This ADR is not about the runtime application of recipes. Once a
recipe is authored against a non-Observation expectation, the
runtime applies it normally; the gap is solely on the path from
plan-emission to LLM-authoring.

This ADR is not about ADR 0016 Phase 2 (multi-extracted fields per
match). That is a separate gate that blocks magnitude-bearing
events, relations, and entity-attributes from being authored at
all; see ADR 0019. The two gates compose: the executor must dispatch
to non-Observation targets *and* the recipe shape must be expressive
enough to author against them. ADR 0018 opens the first gate; ADR
0019 opens the second.

## Decision

Replace `build_target_expectations`'s fixed declaration-order
concatenation with a **bucket-fair dispatch order** so that no
single bucket can starve the others under the per-nomination cap.

### The shape

Two changes, both confined to `fetch_executor.rs`:

#### 1. Fair-by-bucket ordering

Reorder the targets returned by `build_target_expectations` so that
the first N entries (where N is the number of non-empty buckets)
each draw from a different bucket. Subsequent entries fall back to
the original declaration order. Concretely:

```rust
fn build_target_expectations(plan: &ResearchPlan, max: usize) -> Vec<ExpectationRef> {
    // Bucket iterators in declaration order.
    let buckets: Vec<Vec<ExpectationRef>> = vec![
        (0..plan.expectations.observation_metrics.len())
            .map(|i| ExpectationRef::ObservationMetric { index: i as u32 })
            .collect(),
        (0..plan.expectations.event_types.len())
            .map(|i| ExpectationRef::EventType { index: i as u32 })
            .collect(),
        (0..plan.expectations.entity_kinds.len())
            .map(|i| ExpectationRef::EntityKind { index: i as u32 })
            .collect(),
        (0..plan.expectations.relation_kinds.len())
            .map(|i| ExpectationRef::RelationKind { index: i as u32 })
            .collect(),
    ];

    // Round-robin pass: one entry from each non-empty bucket, in
    // declaration order, until cap or every bucket exhausted.
    let mut out: Vec<ExpectationRef> = Vec::new();
    let mut indices = vec![0usize; buckets.len()];
    let mut any_progress = true;
    while out.len() < max && any_progress {
        any_progress = false;
        for (b, bucket) in buckets.iter().enumerate() {
            if out.len() >= max { break; }
            if indices[b] < bucket.len() {
                out.push(bucket[indices[b]].clone());
                indices[b] += 1;
                any_progress = true;
            }
        }
    }
    out
}
```

Worked example. A plan declaring 4 metrics + 3 event_types + 2
entity_kinds + 1 relation_kind, under `MAX_AUTHORS_PER_NOMINATION =
4`, today emits `[obs:0, obs:1, obs:2, obs:3]`; under this ADR it
emits `[obs:0, evt:0, ent:0, rel:0]`. The other six expectations
(`obs:1..3`, `evt:1..2`, `ent:1`) are not reached on a single
nomination — same cap — but each bucket gets one slot. Over 5–7
nominations on the same plan, the bucket-fair order surfaces at
least one extraction attempt against every non-empty bucket, where
the declaration-first order would surface zero against any bucket
beyond Observations.

#### 2. Lift the per-nomination cap, with cost framing

The cap exists to bound the per-fetch LLM bill: 1 propose-URL call
+ N author calls per nomination per attempt, with up to 3 attempts.
At `MAX_AUTHORS_PER_NOMINATION = 4` and a typical 5–7 nomination
plan with 3 attempts allowed, the worst-case authoring bill is
~50–80 author calls per plan run.

Raising the cap to **6** is the proposed adjustment. The arithmetic:
6 covers one slot per record-typed bucket plus two more in the
densest bucket, which under bucket-fair order yields `[obs:0, evt:0,
ent:0, rel:0, obs:1, evt:1]` for the four-bucket worked example.
Worst-case authoring bill rises to ~120 calls per plan run — still
inside the Workhorse-tier budget envelope established by Session 47
and confirmed unchanged by Sessions 53–57.

Six is the conservative choice. Eight would cover two slots per
bucket for a four-bucket plan, but the marginal value of the second
slot in a bucket the proposer wasn't tuned for is empirically
unclear. The cap can be raised again once Session 60+ produces
records-per-bucket data that's not dominated by the truncation
itself.

### Naming and call sites

The function name stays `build_target_expectations`; the cap
constant stays `MAX_AUTHORS_PER_NOMINATION`. The call site at
`author_for_nomination` is unchanged. The four existing unit tests
in `fetch_executor.rs` (Session 47, currently asserting declaration
order) are updated to assert bucket-fair order; one new test
covers the four-bucket-full case showing each bucket gets at
least one slot.

## Consequences

### Positive

- Non-Observation expectations stop being silently dropped. Plans
  declaring event_types / entity_kinds / relation_kinds get at
  least one extraction attempt per nomination per bucket.
- The dashboard's five collapsed-pill panels become *empirically
  testable* — Session 59's "zero events ever" finding becomes
  recoverable to "events were tried, here is the apply-stage
  failure rate" or "events succeeded N times."
- The signal Session 59 attributed to classifier bias becomes
  separately measurable. After this ADR lands, a classifier
  emitting four metrics + three event_types should produce
  per-target nominations of *both* shapes; a flat `event_type = 0`
  result post-fix is then attributable to the classifier rather
  than the executor.
- Existing recipes are unaffected. The change touches dispatch
  order, not recipe shape; pre-Session-60 recipes in storage
  continue to apply identically.

### Negative / costs

- Worst-case per-nomination LLM bill rises from ~28 to ~40 calls
  (one propose + 6 author × 3 attempts). At Workhorse-tier rates
  this is a ~40% increase in authoring-stage cost per nomination.
  The increase is upper-bounded by the cap; in practice most
  attempts decline early and don't run all 6 authors.
- Non-Observation buckets gain a "first impression" against
  sources the classifier nominated for Observation framings. A
  USGS PDF or a FRED CSV is unlikely to surface events, so the
  bucket-fair slot for `event_type:0` will frequently decline on
  topical mismatch. That's the right outcome — declines are
  honest — but it raises the per-plan decline count and the
  operator-facing FetchReport line count. The dashboard's pill
  row should expose the decline counts honestly (see ADR 0009,
  amendment territory) rather than hiding them.
- Per-bucket validation pressure shifts: today every non-OBS
  failure mode (selector-against-event-shaped-DOM, multi-field
  needs, dedup_key requirements for relations) is dormant. Under
  this ADR they become live. ADR 0019 (Phase 2 multi-field
  extraction) is the matching prerequisite for magnitude-bearing
  events, relations, and entity-attributes to actually author
  cleanly; without ADR 0019 the bucket-fair dispatch surfaces
  non-Observation declines at a high rate.

### Alternatives considered

**(a) Raise the cap without changing order.** `MAX_AUTHORS_PER_NOMINATION
= 8` and keep declaration-first concatenation. Adds budget but does
not address the structural bias: a plan with 8+ metrics still
truncates events. Rejected because the bias is order-of-iteration,
not budget.

**(b) Round-robin without raising the cap.** Use the round-robin
order at `cap = 4`. A four-bucket plan gets exactly one slot per
bucket; a three-bucket plan gets `[obs:0, evt:0, ent:0, obs:1]`.
Cheaper than (decision). Worth piloting before committing to the
cap raise — Session 61's first run under bucket-fair dispatch could
keep cap = 4 and let the operator decide whether the extra slots
are worth the budget.

**(c) Per-bucket weighting.** Weight buckets by some prior
(observations 2×, events 1×). Composable with bucket-fair, but the
prior is the question this ADR is trying to *measure* — picking it
analytically risks re-encoding the bias we're trying to expose.
Rejected as premature.

**(d) Per-bucket cap (e.g. ≤2 per bucket).** Caps the densest
bucket so the others can't be starved. Effectively round-robin
with a different formulation; the explicit round-robin in the
decision is simpler to reason about. Equivalent in steady state.

**(e) Source-priority-weighted dispatch.** Some nominations are
authoritative_primary (USGS), some are general_news (Reuters). A
news-shaped nomination is a better event candidate than a
statistical-agency PDF; a stats-agency nomination is a better
metric candidate. Per-nomination bucket-prior is appealing but
violates ADR 0007's closed-vocabulary discipline (the executor
must not encode source-routing heuristics). Rejected on principle.

## Validation

This ADR is empirically falsifiable: re-run the Session 59
hurricanes 5-trial eval after the Session 61 implementation lands.
Pre-fix: 1 `event_type:N` per-target nomination across 5 trials.
Post-fix expectation: ≥10 `event_type:N` per-target nominations
across 5 trials (one per nomination per trial, modulo nominations
that hit URL-discovery declines before reaching the inner authoring
loop). If the post-fix count is still in the single digits, the
classifier-bias hypothesis re-emerges and Session 62 picks up the
prompt-side investigation.

The dashboard's pill row should also light up:
- `events` pill: ≥1 per topic on average (vs. 0 today)
- `entities` pill: ≥1 on entity-shaped topics (lithium with named
  producers; Fed with named agencies)
- `relations` pill: still likely 0 without ADR 0019 (multi-field
  needed for `from` + `to` extraction)

## Status

**Proposed** (2026-05-11, Session 60). Implementation deferred to
Session 61 per the kickoff discipline that this session ships no
code. The four-track decision matrix in Session 60's handoff
records this ADR alongside ADR 0019 (multi-field extraction),
plus a sub-direction pick for A (Observations deepening) and a
dashboard-honesty decision for D.

End of ADR.
