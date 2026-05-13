# 2026-05-11 — `www.nhc.noaa.gov` (trial 3) — css_select inner-selector matched no elements

> **Strict Class B** by ADR 0012's definition. Fourth and last
> of the documented CssSelect predicate-matching cases against
> `www.nhc.noaa.gov` across the Session 63 + Session 64 eval runs.
> Spec + bytes status is the same as the trial-0 sibling case
> (Session 63's run did not retain DBs).

## Summary

Eval-harness 5-trial v1.20 hurricane re-run on 2026-05-11, trial 3.
Like trial 0, this trial produced 1 persisted recipe that failed
at apply with the predicate-matching error. The recurrence within
the same harness run (trial 0 + trial 3 hitting the same shape on
independent authoring calls) is itself evidence the failure mode
is reproducible, not a single-trial anecdote.

## Source id and plan topic

- **Source id**: `www.nhc.noaa.gov`
- **Plan id**: `019e17d8-749f-75f3-9034-921a7dc32e39`
- **Run id**: `019e17d8-74a5-75c2-98fd-6fa36e6596bd`
- **Topic**: `2025 Atlantic hurricane season`
- **Trial**: 3 of 5 (eval run `019e17cc-34e1-7aa0-966f-07b3d0a7e49f`)

## Extraction mode and the failing spec

- **Mode**: `css_select` (with iterator)
- **Spec**: *Not available — Session 63's eval run did not pass
  `--keep-dbs`; the per-trial DB at
  `/var/folders/rs/ztvlbr6n58s2mjs0lwsk2dv80000gn/T/situation_room-eval-019e17cc-34e1-7aa0-966f-07b3d0a7e49f/trial-3.duckdb`
  was removed by the harness clean-exit. Same recurrence-only
  status as the trial-0 sibling case — counts toward
  Condition 1, does not count as spec-grounded evidence for the
  predicate string.*

## Failure message verbatim

```
extraction [css_select]: inner selector matched no elements within
iterator match (the iterator's selector matched a card, but the
inner selector found nothing inside it). Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match.
```

## First 512 bytes of the fetched content

*Not available — same reason as the spec.*

## Re-authoring outcome

**Not attempted.** Session 63 measurement pass; no remediation.

## Corrected extraction spec

N/A.

## Cross-references

- **Same-run sibling**: `2026-05-11_www_nhc_noaa_gov_trial0_css_inner_no_elements.md`
  (intra-run recurrence).
- **Cross-day siblings**:
  `2026-05-12_www_nhc_noaa_gov_trial0_css_inner_no_elements.md`,
  `2026-05-12_www_nhc_noaa_gov_trial4_css_inner_no_elements.md`
  (predicate stability across days).

## Aggregate observation across the four NHC CssSelect cases

By plan_id distinctness:

| Date       | Plan id (first 12)         | Trial | DB retained |
|------------|----------------------------|-------|-------------|
| 2026-05-11 | `019e17cc-c50b`            | 0     | No          |
| 2026-05-11 | `019e17d8-749f`            | 3     | No          |
| 2026-05-12 | `019e1cd2-5836`            | 0     | Yes (--keep-dbs) |
| 2026-05-12 | `019e1cdb-d2b7`            | 4     | Yes (--keep-dbs) |

Four distinct authoring instances. Two with retained DBs (specs
and bytes operator-fillable). Two without (recurrence-only).

For ADR 0012's Condition 1 (10+ distinct Class B cases): these
four count as four (different plan_id values = different
authoring calls = distinct observations).

For ADR 0012's Condition 3 (predicate-string verification — "each
string must match at least two observed cases"): the `selector
matched no elements` predicate fires on all four. Two are
spec-grounded (the 2026-05-12 cases pending operator fill-in);
two are recurrence-only. The predicate has its ≥2 spec-grounded
observations once the operator fill-in is done, satisfying the
condition for this string.

Outstanding for the full predicate: the other three predicate
strings (`matched 0 times` for RegexCapture, `path matched no
nodes` for JsonPath, `no row matched filter` for CsvCell) each
still need ≥2 spec-grounded cases. The pre-existing `rss_feeds`
case (2026-05-03) provides 1 RegexCapture; the JsonPath and
CsvCell columns are empty.
