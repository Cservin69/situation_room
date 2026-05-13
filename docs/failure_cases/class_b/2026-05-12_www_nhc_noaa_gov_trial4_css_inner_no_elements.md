# 2026-05-12 — `www.nhc.noaa.gov` (trial 4) — css_select inner-selector matched no elements

> **Strict Class B** by ADR 0012's definition. Same failure message
> as the trial-0 sibling case; this is a second independent
> authoring instance against the same source on the same plan
> topic. Two cases from the same run satisfy ADR 0012's
> "each predicate string must match ≥2 observed cases" rule for
> the `selector matched no elements` string in isolation. The
> two yesterday-cases (`2026-05-11_www_nhc_noaa_gov_*`) bring the
> CssSelect strict total to 4.

## Summary

Same eval-harness run as the trial-0 case, trial 4. The trial
produced 2 persisted recipes; 1 succeeded (`records=1`); 1 failed
at apply with the same "inner selector matched no elements" shape.
The succeeded recipe + the failed recipe co-exist in the same
trial's DB, which is useful for the manual diff pass — what did
the successful recipe's selector pattern look like vs the failing
one's?

This trial also has `recipes_with_extracted_inner = 1`. Either the
succeeded recipe or the failed recipe (or both) reaches for the
ExtractedInner shape. The per-trial DB carries the truth.

## Source id and plan topic

- **Source id**: `www.nhc.noaa.gov`
- **Plan id**: `019e1cdb-d2b7-72b3-b4eb-dc0e7f24dd7d`
- **Run id**: `019e1cdb-d2be-7f30-96ef-1e55bf0217c7`
- **Topic**: `2025 Atlantic hurricane season`
- **Trial**: 4 of 5 (eval run `019e1cd1-e0b6-7563-bc45-bbe0e413eb65`)

## Extraction mode and the failing spec

- **Mode**: `css_select` (with iterator)
- **Spec**: *TBD — query
  `/var/folders/rs/ztvlbr6n58s2mjs0lwsk2dv80000gn/T/situation_room-eval-019e1cd1-e0b6-7563-bc45-bbe0e413eb65/trial-4.duckdb`:*
  ```sql
  SELECT id, extraction, iterator, produces
  FROM recipes
  WHERE plan_id = '019e1cdb-d2b7-72b3-b4eb-dc0e7f24dd7d';
  ```
  Two recipe rows are expected. The operator pass should record
  both (the one that succeeded against bytes producing 1 record,
  and the one that failed) so a diff between them surfaces the
  selector-pattern difference between LLM-author-was-right vs
  LLM-author-was-wrong on the same source on the same trial.

## Failure message verbatim

```
extraction [css_select]: inner selector matched no elements within
iterator match (the iterator's selector matched a card, but the
inner selector found nothing inside it). Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match.
```

Identical to trial 0's. Confirms the predicate string `selector
matched no elements` is stable across independent authoring
instances against the same source.

## First 512 bytes of the fetched content

*TBD — same query shape as trial 0 against `trial-4.duckdb`. The
`recipe_id` filter in the JOIN identifies which recipe's fetched
bytes to extract.*

## Re-authoring outcome

**Pending.** The succeeded sibling recipe in the same trial proves
that *some* CSS selector pattern against this source's bytes does
land records (1 record produced). The failed recipe's
re-authoring should consult the succeeded sibling's selector
pattern as a structural hint without copying it verbatim — the
two recipes target different expectations (probably different
event_type indices, possibly different observation_metrics).

## Corrected extraction spec

N/A (re-authoring pending).

## Cross-references

- **Same-source sibling, this run**: `2026-05-12_www_nhc_noaa_gov_trial0_css_inner_no_elements.md`
  (independent authoring instance, same message text, same source,
  same plan topic).
- **Same-source siblings, prior run**:
  `2026-05-11_www_nhc_noaa_gov_trial0_css_inner_no_elements.md`,
  `2026-05-11_www_nhc_noaa_gov_trial3_css_inner_no_elements.md`.
  Both observed during Session 63's eval-harness run on
  2026-05-11. The 4-case CssSelect cluster is one indicator that
  `www.nhc.noaa.gov` is repeatedly LLM-confusable for the v1.19/
  v1.20 prompts at the inner-selector authoring step — even though
  some recipe variants do succeed (this trial's sibling +
  Session 63's trial 4 with 1 record).

## Pattern hypothesis (operator-confirm)

The NHC homepage's structure carries multiple parallel "card"-like
containers (the active-storm-information block, the recent
advisories list, the seasonal summary). The iterator selector
identifies one container family; the inner selector — likely
targeting per-card headline text — lands on a sibling instead of a
descendant. The Likely-cause-line in the runtime error message
names exactly this shape, which suggests the runtime's diagnostic
is well-calibrated to the actual failure mode.

A predicate-grounded re-author would feed the prefetch excerpt
plus the runtime's diagnostic to the LLM and ask for a
descendant-targeted inner selector. The Session 64
recipe-iteration-on-FetchReport loop work (deferred per ADR 0012's
gate) would automate that feedback path.
