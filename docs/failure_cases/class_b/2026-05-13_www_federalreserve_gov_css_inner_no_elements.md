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

**Session 66 live verification (2026-05-13, post persistence-fix):**
operator clicked re-author on each of the two failing recipes
(`019e20b5-3881-...` and `019e20b5-4f36-...`, both
`ul.list-unstyled li` + `a` against the press-releases listing).
Both calls **landed `CommandError::ReauthorDeclined`** — the LLM
read the bytes + the failing selectors + the
`inner selector matched no elements` predicate and explicitly
declined to author a corrected recipe under the closed extraction
vocabulary.

The decline is architecturally correct, not a Track A regression:
the page's `<ul class="list-unstyled">` items do not directly
contain the `<a>` descendants those recipes' inner selectors
assumed. A *different* recipe authored from the same fetched
bytes — `019e20b5-1bca-...` with `div.Card-card` + `a.Card-title`
— succeeded with 57 records on the same page (the Press Releases
listing uses Card-card containers as its actual press-release-row
shape). Given only the failing recipe + bytes + failure_message,
the re-author LLM call cannot reach the Card-card pattern from
"correct the selectors of `ul.list-unstyled li + a`" alone; the
honest answer is decline. ADR 0012 §"Frontier LLM pushback
discipline" identifies this exact shape as a legitimate Track B
outcome at re-author time.

The pre-Session-66 backend squeezed `Declined` through
`CommandError::ReauthorFailed` with a `[declined]` prefix on the
message; the frontend handled it as a generic error, the dialog
stayed open, and the operator saw "the same message reappear"
with no clear signal. Session 66 landed the dedicated
`CommandError::ReauthorDeclined { prior_recipe_id, reason }` wire
variant + frontend arm; the dialog now closes cleanly and the
failed-apply row's `re-author` button is replaced by an italic
`declined: <reason>` badge that surfaces the LLM's prose verbatim.

Status of ADR 0012 Condition 5 (chain populated in real data):
**still pending** as of this case's outcome — both re-authors
declined, so no `prior_recipe_id`-stamped row exists. The Session
65 morning screenshot showed Track A succeeding end-to-end on an
*earlier* version of these recipes (`019e1ffc → 019e1fff`), but
that DB state was lost to the Session 65 persistence bug.
Subsequent live verification of Condition 5 is unblocked by the
fix here; a different plan (e.g. the 2025 atlantic hurricane
season) is a more likely candidate where the LLM's re-author call
authors rather than declines.

## Corrected extraction spec

**N/A — re-author was declined twice in Session 66.** No corrected
recipe was authored for either failing recipe. The case-file's
"Corrected extraction spec" section is left empty in honest
acknowledgment that the LLM, in the closed extraction vocabulary
and given only the prior recipe + bytes + failure message, judged
that no fix exists.

A *separate* recipe authored from the same bytes
(`019e20b5-1bca-7251-a1c8-1011ac469af7`) uses
`div.Card-card` + `a.Card-title` and succeeds with 57 records.
That is the working pattern on this page, but it's not a
"correction" of the failing recipes' selectors — it's a different
recipe entirely, authored at the original authoring step.

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
