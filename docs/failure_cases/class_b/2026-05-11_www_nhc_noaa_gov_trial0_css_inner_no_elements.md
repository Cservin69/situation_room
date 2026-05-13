# 2026-05-11 — `www.nhc.noaa.gov` (trial 0) — css_select inner-selector matched no elements

> **Strict Class B** by ADR 0012's definition. From the Session 63
> eval-harness run on the 2025 Atlantic hurricane season plan
> (JSONL: `apps/desktop/eval-runs/2025-atlantic-hurricane-season-20260511T160839Z.jsonl`).
> Two trials in that run (0 and 3) hit the same predicate string on
> the same source; together with the Session 64 trial-0 / trial-4
> sibling cases, the CssSelect strict count reaches 4 across two
> independent eval runs separated by 24 hours.

## Summary

Eval-harness 5-trial v1.20 hurricane re-run on 2026-05-11 (Session
63's measurement pass, before the cross-plan dashboard pivot
consumed the rest of that session). Trial 0 produced 1 persisted
recipe; that recipe failed at apply with the same "inner selector
matched no elements within iterator match" message Session 64
re-produced on 2026-05-12.

The recurrence is the load-bearing observation: the LLM, given the
v1.20 prompt and fresh classification on `www.nhc.noaa.gov`,
repeatedly authors an iterator-bearing recipe whose inner selector
targets a sibling rather than a descendant of the iterator's
match. Same predicate string, same diagnostic, multiple distinct
authoring calls.

## Source id and plan topic

- **Source id**: `www.nhc.noaa.gov`
- **Plan id**: `019e17cc-c50b-71b1-a753-7644d01781c9`
- **Run id**: `019e17cc-c512-7283-88f6-055b5277da1d`
- **Topic**: `2025 Atlantic hurricane season`
- **Trial**: 0 of 5 (eval run `019e17cc-34e1-7aa0-966f-07b3d0a7e49f`)

## Extraction mode and the failing spec

- **Mode**: `css_select` (with iterator)
- **Spec**: *Not available — Session 63's eval run did NOT pass
  `--keep-dbs`. The per-trial DuckDB at
  `/var/folders/rs/ztvlbr6n58s2mjs0lwsk2dv80000gn/T/situation_room-eval-019e17cc-34e1-7aa0-966f-07b3d0a7e49f/trial-0.duckdb`
  was removed by the harness's clean-exit path. The recipe is no
  longer recoverable.*

  This case therefore counts as a **partial** Class B observation
  — the failure shape (mode, predicate string, source, plan
  topic) is recorded, but the specific selectors that produced
  the miss are not. ADR 0012 §"Documenting observed Class B
  failures" requires the spec verbatim; this case provides the
  failure observation but not the predicate-grounding evidence
  the spec gives. Future runs against this same source should
  use `--keep-dbs` so the spec is recoverable.

## Failure message verbatim

```
extraction [css_select]: inner selector matched no elements within
iterator match (the iterator's selector matched a card, but the
inner selector found nothing inside it). Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match.
```

Identical to the Session 64 cases (predicate-stable across days).

## First 512 bytes of the fetched content

*Not available — same DB-cleanup reason as the spec.*

## Re-authoring outcome

**Not attempted.** Session 63's run was a measurement pass; no
remediation followed. The plan was deleted alongside the trial
DB at session end (no `--keep-dbs`).

## Corrected extraction spec

N/A.

## Why document a case with two TBD fields

ADR 0012 §"Documenting observed Class B failures" requires the
spec and the bytes. This case files them as Not-available with
the procedural reason (Session 63's harness run did not retain
the DBs). The case is still useful for the gate because:

1. **The failure shape is observed and labelled.** Future
   sessions deciding whether to extend the predicate know the
   `selector matched no elements` string fired against
   `www.nhc.noaa.gov` on 2026-05-11 in addition to 2026-05-12.
   Predicate-stability across two days on the same source is
   evidence the predicate is real, not a single-run artefact.

2. **The recurrence count matters even if the spec doesn't.**
   ADR 0012 Condition 1 wants 10+ distinct cases. The
   distinctness is established by the (plan_id, recipe_id) pair
   (different authoring calls, even if the source is the same).
   This case's plan_id differs from every other documented
   case's plan_id.

3. **It documents the discipline gap.** Future eval runs must
   pass `--keep-dbs` if their failures are to count toward the
   gate fully. This case is an explicit note that the gap was
   identified.

This file's status in the predicate-grounding sense is
"recurrence-only" — it counts toward Condition 1 but does NOT
count as one of the spec-grounded cases the predicate needs for
its `selector matched no elements` string to be evidence-based.
The Session 64 sibling cases (`trial0` and `trial4` on
2026-05-12) DO provide spec-grounded evidence pending the
operator's DB-query fill-in pass.
