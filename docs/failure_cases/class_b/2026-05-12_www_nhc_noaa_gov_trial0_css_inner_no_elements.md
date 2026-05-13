# 2026-05-12 — `www.nhc.noaa.gov` (trial 0) — css_select inner-selector matched no elements

> **Strict Class B** by ADR 0012's definition. Predicate string
> `selector matched no elements` (the CssSelect predicate) fires on
> the failure message verbatim. Documented from the Session 64
> eval-harness run on the 2025 Atlantic hurricane season plan; one
> of four CssSelect strict Class B cases produced by the same run
> against the same source.

## Summary

Eval-harness 5-trial v1.20 hurricane re-run on 2026-05-12, trial 0.
The LLM authored an iterator-bearing recipe against
`www.nhc.noaa.gov`. The iterator selector matched cards (the
runtime found N elements), but the inner-leaf selector didn't land
on a descendant of any matched card. The error message names this
exact diagnostic — "the iterator's selector matched a card, but the
inner selector found nothing inside it. Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match."

This is the apply-stage shape Class B was defined for: the recipe
was syntactically valid, the iterator structurally identified
something, but the per-row leaf assumption was wrong against the
actual bytes. Pre-fetch did happen (the bytes-aware authoring path
gave the LLM real markup), yet the inner selector still missed.
The bytes-aware authoring discipline (ADR 0014's `authored_from =
fetched_bytes`) is not sufficient on its own — the LLM can read
the right document and still pick a wrong descendant selector.

This case also carries the ADR 0019 multi-leaf marker: the trial's
`recipes_with_extracted_inner` counter is 1, meaning the LLM did
reach for the `FieldValueSource::ExtractedInner` shape. Whether
THIS recipe's failed binding uses ExtractedInner or sibling
single-leaf `Extracted` is TBD pending the per-trial DB inspection
below.

## Source id and plan topic

- **Source id**: `www.nhc.noaa.gov`
- **Plan id**: `019e1cd2-5836-7820-a9ca-0c8f315dd042`
- **Run id**: `019e1cd2-583c-7c40-bbba-0b3bf32d1630`
- **Topic**: `2025 Atlantic hurricane season`
- **Trial**: 0 of 5 (eval run `019e1cd1-e0b6-7563-bc45-bbe0e413eb65`)

## Extraction mode and the failing spec

- **Mode**: `css_select` (with iterator)
- **Spec**: *TBD — populate from the per-trial DB at
  `/var/folders/rs/ztvlbr6n58s2mjs0lwsk2dv80000gn/T/situation_room-eval-019e1cd1-e0b6-7563-bc45-bbe0e413eb65/trial-0.duckdb`.*
  Query:
  ```sql
  SELECT extraction, iterator, produces
  FROM recipes
  WHERE plan_id = '019e1cd2-5836-7820-a9ca-0c8f315dd042';
  ```
  The harness was run with `--keep-dbs`, so the trial DBs were
  retained at session end.

## Failure message verbatim

```
extraction [css_select]: inner selector matched no elements within
iterator match (the iterator's selector matched a card, but the
inner selector found nothing inside it). Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match.
```

Matches the ADR 0012 predicate string `selector matched no elements`.

## First 512 bytes of the fetched content

*TBD — query the same per-trial DB:*

```sql
SELECT substr(fetched_bytes, 1, 512)
FROM recipe_fetch_attempts
WHERE recipe_id IN (
    SELECT id FROM recipes
    WHERE plan_id = '019e1cd2-5836-7820-a9ca-0c8f315dd042'
)
ORDER BY started_at DESC
LIMIT 1;
```

The bytes are NHC's homepage or seasonal-summary page; the exact
URL is on the recipe's `source_url` field. Operator-fill pass
should record both the URL and the leading bytes.

## Re-authoring outcome

**Pending.** ADR 0012 §"Part 1" mandates a manual re-author path:
operator surfaces the failure into the conversation with the
fetched bytes + failure reason, frontier LLM authors a corrected
recipe, evaluator decides whether to persist. The Session 64
eval-harness run was a measurement pass, not a remediation pass —
none of the trial outcomes have been re-authored.

The trial 0 outcome is unrelated to the trial 4 outcome on the
same source: each trial used a fresh DuckDB and a fresh
classifier→author cycle, so the two recipes are independent
authoring instances (the recipe IDs differ even though the source
id is the same).

## Corrected extraction spec

N/A (re-authoring pending).

## Connection to ADR 0019

This trial's `recipes_with_extracted_inner` counter is 1: the LLM
authored at least one recipe that uses the multi-leaf
`FieldValueSource::ExtractedInner` shape. If THIS failing recipe
is the same one, then ADR 0019's Phase 2A apply path is implicated
— the inner-extraction sub-spec's selector is the one that
matched no elements. If it's a separate single-leaf recipe, ADR
0019's path is unaffected and the failure is the legacy Phase 1
iterator+inner-leaf shape.

The verification query (against the per-trial DB) distinguishes
the two: look at the binding's `field_mappings[*].source.kind` —
`extracted` is single-leaf, `extracted_inner` is multi-leaf. Both
shapes use a css inner selector against the iterator's match
scope, and both can produce this exact error message. The
predicate doesn't disambiguate them — that's the operator's job
during the manual classification step.
