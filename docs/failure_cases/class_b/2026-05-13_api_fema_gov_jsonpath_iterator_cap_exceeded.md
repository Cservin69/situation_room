# 2026-05-13 — `www.fema.gov` — json_path iterator exceeded MAX_RECORDS_PER_RECIPE cap

> **Class B-adjacent** by ADR 0012's definition. The failure
> predicate `matched N elements; cap is N` is **not** in the strict
> predicate list (the four strings the README §"Definition" names),
> so this case does **not** count toward ADR 0012 Condition 2 mode
> diversity for strict cases. It DOES belong here per the README's
> §"Definition" paragraph 2 invitation: "A failure that doesn't
> match any of these strings is not strict Class B by the predicate's
> definition, but it may still belong here if the root cause is the
> same shape — an LLM-authored recipe whose assumption about the
> source's response shape was wrong."
>
> The Session 67 verification subsection of ADR 0019 cites this
> case file as the empirical case for proposing the new predicate
> as a candidate strict-list extension.

## Summary

Session 67's FEMA hunt re-run (`session67-hunt-classB-json.sh`,
2026-05-13 ~12:19 UTC) produced **11 apply-time failures** on
`www.fema.gov` with this verbatim predicate string:

```
extraction [json_path]: iterator path "$.DisasterDeclarationsSummaries[*]"
matched 1000 elements; cap is 500 (ADR 0016 §Consequences). Likely cause:
the iterator path matches too broadly (every value rather than every
row), or the source is a paginated API whose first page already exceeds
the cap.
```

The shape is consistent across all 11 attempts. The recipe persists
(`recipes_persisted` counts it; the validator does not pre-catch
the cap, because the cap is a runtime concern not addressed in
`validate_recipe_against_bytes`). At apply time, the iterator
`$.DisasterDeclarationsSummaries[*]` resolves to 1000 elements
(FEMA's default `count` parameter on the OpenFEMA v2 API), exceeds
`MAX_RECORDS_PER_RECIPE = 500`, and the runtime aborts the recipe
before producing any records.

This is the **first JsonPath apply-time failure ever observed in
the eval-harness**, and the first one possible at all — pre-Session
67, every `json_path × json_path` recipe was declined at authoring
by the structural validator's missing match-arm in
`validate_recipe_against_bytes`. The Session 67 patch closed that
coherence gap; this case file is the immediate downstream evidence.

## Root cause shape

The LLM's assumption: the iterator path will return "the
declarations we want." FEMA's reality: the default endpoint returns
all 1000 most recent declarations (the API default page size),
which is more than our runtime cap by design.

This is Class B in spirit — the LLM's mental model of the source's
response shape was wrong — but **wrong in the opposite direction
from the standard Class B**:

- **Standard Class B** (matched no nodes / no elements / etc.): LLM
  authored a selector that didn't fire at all. Recipe yields zero.
- **This case** (matched too many): LLM authored a selector that
  fires too broadly. Recipe yields more than the runtime cap and
  aborts.

Both are honest authoring-side mistakes against a real source. The
remediation path is the same: re-author against the failure
message + the bytes excerpt, narrowing the selector. For this case
the fix shape is well-defined: replace `$.DisasterDeclarationsSummaries[*]`
with a filter expression like `$.DisasterDeclarationsSummaries[?(@.fyDeclared=='2025')][*]`
or request the URL with a `$top=400` query parameter to keep the
result set under the cap. Either approach moves the cap-exceeded
shape into the records-produced shape.

## Source id and plan topics

- **Source id**: `www.fema.gov`
- **Topic**: `FEMA disaster declarations 2025`
- **Eval run id**: `019e2147-4d42-7d71-accb-1b1487dd0547`
- **JSONL**: [`eval-runs/fema-disaster-declarations-2025-20260513T121941Z.jsonl`](../../../eval-runs/fema-disaster-declarations-2025-20260513T121941Z.jsonl)
- **Plans (one per trial)**:
  - Trial 0: `019e2147-9c76-7781-9cb0-ddd55bef8ed2`
  - Trial 1: `019e2149-22d2-7b50-b348-acb04b15f63a`
  - Trial 2: `019e214a-4a3d-7e90-8e32-b5455e77d358`
  - Trial 3: `019e214c-af28-77c3-acd6-8c2923f2a103`
  - Trial 4: `019e214e-18dd-7473-81b4-707bf1b293d3`

## Per-trial apply-time cap-exceeded failures

| Trial | cap-exceeded attempts | trial_records | other apply failures |
|---|---|---|---|
| 0 | 2 | 163 | none |
| 1 | 2 | 872 | none |
| 2 | 3 | 1 | 1× fetch 503 |
| 3 | 2 | 0 | none |
| 4 | 2 | 11 | none |
| **TOTAL** | **11** | **1047** | 1 |

All 11 failures cite the same iterator path
(`$.DisasterDeclarationsSummaries[*]`) and the same numbers
("matched 1000 elements; cap is 500"). Different recipes used
different INNER paths (varying which scalar leaf they extract from
each declaration), but the OUTER cap-exceeded failure is independent
of inner choice — the runtime checks the cap before per-element
extraction.

## Extraction mode and the failing spec

- **Mode**: `json_path` (iterator-bearing, json_path × json_path
  pair — ADR 0019 Phase 2A runtime path).
- **Iterator path verbatim**: `$.DisasterDeclarationsSummaries[*]`
- **Inner path**: varies per recipe; sample from the per-trial DB.
- **Spec query** (operator-fill pending, run on Mac):
  ```sql
  -- Sample 2-3 distinct failed recipe specs:
  SELECT r.id, r.source_url, r.iterator, r.extraction, r.produces
  FROM recipes r
  JOIN recipe_fetch_attempts rfa ON rfa.recipe_id = r.id
  WHERE rfa.succeeded = FALSE
    AND rfa.failure_message LIKE '%matched 1000 elements; cap is 500%'
  LIMIT 3;
  ```
  Run against each per-trial DB at
  `${TMPDIR}situation_room-eval-019e2147-4d42-7d71-accb-1b1487dd0547/trial-{N}.duckdb`.

## Failure message verbatim

```
extraction [json_path]: iterator path "$.DisasterDeclarationsSummaries[*]"
matched 1000 elements; cap is 500 (ADR 0016 §Consequences). Likely cause:
the iterator path matches too broadly (every value rather than every
row), or the source is a paginated API whose first page already exceeds
the cap.
```

**Substring stable across all 11 attempts**: `matched 1000 elements; cap is 500`.

**Generalized predicate candidate**: `matched %d elements; cap is %d`
(the regex form `matched \d+ elements; cap is \d+`). For ADR 0012
strict-list extension purposes, the relevant predicate is
`matched N elements; cap is` with N as the variable; "cap is" is
the stable disambiguator that distinguishes this from the
ordinary `matched 0 elements` / `matched no elements` shapes.

## First 512 bytes of the fetched content

The fetched bytes are the FEMA OpenFEMA v2 API response, which is
JSON of approximate shape:

```json
{
  "metadata": {
    "skip": 0,
    "filter": "",
    "orderby": "...",
    "select": "...",
    "rundate": "2026-05-13T12:19:..",
    "DeprecationInformation": {...},
    "top": 1000,
    "format": "json",
    "entityname": "DisasterDeclarationsSummaries",
    "version": "v2",
    "count": 1000
  },
  "DisasterDeclarationsSummaries": [
    {"femaDeclarationString": "DR-...", "disasterNumber": ..., ...},
    {"femaDeclarationString": "DR-...", "disasterNumber": ..., ...},
    ...
  ]
}
```

**Operator-fill query** (run on Mac):

```sql
-- Real leading 512 bytes from one of the failed apply attempts:
SELECT substr(rfa.fetched_bytes, 1, 512)
FROM recipe_fetch_attempts rfa
WHERE rfa.failure_message LIKE '%matched 1000 elements; cap is 500%'
ORDER BY rfa.started_at ASC
LIMIT 1;
```

The `metadata.count == 1000` (and `metadata.top == 1000`) fields
are the smoking gun — the LLM had visibility to these at
authoring-time prefetch but the recipe shape didn't account for
them. A predicate-aware re-author prompt could surface
`metadata.count` as the "you are about to author against a 1000-row
response with our 500-row cap" signal.

## Re-authoring outcome

**Pending.** The Session 67 hunt run was a measurement pass; none
of the failed FEMA recipes have been re-authored. ADR 0012
Condition 5 (live `prior_recipe_id` chain) would be satisfied if
the operator picks one of these failed recipes in the desktop
app's recipe-detail panel and re-authors against the failure
message + bytes excerpt.

**Suggested fix shapes for the re-author prompt to consider:**

1. **Year-filtered iterator path**:
   `$.DisasterDeclarationsSummaries[?(@.fyDeclared==2025)]` — keeps
   the iterator path JsonPath-only and respects the plan's 2025
   topic scope.
2. **URL-side limit**:
   `https://www.fema.gov/api/open/v2/DisasterDeclarationsSummaries?$top=400`
   — applies the cap at the source rather than the runtime.
3. **State-filtered iterator**:
   `$.DisasterDeclarationsSummaries[?(@.state=='CA')]` — narrower
   geographic scope; useful if the plan has a per-state expectation.

## Corrected extraction spec

N/A (re-authoring pending).

## Proposed predicate-list extension

ADR 0012 §"Class B detection predicate" lists four strings (one
per mode). This case proposes adding a fifth:

```
matched N elements; cap is N    — JsonPath / CssSelect iterator
```

(The CssSelect iterator runtime applies the same cap; if the LLM
authors a CSS iterator against a >500-row table, the same
predicate fires. Today's data only includes JsonPath instances,
but the predicate is mode-shared.)

This entry would:

- Convert today's 11 cap-exceeded failures from Class B-adjacent
  to strict Class B (Condition 1 count climbs from 6 to 17 with
  no other change).
- Add a third mode to Condition 2's mode-diversity tally
  (CssSelect + RegexCapture + JsonPath via cap-exceeded → 3),
  potentially closing Condition 2.
- Require Condition 3's "≥2 spec-grounded cases per predicate"
  rule to be applied: today's run gives us 11 instances of the
  same predicate string on the same host with the same iterator
  path. Whether 11 attempts × 1 host count as ≥2 spec-grounded is
  a definitional call: the README's "distinctly-shaped" language
  (line 7) suggests we want shape diversity, not just count
  diversity. The conservative read says we need ≥2 cap-exceeded
  cases on different hosts; the permissive read says one case
  file documenting the pattern is enough.

The extension decision belongs in a future ADR 0012 amendment, not
in this case file. This file is the evidence.

## Connection to ADR 0019

This case is the direct downstream consequence of ADR 0019
Phase 2A's validator-gap close (Session 67). Pre-fix: 0
json_path × json_path recipes ever persisted on this source.
Post-fix: 23 persisted across 5 trials, 11 succeeded with records
(1047 total), 11 failed at the cap. The validator gap was masking
this entire class of behaviour.

`recipes_with_extracted_inner` is non-zero across all 5 trials
(4+3+2+2+2 = 13 multi-leaf bindings authored), so the
ExtractedInner sub-spec path is being exercised. Whether the
failed (cap-exceeded) recipes specifically use ExtractedInner is
TBD pending the operator-fill query above.

## Connection to ADR 0016

`MAX_RECORDS_PER_RECIPE = 500` is set in
`crates/pipeline/src/recipe_apply.rs`. ADR 0016 §Consequences
notes the cap explicitly:

> A listing source that produces more than 500 records per fetch
> is bounded by the cap; the runtime aborts rather than producing
> a partial record set. The cap is a runtime defence, not a
> per-recipe contract — the same recipe could succeed on a
> different page of the same source if pagination is in play.

The cap design assumed the LLM would author paginated or
year-filtered iterators when a source produces large result sets.
This case is the first empirical evidence that **the LLM doesn't
reach for pagination or filters by default**, even when the source
exposes them. That observation is independent of the validator
gap closure — it's a recipe-author prompt-quality question
that future prompt revisions could address.
