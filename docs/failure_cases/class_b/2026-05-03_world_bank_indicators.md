# 2026-05-03 — `world_bank_indicators` — null indicator value

> First concrete entry in `docs/failure_cases/class_b/`. Establishes
> the writeup convention; subsequent entries should follow this
> shape unless ADR 0012 §"Documenting observed Class B failures" is
> amended.

## Summary

Operator-machine live verification run on 2026-05-03, against the
pre-Session-24 baseline (no `endpoint_hint` for several sources;
`world_bank_indicators` did have a hint that worked). The
`world_bank_indicators` recipe was authored from real fetched bytes
(`authored_from = "fetched_bytes"`) — so the recipe author had
ground truth — but the resulting recipe failed at the Apply stage
because the value the JSONPath returned was JSON `null`, not the
`f64` the runtime tried to deserialize it as.

Taxonomy: **Class B-adjacent.** ADR 0012's strict Class B predicate
matches one of four specific strings (`matched 0 times`, `path
matched no nodes`, `selector matched no elements`, `no row matched
filter`). This failure's message is `invalid type: string "null",
expected f64`, which is not one of them. The root cause has the
same shape as Class B — the LLM's recipe assumed something about
the source's response that turned out not to hold at runtime — but
the surface is different (the extraction *did* match a node; the
node held `null`). The taxonomy ambiguity is itself useful: it
hints that the deferred predicate may need to grow before the
gate is met. **Do not extend the predicate based on this single
case** — ADR 0012 §"Class B detection predicate" requires ≥ 2
observed live failures per predicate string.

## Source id and plan topic

- **Source id**: `world_bank_indicators`
- **Plan id**: `019dee87-2ba9-7dc2-b86f-4b6d820abe7e`
- **Topic**: `venezuela oil production`
- **Recipe id**: `019dee88-2ed7-76c3-a388-77eb96cbe891`

The plan nominated six sources; this one was the highest-priority
authoritative source for country-level macroeconomic indicators
relevant to oil production (Venezuela is a particularly empirically
fraught WB query subject because of sanctions-driven reporting gaps
that produce `null` values frequently — the failure is somewhat
expected for this country specifically).

## Extraction mode and the failing spec

- **Mode**: `json_path`
- **Spec**: *operator to fill in from the desktop app's recipe
  detail panel in `apps/desktop/src/components/RecipesPanel.svelte`
  — the live run captured the failure message but not the recipe
  body in the writeup author's view.*

The most plausible spec, given the World Bank Indicators API shape
documented at
[`datahelpdesk.worldbank.org`](https://datahelpdesk.worldbank.org/knowledgebase/articles/898599-indicator-api-queries),
is something like:

```json
{
  "mode": "json_path",
  "path": "$[1][0].value"
}
```

— pulling the first data row's `value` field from the standard
World Bank indicator response shape `[ {meta}, [ {row}, ... ] ]`.
Replace this with the actual spec when filling the writeup in.

## The failure message verbatim

```
content assembly failed: observation content: invalid type: string "null", expected f64
```

(From `situation_room_pipeline::fetch_executor` warn log at
`2026-05-03T15:54:10.390785Z`.)

The error originates inside the `recipe_apply` runtime's
content-assembly step: the JSONPath extractor returned a value
(the JSON `null`), `serde_json` serialized that value's runtime
representation back to a `String` for the deserialization step,
and the typed Observation's `content: f64` rejected it.

## The first 512 bytes of the fetched content

Fetched URL (from the recipe's `endpoint_url`): inferred to be a
World Bank Indicators API call of the shape
`https://api.worldbank.org/v2/country/VEN/indicator/{INDICATOR}?format=json`.
The exact indicator code the recipe author chose is in the recipe
body the operator can read off the GUI; for the schema-shape
discussion below it doesn't matter which.

Representative response shape (from World Bank's published API
documentation; the live response on the operator's run was not
captured but the shape is stable):

```json
[
  {
    "page": 1,
    "pages": 1,
    "per_page": 50,
    "total": 64,
    "sourceid": "2",
    "lastupdated": "2025-..."
  },
  [
    {
      "indicator": { "id": "NY.GDP.MKTP.CD", "value": "GDP (current US$)" },
      "country":   { "id": "VE", "value": "Venezuela, RB" },
      "countryiso3code": "VEN",
      "date": "2024",
      "value": null,
      "unit": "",
      "obs_status": "",
      "decimal": 0
    },
    ...
  ]
]
```

The `null` in the `value` field is the canonical "no data" signal
in the WB API. It is not an error; it is the correct response when
WB's data team has not received or validated a value for the given
country / year / indicator. Venezuela's recent series have many
nulls because of the country's statistical-office reporting decline.

## Whether re-authoring succeeded, failed, or oscillated

**Pending.** Re-authoring was not attempted in this session; the
manual-practice protocol per ADR 0012 §"Manual-practice protocol"
calls for the operator to choose between:

1. Re-author with a different indicator code that has fewer null
   values for Venezuela (e.g. an indicator the WB sources from
   OPEC reports rather than national accounts).
2. Re-author with the same indicator but accept the null with a
   clean "no data" outcome rather than treating it as a fetch
   success.
3. Address the underlying type mismatch in `recipe_apply` (treat
   JSON null as `Option::None` rather than letting deserialization
   fail) — but this is a runtime change and ADR 0007 is strict
   that the runtime path is LLM-free, so the change would be
   purely deterministic and orthogonal to re-authoring. Worth
   considering as a separate session.

The operator's call. Re-running the live verification after Session
24's `config/sources.toml` patch lands will not change this case
(the WB hint already produced a fetched-bytes recipe; the failure
is downstream of authoring). It will, however, change the four
*other* failures observed in the same run, which were stub-
authored against the no-hint baseline.

## The corrected extraction spec if re-authoring succeeded

**Pending.** Fill in when (5) is no longer pending.

## Cross-cutting observation

This failure is independent of Session 24's P1 patch (endpoint-hint
coverage). It is not caused by, fixed by, or made worse by the
hint changes. It is an evidence point under ADR 0012's gate, which
remains at 0 / 10 distinct shapes prior to this writeup and 1 / 10
after — though the predicate-string ambiguity (§"Summary" above)
means strict-Class-B-by-the-predicate count remains 0 / 10.

The remaining four Apply-stage failures from the same run
(`imf_weo`, `ofac_sdn`, `comtrade`, `gdelt`) were either
stub-authored (the first three; Session 24's hint coverage is the
fix) or transient (gdelt's HTTP 429, runtime rate-limit, not a
recipe defect). None of those four belong in this archive.
