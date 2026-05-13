# Session 64 — Handoff

Session 64 opened on the v1.20 hurricane re-run Session 63's
handoff had queued (the ADR 0019 acceptance gate). Operator's
in-session Fed-volatility screenshot redirected the session
mid-stream: same-plan recipe was succeeding-then-failing on
`federalreserve.gov` with `content assembly: invalid type:
floating point '22.0', expected a string`. Three structural
moves landed.

## What Session 64 changed

### Fix — schema-aware coercion at content_assembly

[`crates/pipeline/src/recipe_apply.rs`](crates/pipeline/src/recipe_apply.rs)
gains `path_expects_string(record_type, path)` + `coerce_for_string_path
(value, record_type, path)`. Called in `build_record` between
`resolve_field_value` and `insert_at_path`. Hardcoded String-path
set per `RecordType`:

- Observation: `metric`, `unit`, `currency`, `period`
- Event: `event_type`, `headline`, `direction`, `magnitude.{metric,unit,currency,period}`
- Relation: `kind`, `from`, `to`, `magnitude.{metric,unit,currency,period}`

Root cause: `parse_extracted_scalar` is type-blind — every
numeric-looking leaf becomes `Value::Number`. When a binding's
target path is JSON String on the schema (e.g.
`Event.headline`), serde rejects the float at content_assembly.
Fed page leaf drifted between fetches from a date string to a
bare number; same persisted recipe, fresh bytes, hard failure.

8 unit tests + 2 end-to-end tests cover known-String coverage
across the 3 record types, the `Observation.value: f64`
negative case (must NOT stringify), already-string passthrough,
non-Number/non-String passthrough, and the Fed shape end-to-end
(numeric leaf → headline → record assembles).

### Instrumentation — eval-harness counts extracted_inner per trial

[`apps/eval_harness/src/main.rs`](apps/eval_harness/src/main.rs)
gains `recipes_persisted` + `recipes_with_extracted_inner` on
`TrialReport`. Count computed after `run_fetch_for_plan` returns
by querying `store.recipes_for_plan(plan_id)` and substring-
matching `"kind":"extracted_inner"` on the produces_json column
(robust because the serde tag is unique). Pre-fetch failure
arms leave both counters at 0 with an operator-filter note.

`RunOneTrialOk` struct replaces the body's three-tuple — same
shape, more legible at five fields.

### ADR 0019 → Accepted

[`docs/adr/0019-per-field-extraction-subspecs.md`](docs/adr/0019-per-field-extraction-subspecs.md)
status header updated. Session 64 hurricane eval (5 trials,
`eval-runs/2025-atlantic-hurricane-season-20260512T153257Z.jsonl`)
shows 2/5 trials with extracted_inner recipes (NHC trials 0+4).
Session 61 baseline: 0/10. Sufficient condition met. The
stronger signal (≥3 Event records per trial via multi-leaf) is
unmet but tracked as a follow-on improvement target rather than
an acceptance blocker — the bottleneck has moved from shape
recognition (which v1.20 carries) to selector quality at
authoring time, which is a different axis.

### ADR 0012 gate progress

`docs/failure_cases/class_b/` grew from 2 documented cases to 6.
Added four CssSelect-strict cases against `www.nhc.noaa.gov`:

- `2026-05-12_www_nhc_noaa_gov_trial0_css_inner_no_elements.md` (Session 64 eval, DB retained)
- `2026-05-12_www_nhc_noaa_gov_trial4_css_inner_no_elements.md` (Session 64 eval, DB retained)
- `2026-05-11_www_nhc_noaa_gov_trial0_css_inner_no_elements.md` (Session 63 eval, recurrence-only — DB was cleaned up)
- `2026-05-11_www_nhc_noaa_gov_trial3_css_inner_no_elements.md` (Session 63 eval, recurrence-only)

README updated with gate-status table. Strict Class B total: 5.
Modes covered: CssSelect (4) ✓ ≥2, RegexCapture (1, need 1 more
for predicate Condition 3), JsonPath (0), CsvCell (0). Toward
Condition 1's ≥10, need 5 more; specifically a JsonPath strict
and a CsvCell strict to satisfy mode-diversity. Condition 4
(Class C disguised as Class B) still has no documented case.

### ADR 0012 Condition 5 reconciliation

ADR 0012 §"Storage: recipe version chain" called the column
migration "v7"; it actually landed as v11
(`migrations/0011_recipes_prior_recipe_id.sql`) in Session 26 —
migrations 0001-0010 were used in the intervening sessions.
The functional substrate (column, RecipeRow field, StoredRecipe
field, `Store::recipe_lineage`, round-trip tests) is fully in
place. ADR 0012 §"Condition 5" updated with a Session 64 note
naming the actual migration file and the path to live
verification: the Fed re-author surface on recipe `019e1cbb`,
when the operator clicks through, writes the first non-NULL
`prior_recipe_id` in real data and closes the "verified in a
real run" half.

## Why the loop wasn't built

The Session 63 handoff queued the recipe-iteration-on-FetchReport
loop as the recommended Session 64 work. The data supports it —
both Session 64 NHC apply failures would feed it directly. **ADR
0012 explicitly forbids it** until the gate is met. Per the ADR's
own §"READ THIS FIRST — DO NOT DEVIATE":

> The single gate: 10 or more empirically observed,
> distinctly-shaped Class B failures across diverse sources and
> plan types, documented in `docs/failure_cases/class_b/`. We do
> not have that yet.

Building the loop early would automate guesswork — the predicate
strings need ≥2 spec-grounded cases each before they're
trustworthy. Session 64 contributes 4 toward CssSelect's
Condition 3 (2 spec-grounded once operator fills DB-query data,
2 recurrence-only). Honoring the ADR IS the no-easy-wins move
per standing operator feedback.

## Files changed

```
crates/pipeline/src/recipe_apply.rs                    (+~130 lines: helpers, doc block, build_record wire, 10 tests)
apps/eval_harness/src/main.rs                          (+~80 lines: TrialReport fields, RunOneTrialOk, match-arm rewrites)
docs/adr/0019-per-field-extraction-subspecs.md         (status header + Session 64 verification block)
docs/adr/0012-reauthor-on-failure.md                   (Condition 5 status update)
docs/failure_cases/class_b/README.md                   (gate-status table)
docs/failure_cases/class_b/2026-05-12_*.md             (2 new — DB retained)
docs/failure_cases/class_b/2026-05-11_*.md             (2 new — recurrence-only)
session64-eval.sh                                       (new — repeatable eval invocation)
SESSION_64_HANDOFF.md                                   (this file)
```

## Verification gate

- `cargo test --workspace`: passed on operator's Mac (Session 64 first half).
- 5-trial v1.20 hurricane eval ran cleanly:
  `eval-runs/2025-atlantic-hurricane-season-20260512T153257Z.jsonl`
  + `logs/session64-hurricane-eval-*.log`. records [0, 30, 0, 0, 1];
  recipes_with_extracted_inner [1, 0, 0, 0, 1].
- Per-trial DBs retained at
  `/var/folders/.../situation_room-eval-019e1cd1-e0b6-7563-bc45-bbe0e413eb65/`
  — operator fill-in pass uses these to ground the Session-64
  CssSelect case files' TBD spec + bytes sections.

## Session 65 candidates

In ADR-discipline order (the loop is still gated):

1. **Fill in Session 64 CssSelect case TBDs.** SQL queries are
   inline in each case file. Converts 2 cases from
   "recurrence-only" to "spec-grounded" and satisfies ADR 0012
   Condition 3 for the `selector matched no elements`
   predicate string.

2. **Verify the Fed coercion fix live + Condition 5.** Click
   re-author on recipe `019e1cbb`. The fix's narrowest live
   verification is the assembly succeeds at all; the structural
   gain is the first non-NULL `prior_recipe_id` chain in
   real data, closing ADR 0012 Condition 5's
   "verified in a real run" half.

3. **Hunt the JsonPath + CsvCell strict Class B cases.** A
   different plan topic (FEMA-style JSON API or a CSV-shaped
   source) likely produces these naturally. Eval them with
   `--keep-dbs` so the cases land spec-grounded.

4. **Reasoning-block-before-JSON prompt experiment.** Unblocked
   by ADR 0012 (it's prompt-only, not the loop). Targets
   selector quality at authoring time — the bottleneck Session
   64 surfaced. The 5-trial hurricane eval-harness path makes
   variance-bounded comparison against the v1.20 baseline a
   single command.

5. **The loop itself** when ≥10 documented cases + 3 modes +
   Condition 4 case + Condition 5 live verification all land.
   Probably Session 67–68 at current rate.

End of handoff.
