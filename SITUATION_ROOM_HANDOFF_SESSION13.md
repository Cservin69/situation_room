# situation_room — Session 13 handoff

You are picking up after Session 12. Read this document, the
Session 12 handoff, and ADR 0007 §"runtime path" + the new
Session-12 review note before writing any code. ADRs 0007 and
0011 remain authoritative.  in the meantime Ervin refactored most of the things 
from Stockpile to Situation Room as he hated it because he fighted with us as 
we deviated always towards mineral specific queries

## What Session 12 shipped

P1 (real-plan run against the existing P2.5 panel) was effectively
done by the user before the patch landed: two real classifications
(`hungary's frozen EU funds` and `italy gdp`) were run end-to-end,
the recipes panel rendered correctly, and the fetch runs surfaced
two new failure shapes that the carry-over notes below capture.

P2 (CssSelect promotion) and P3 (ADR 0007 amendment) shipped as
`session12_p2_patch.tar.gz`:

- **CssSelect dispatch wired.** `ExtractionSpec::CssSelect` now
  routes through `run_css_recipe` (fetch → apply → insert),
  structurally identical to the CSV and JSON paths. Three of five
  modes are now wired (CsvCell, JsonPath, CssSelect); RegexCapture
  and PdfTable still surface as `Skipped { reason }`.
- **Skipped-mode canary updated.** The
  `_skips_unwired_extraction_modes` test re-pointed from CssSelect
  to RegexCapture. CssSelect was the canary in Sessions 8–11.
- **Two new tests.** `run_fetch_for_plan_succeeds_against_css_recipe_without_calling_llm`
  (happy path: HTML body + `td.prod` selector → 49,000 → 49000.0
  → Observation) and
  `run_fetch_for_plan_reports_apply_failure_on_unmatched_css_selector`
  (selector matches nothing → `FailureStage::Apply`).
- **ADR 0007 amendment.** New 2026-04-30 review note covering
  CssSelect promotion + the date-keyed-object known limitation
  from the Session 11 IMF case. Enumerates four response options
  for the date-keyed shape in increasing cost; none taken in
  Session 12. Also names two adjacent failure shapes
  (country-code format inconsistency, JSONPath syntax synthesis
  errors) as carry-over for the failure-mode taxonomy that ADR
  0012 will eventually need.

P4 (recipe-author prompt v1.4) deliberately not done. The
Session 5/12 discipline holds: prompt revisions wait for clear
recurrence patterns, not single data points.

## Build / test state at end of Session 12

If `session12_p2_patch.tar.gz` applied cleanly:

- 113 + 3 unit tests in pipeline + api (the two new CSS executor
  tests; api unchanged from P2.5).
- All 4 live tests still ignored.
- `cargo check`, `cargo test`, `cargo clippy --workspace
  --all-targets -- -D warnings` all green.

If clippy flags anything, the most likely culprits are pre-existing
lint drift in code untouched by this patch (the Session 10/11
fixes-patches saga gives the precedent). The patch itself is two
small adds (one helper function, one test helper, two tests) plus
a doc-only amendment, so the surface for new lints is narrow.

## What Session 13 should do

In rough order of leverage. The list is long because Session 12
was small; pick two or three, ship them clean, write the next
handoff.

### P1 — Real-plan CssSelect run

The Session 12 handoff suggested "EU AI Act enforcement" as a
plausible eur_lex target now that CssSelect is wired. Five-minute
exercise: classify, accept, run fetch, observe what happens in the
recipes panel and the fetch report.

The likely outcomes:
- Recipe authored against an EUR-Lex HTML page; selector reaches
  apply; either matches and produces a record, or matches nothing
  / matches multiple things and reveals a recipe-author-prompt
  weakness.
- Recipe author pre-fetches the endpoint hint, sees real bytes,
  generates a plausible selector against them. The Session 10
  Option-F pre-fetch is doing real work here.
- Or: the LLM picks a different binding (CsvCell, JsonPath) for
  eur_lex despite the HTML endpoint hint. Worth noting.

The diagnostic surface is now reasonable: the recipes panel shows
the authored selector, the fetch report shows where it failed.
DuckDB spelunking should not be needed.

### P2 — Per-recipe outcome on each recipe card

This was Session 12's deferred Option-D-in-new-shape. With the
recipes panel landed, the next ergonomic improvement is showing
the *most recent fetch outcome* on each recipe card so the user
can see at a glance which recipes are working and which aren't.

The matching is by `recipe_id` — both `RecipeDto` and
`RecipeOutcomeDto` already carry it. Rough shape:

- Frontend-only change. No new Tauri commands; the data is already
  in `plans.fetchReport.outcomes` and `plans.recipes`.
- A small badge per card: `Succeeded (3 records)` / `Failed @
  Apply` / `Failed @ Fetch` / `Skipped (regex_capture)` / `(no
  fetch run yet)`. Color from existing CSS vars.
- Click to expand into the failure message if Failed.

This is squarely a polish session. Half a day at most.

### P3 — Promote `RegexCapture`

If P1 doesn't reveal anything urgent and P2 lands quickly, this is
the next executor-wiring step. Mechanically identical to P2 of
Session 12: add a `run_regex_recipe` helper, route the dispatch
arm, update the canary test (PdfTable becomes the sole canary),
add happy-path + apply-failure tests.

Caveat: the Session 11 carry-over notes flagged that RegexCapture
is the "last-resort mode for sources that don't fit any structured
extractor." Whether real plans will ever bind a recipe to it is an
open question; in practice it might only land via the LLM's
failure modes ("I couldn't find a structured way to extract this,
so here's a regex"). Worth wiring it anyway to close the
unwired-modes-canary chapter, but expect it to see less production
use than CssSelect.

After P3, only PdfTable remains unwired, and PdfTable's
`NotImplemented` arm is its own session per the 2026-04-22 ADR
review note.

### P4 — ADR 0012 (re-author on failure)

The deferred Option C from Session 8. The Session 11 production
run plus the Session 12 production runs have now produced enough
distinct failure shapes to start sketching the failure-mode
taxonomy:

- **Class A: knowledge applied inconsistently.** CH vs CHE in
  the Swiss-debt run. (Session 11.)
- **Class B: synthesis of plausible-but-invalid syntax.**
  - JSONPath `1[0].value` (missing `$.` and separator) — Session 11.
  - JSONPath `$.['NGDP@WEO'][43]` (quoted-bracket form
    `jsonpath_rust` rejects) — Session 12.
- **Class C: endpoint-shape mismatch with extraction vocabulary.**
  IMF `$.values[-1]` (date-keyed object, no last-key semantics) —
  Session 11. Documented in the Session-12 ADR 0007 amendment.
- **Class D (new with Session 12 italy gdp run): URL pattern
  mismatch.** The "italy gdp" plan produced a recipe that fetched
  with HTTP 400. Without the recipe text in front of me as I write
  this, I can't classify whether it's a Class A (knew the right
  pattern, applied wrong) or a new class. Worth examining the
  stored recipe via the panel before sketching ADR 0012.

ADR 0012 should taxonomize these and propose a re-authoring
contract that includes the failure context (stage, message,
attempt count). Re-authoring on Class A or B failures is plausible
within one retry; Class C is structural and shouldn't trigger
re-auth at all. The ADR is the place to draw those lines clearly.

This is meaningful work. Probably two sessions: ADR + Option C
implementation.

### P5 — Fix the "0 recipes attempted" footgun from the Hungary
run

The Session 12 production log shows "hungary's frozen EU funds"
classified, accepted, fetched → 0 recipes attempted. The likely
diagnosis: no source in `config/sources.toml` has authoritative
coverage for "EU funds frozen" topics, so the classifier's
`document_sources` hints didn't bind to any registered descriptor
during authoring, so `load_or_author_recipes` had nothing to do.

The current behaviour (fetch run completes with 0 records and a
clean exit) is technically correct but ergonomically poor — the
user has no signal that *no recipes were even tried* vs *recipes
were tried and all failed*. A surface-level fix: the fetch report
could include a `unmatched_sources` count, or the report panel
could render a distinct empty-state message when
`recipes_attempted == 0`. Either is small. Either is also
adjacent to ADR 0007's deferred CoverageReport — worth thinking
about whether to address it locally now or wait for the broader
coverage-reporting design.

### P6 — Session 11 carry-overs still unresolved

- **Anthropic provider stub.** Worth looping back when xAI
  classification quality plateaus or rate limits become real.
- **PdfTable extractor.** Long tail. Unlocks USGS / EIA / OECD.
- **`SecureHttpClient` doesn't surface response headers.** Affects
  authoring-time content-type sniffing (could let the recipe
  author know HTML vs JSON vs CSV without parsing). Cheap.

## Hard rules (carry-over, unchanged)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All
  HTTP through `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test --package situation_room-api`.
  Never hand-edit *for long*.
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Closed enum of 5 extraction modes. Adding a sixth needs an ADR.
  (Session 12 reaffirmed this in the ADR 0007 amendment.)
- UUIDv7 + dedup_key for identity.
- Code validates format, prompt teaches content. The LLM is
  trusted for what to put in the recipe; the code is responsible
  for what shape it must take.
- When the user pushes back on a category of work that "keeps
  clinging back", purge it.
- The wire schema between Rust and the frontend lives in
  `crates/api`. Pipeline types are internal. `From` conversions
  sit at the boundary so internal type changes produce a single,
  obvious place to update the wire shape.

## What Session 13 is explicitly NOT

- **Not a sixth extraction mode.** The closed-enum invariant
  matters; the Session-12 ADR 0007 amendment enumerates four
  response options to date-keyed shapes and option (4) — adding
  a mode — is explicitly the most expensive and least warranted.
- **Not vocabulary expansion in any form** without an ADR pass.
- **Not coverage reports** as a full feature. The "0 recipes
  attempted" ergonomics in P5 are local; full coverage reporting
  is later.
- **Not multi-plan / background fetch.** One plan, one click.
- **Not editing recipes from the UI.** Re-classify if a recipe
  is wrong.
- **Not v1.4 of the recipe-author prompt** unless P1 produces
  the third recurrence of one of the Class-A or Class-B failure
  shapes from the ADR 0007 amendment.

## Files to read first when starting Session 13

In order of importance:

1. This file.
2. `situation_room_HANDOFF_SESSION12.md` — architectural primer.
3. `docs/adr/0007-research-function.md` — especially the new
   2026-04-30 review note at the end. The four-option taxonomy for
   date-keyed shapes is the closest existing precedent for
   thinking about Class-C failures.
4. `crates/pipeline/src/fetch_executor.rs::run_one_recipe` — the
   dispatch arm that now routes three of five modes. Mirror the
   pattern when wiring RegexCapture in P3.
5. `crates/pipeline/src/fetch_executor.rs::run_css_recipe` — the
   newly wired mode, structurally identical to its CSV/JSON
   siblings.
6. `apps/desktop/src/components/RecipesPanel.svelte` — for the
   per-recipe-outcome badge work in P2.
7. `apps/desktop/src/components/FetchReport.svelte` — for the
   "0 recipes attempted" ergonomics in P5.

## Continuity note

Session 12 was small by design (CssSelect wiring + ADR amendment;
two files). The handoff was deliberate about not chasing every
queued item. That discipline matters more as the codebase grows
— "ship two things clean, write the next handoff" beats "ship
seven things, none cleanly."

The user (Ervin) has been rigorous about security, prefers
honesty about uncertainty over false confidence, and reacts well
to direct disagreement when warranted. The "do not deviate"
discipline holds. If you want to deviate, say so and explain why.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments
and the ADR cross-references — the comments aren't decoration,
they're load-bearing for the next reader.

End of handoff.
