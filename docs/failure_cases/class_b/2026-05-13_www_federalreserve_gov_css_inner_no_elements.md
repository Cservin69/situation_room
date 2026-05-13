# 2026-05-13 — `www.federalreserve.gov` — css_select inner-selector matched no elements

> **Strict Class B** by ADR 0012's definition. Predicate string
> `selector matched no elements` (the CssSelect predicate) fires on
> the failure message verbatim. Captured from the live re-author flow
> in the operator's morning-of-2026-05-13 desktop session (Session
> 65 chat screenshot). Adds **host diversity** to the CssSelect
> Condition 3 evidence: the prior four CssSelect cases are all
> `www.nhc.noaa.gov`; the Fed case breaks the host-monoculture and
> is the first non-NHC strict CssSelect entry.

## Summary

Operator classified a "federal reserve interest rate policy" plan
in the morning desktop session. Stage-2 fetch executor authored a
recipe (`019e1ffc`) against `www.federalreserve.gov`. Apply ran
against fetched bytes; the iterator's CSS selector matched cards
on the page, but the inner-leaf selector hit zero descendants. The
failure_message reproduced the Session 64 NHC predicate text
verbatim — "the iterator's selector matched a card, but the inner
selector found nothing inside it. Likely cause: the inner selector
is targeted at a sibling rather than a descendant of the
iterator's match."

Operator clicked the re-author surface; a fresh recipe (`019e1fff`)
was authored with `prior_recipe_id = 019e1ffc` and `reauthor_reason`
populated, then succeeded with 1 extracted record. The Track A UI
loop in ADR 0012 worked end-to-end on a real failure on a real
source. This is the first non-NHC instance of the Class B predicate
firing on bytes-grounded authoring against a top-tier government
domain (federalreserve.gov is in the classifier's preferred-source
set per ADR 0015 sources memory).

The Session 65 persistence bug (DuckDB writes vanished on
Ctrl-C-the-script-terminal shutdown path) destroyed the DB
underlying the morning screenshot. Spec + bytes excerpts below
must be re-captured against a Session 66 re-run after the SIGTERM
handler in `apps/desktop/src-tauri/src/main.rs` lands and the
write path survives.

## Source id and plan topic

- **Source id**: `www.federalreserve.gov`
- **Plan id**: *TBD — re-classify "federal reserve interest rate
  policy" in Session 66 desktop run; copy plan id from
  `research_plans.id` post-persistence-fix.*
- **Run id**: *TBD — `fetch_runs.id` for the same plan.*
- **Topic**: federal reserve interest rate policy
- **Failed recipe id (Session 65 screenshot)**: `019e1ffc` —
  evidence lost to Session 65 persistence bug; re-derive in
  Session 66 via the runbook in `scripts/session66_verify.sql`.
- **Succeeded re-author recipe id (Session 65 screenshot)**:
  `019e1fff` — same caveat.

## Extraction mode and the failing spec

- **Mode**: `css_select` (with iterator) — same shape as the four
  NHC entries.
- **Spec**: *TBD — re-derive in Session 66. Query against the
  workspace-root DuckDB after fixed-binary live re-run:*

  ```sql
  SELECT extraction, iterator, produces, endpoint_url
  FROM recipes
  WHERE endpoint_url LIKE '%federalreserve.gov%'
    AND succeeded = FALSE -- or join recipe_fetch_attempts
  ORDER BY authored_at DESC
  LIMIT 5;
  ```

  See `scripts/session66_verify.sql` Q3 for the join over
  `recipes` + `recipe_fetch_attempts`. The earlier Session 65
  evidence (`019e1ffc`) is not recoverable — the persistence bug
  wiped the row.

## Failure message verbatim

```
extraction [css_select]: inner selector matched no elements within
iterator match (the iterator's selector matched a card, but the
inner selector found nothing inside it). Likely cause: the inner
selector is targeted at a sibling rather than a descendant of the
iterator's match.
```

Matches the ADR 0012 predicate string `selector matched no elements`.
Captured verbatim from the operator's morning Session-65 screenshot
of the recipe-detail panel.

## First 512 bytes of the fetched content

*TBD — re-derive in Session 66.* The query:

```sql
SELECT SUBSTR(rfa.bytes_excerpt, 1, 512)
FROM recipe_fetch_attempts rfa
JOIN recipes r ON r.id = rfa.recipe_id
WHERE r.endpoint_url LIKE '%federalreserve.gov%'
  AND rfa.succeeded = FALSE
ORDER BY rfa.attempted_at DESC
LIMIT 1;
```

The bytes should be the Fed's rate-policy press-release index or
the FOMC statement archive page; the exact URL is in the recipe's
`endpoint_url`. The Session-65 morning capture showed cards-of-press-
releases shape on the page; the iterator selector matched the cards,
the inner selector aimed at a date sibling instead of a date
descendant.

## Re-authoring outcome

**Succeeded** (per Session 65 morning screenshot). Recipe `019e1ffc`
failed apply; operator clicked the re-author button; recipe
`019e1fff` authored with `prior_recipe_id = 019e1ffc` and
`reauthor_reason` populated; apply ran cleanly on the next attempt
and produced 1 record. The Track A UI in ADR 0012 works on real
data. The full recipe-history column in the UI rendered the
oldest-to-newest chain.

The actual rows did not persist to disk (Session 65 persistence
bug). Re-derivation in Session 66 should reproduce the shape
(non-determinism risk is small — same source, same recipe-author
prompt, same fetched markup).

## Corrected extraction spec

*TBD — re-derive in Session 66.* Query against the Session-66 DB:

```sql
SELECT extraction, iterator, produces
FROM recipes
WHERE prior_recipe_id IS NOT NULL
  AND endpoint_url LIKE '%federalreserve.gov%'
ORDER BY authored_at DESC
LIMIT 1;
```

Expected shape: inner selector swapped from sibling-of-card to
descendant-of-card. The Session-65 screenshot showed the correction
visually but the exact selector strings weren't transcribed.

## Connection to ADR 0019

Pending re-derivation. The Session-65 screenshot didn't reveal
whether the failed binding used `FieldValueSource::Extracted` or
`FieldValueSource::ExtractedInner`. Both shapes produce this
predicate string. Query post-fix:

```sql
SELECT produces_json
FROM recipes
WHERE id = <failed-recipe-id>;
```

Look for `"kind":"extracted_inner"` in the binding's source.

## Notes

- This case is **host-diverse from the prior CssSelect Class B
  entries** (NHC × 4) and thus is the most valuable single entry
  for ADR 0012's Condition 1 toward the ≥10 threshold: it proves
  the CssSelect inner-no-elements shape recurs across domains
  rather than being NHC-specific.
- The Session 65 persistence bug means the spec + bytes fields are
  marked TBD pending the Session 66 fixed-binary re-run. ADR 0012
  §"File naming and schema" item (5) — "Pending" while the manual
  fix hasn't been done — applies here, but in the inverse: the
  manual fix already happened (one recipe re-authored, one record
  extracted), but the database state didn't persist to capture it.
