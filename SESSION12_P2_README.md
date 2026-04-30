# Session 12 P2 — CssSelect promoted to wired

Wires `ExtractionSpec::CssSelect` through to the apply + insert
pipeline. Was queued in Session 10's handoff as P2, deferred in
Session 11 when the recipe-inspection panel (P2.5) became urgent.
Now done.

The `recipe_apply` runtime has supported CssSelect since Session 3
via the `scraper` crate; the `css_select_extracts_text`,
`css_select_extracts_attribute`, and
`css_select_errors_when_selector_matches_nothing` tests already
pass. What was missing was the executor-level dispatch + the
apply-and-insert plumbing. This patch adds that, mirroring the
existing CSV and JSON paths exactly.

Apply on top of the green Session 11 P2.5 build:

    tar -xzf ~/Downloads/session12_p2_patch.tar.gz --strip-components=1 -C .
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

Two new `#[tokio::test]`s; `_skips_unwired_extraction_modes` was
re-pointed at `RegexCapture` since CssSelect is no longer the
canary. Net test delta: +2.

## What this patch does

### `crates/pipeline/src/fetch_executor.rs`

- **Module docs.** "Extraction-mode policy in this session" updated:
  three of five modes wired (CsvCell, JsonPath, CssSelect),
  RegexCapture and PdfTable still skipped. New paragraph cross-
  references the Session 3 introduction of the CssSelect extractor
  in `recipe_apply` and explains why this was always an executor-
  side wiring step rather than a schema change.
- **`run_one_recipe` dispatch arm.** `ExtractionSpec::CssSelect { .. }`
  now dispatches to `run_css_recipe` instead of returning
  `Skipped`.
- **`run_css_recipe`.** New helper, structurally identical to
  `run_csv_recipe` and `run_json_recipe` (fetch → apply → insert,
  same FailureStage mapping, same one-record-fails-the-batch
  insert discipline). Doc comment cross-references the Session-9
  duplication-with-comments choice — keeping the dispatch in
  `run_one_recipe` honest about which modes are wired is worth
  more than line-saving via a generic helper.
- **`working_css_recipe` test helper.** Mirrors `working_csv_recipe`
  and `working_json_recipe`; only `extraction` differs. Selector
  is `td.prod`, no attribute (text mode).
- **`run_fetch_for_plan_succeeds_against_css_recipe_without_calling_llm`.**
  New happy-path test. HTML body has `<td class='prod'>49,000</td>`;
  `parse_extracted_scalar` strips the comma and produces `49000.0`,
  which flows into the Observation's `value` field — same end-state
  as the CSV / JSON success tests. Asserts the fetch_runs row was
  opened and closed cleanly with the expected counters.
- **`run_fetch_for_plan_reports_apply_failure_on_unmatched_css_selector`.**
  New failure-shape test. Body parses as HTML but the recipe's
  selector matches no elements; extraction errors at the apply
  stage. Mirrors the malformed-CSV and malformed-JSON apply-failure
  tests.
- **`run_fetch_for_plan_skips_unwired_extraction_modes`.** Updated
  to use `RegexCapture` instead of `CssSelect`. Comment updated
  to record the canary succession (CssSelect was the canary in
  Sessions 8–11 and was promoted in Session 12; PdfTable inherits
  the role if it ever lands as the next promoted mode would
  presumably be RegexCapture itself).

### `docs/adr/0007-research-function.md`

Appended a 2026-04-30 Session-12 review note containing two
amendments:

- **Amendment 1: CssSelect promoted from skipped to wired.** Records
  the executor-side wiring step. Notes that the closed-enum
  invariant is unchanged — promotion is wiring, not schema change.
- **Amendment 2: Known limitation — date-keyed object responses.**
  Documents the IMF `$.values[-1]` failure from the Session 11
  Swiss-debt run. Standard JSONPath has no `[-1]` semantics over
  object members. Enumerates four response options in increasing
  cost (steer the LLM toward array-shaped endpoints; pre-process at
  authoring time; extend the normalize stage; add a sixth extraction
  mode). None taken in Session 12; the amendment exists so the
  next person who hits this finds the failure-shape already named.

The amendment also notes two adjacent failure shapes the prompt
*may* need to address eventually (country-code format inconsistency
and JSONPath syntax synthesis errors), with the explicit note that
neither has crossed the threshold for a prompt-level intervention
yet. Both go in the failure-mode taxonomy that the deferred
ADR 0012 will need.

## What this patch does NOT do

- **Does not promote `RegexCapture`.** Out of scope. The handoff
  doesn't call for it and the canary needs an unwired mode to
  point at.
- **Does not address PdfTable.** Still `NotImplemented` per the
  2026-04-22 ADR review note. Long tail.
- **Does not add a sixth extraction mode** for date-keyed object
  responses. Amendment 2 of the ADR review note enumerates the
  responses; option (4) is ADR-level work, not warranted on one
  data point.
- **Does not bump the recipe-author prompt to v1.4.** Per Session
  5/12 discipline ("prompt revisions are small, prompted by a real
  plan that came back weak, never speculative"). The two
  recurrences of malformed JSONPath syntax are noted in the ADR
  amendment but stay below the threshold for a prompt-level
  intervention.
- **Does not add per-mode bespoke rendering to `RecipesPanel`.**
  Out of scope per the Session 12 handoff's "explicitly NOT" list.

## Verifying after apply

The new tests should bring the pipeline crate from 111 to 113
passing (no new ignored). All four live tests still ignored. End
state target:

    pipeline:    113 unit tests passing, 4 ignored
    api:          35 passing
    core:         31 passing
    llm:          10 passing, 2 ignored
    secure:       20 passing
    storage:      42 passing
    situation:     9 passing
    ----
    total:       260 unit tests passing, 6 ignored
    clippy:      green

For the end-to-end check:

1. `./scripts/run_desktop.sh`.
2. Classify a topic likely to bind to an HTML source — e.g. "EU AI
   Act enforcement" should bind `eur_lex`, whose `endpoint_hint`
   already points at the EUR-Lex search HTML page.
3. Accept the plan, click "Run fetch."
4. Watch the recipes panel: a CssSelect recipe should now reach
   the executor and either succeed (a record lands) or fail at
   `Apply` (selector didn't match) — *not* at `Skipped`.

The Session-11 production-run lessons apply: real HTML pages will
expose new failure modes the prompt + executor haven't seen yet.
That's what this work is for.

## Files in this patch

    crates/pipeline/src/fetch_executor.rs
    docs/adr/0007-research-function.md

Two files, both updates. No new files.
