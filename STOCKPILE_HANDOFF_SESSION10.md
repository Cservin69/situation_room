# Stockpile Session 10 Handoff

You are picking up after Session 9. Read this and ADR 0011 first;
both are authoritative. ADR 0007's "runtime path" section is also
worth re-reading because Session 9 expanded what counts as the
runtime path without changing its shape.

This is the **second version** of this handoff. The first was written
right after the Session 9 patch landed but before any production run
exercised the executor on a real plan. The first version recommended
B or F as Session 10's top priority. After watching one production
run, the recommendation has sharpened to **F first, then E**. The
"Production-run findings" section below is the data that drove that.

## What Session 9 shipped

The fetch executor now wires JSON-path extraction end-to-end. Before
Session 9, only `ExtractionSpec::CsvCell` produced records; everything
else surfaced in reports as `Skipped { reason: "<mode>: extraction
mode not yet enabled in executor" }`. After Session 9, `JsonPath`
joins CSV on the wired side; `CssSelect`, `PdfTable`, and
`RegexCapture` remain `Skipped`.

The patch was deliberately minimal:

- **`run_one_recipe` dispatch**: the `ExtractionSpec::JsonPath` arm
  now calls `run_json_recipe(ctx, plan, recipe).await`. Module-level
  docstring updated.
- **New `run_json_recipe`** in `crates/pipeline/src/fetch_executor.rs`,
  immediately after `describe_apply_error`. Structurally identical
  to `run_csv_recipe` — fetch via `HttpFetcher`, apply via the
  existing `apply()` boundary, insert via `Store::insert_record`.
  The two functions exist side-by-side; the docstring on
  `run_json_recipe` explains why (separation keeps `run_one_recipe`'s
  dispatch contract readable and lets the modes diverge without a
  flag-soup helper).
- **Tests**: removed `run_fetch_for_plan_skips_non_csv_extraction_modes`
  (which asserted JSON gets skipped) and added three:
  `..._succeeds_against_json_recipe_without_calling_llm`,
  `..._reports_apply_failure_on_malformed_json`,
  `..._skips_unwired_extraction_modes` (uses CssSelect as the
  representative; replace when CssSelect is promoted).
- **New `working_json_recipe` test helper** mirroring
  `working_csv_recipe`. Same field-mapping shape.
- **New live ignored test** mirroring the CSV one, with
  `FETCH_LIVE_JSON_URL` / `FETCH_LIVE_JSON_PATH` overrides.
- **New `json_demo` source descriptor** in `config/sources.toml`.

## Build / test state

Verified by the user post-extract:

- `cargo check --workspace`: green.
- `cargo test --workspace` (default, non-ignored): green.
- `cargo test --package stockpile-pipeline -- --ignored`:
  - `live_fetch_against_real_csv_produces_observation_and_closes_run`: ok
  - `live_fetch_against_real_json_produces_observation_and_closes_run`: **ok**
  - `live_author_recipe_against_xai_produces_valid_recipe`: ok
  - `live_classify_topic_against_xai_produces_valid_plan`: **failed** (see
    "Known intermittents" below)

3 of 4 ignored tests pass. The Session 9 deliverable (JSON wiring)
is independently green.

## Production-run findings — Session 9 patch under load

After applying Session 9 the user classified "bulgaria elections
2026", accepted the plan, and clicked Run Fetch. The plan had five
document sources; three with registered ids
(`gdelt`, `world_bank_indicators`, `eur_lex`), two prose-only
(Bulgarian Central Election Commission, Reuters/Balkan Insight/
Novinite). The two prose-only entries were correctly skipped at
authoring (`bound_source_ids` filters them out — empty
`preferred_source_ids` is treated as "no binding").

Three recipes authored, three different failure modes:

### gdelt → Failed @ Fetch: `https://example.invalid/gdelt`

The Level-2 author was given the synthetic URL placeholder
(`AuthoringContext::source_url = https://example.invalid/{source_id}`)
and **kept it** in the authored recipe rather than substituting a
real GDELT endpoint. Result: the runtime fetched against
`example.invalid`, which is a reserved-for-testing TLD that doesn't
resolve.

This is the synthetic-URL issue Session 8 flagged, Session 9 deferred,
and now bites in production for the first time. Two layers of fix
are needed:

1. The executor should pre-fetch a small excerpt of the real source
   and pass that to `AuthoringContext`. Session 3's demo binary had
   a richer pre-fetch step worth reviving.
2. The Level-2 prompt should teach the LLM to write the *real*
   source URL into the recipe, derived from the source's well-known
   endpoints, rather than echoing back any placeholder it sees in
   the context.

Without (1) the LLM has nothing to author against. Without (2) even
a real excerpt won't reliably produce a runnable URL. Both are part
of Option F.

### world_bank_indicators → Failed @ Apply: `observation content: invalid type: string "null", expected f64`

The most interesting outcome — pure runtime, no authoring problem.
The recipe was reachable, fetched bytes returned, the extractor ran
successfully and produced the literal string `"null"`. World Bank's
API returns the string `"null"` (not a JSON null literal) for
country-year combinations where no data exists. The recipe asked
for a missing combination.

This is **real-world drift in source content**, not a Stockpile
bug. The runtime caught it cleanly. ADR 0007's deterministic-runtime
promise held: bad content → typed error → audit trail.

But there's a real design question buried here, flagged below for
Session 10 to decide: should `"null"` (or a JSON `null`, or an
empty string) map to "no record produced" (silent skip at the
field level) or to a typed Apply failure (current behavior)?
Current behavior is honest; the silent-skip alternative is more
forgiving but obscures coverage signal. Discussion point, not a fix
to ship in Session 10.

### eur_lex → Skipped: `css_select: extraction mode not yet enabled in executor`

The cleanest outcome of the three. Eur-Lex has no public API; it's
a website. The Level-2 author correctly chose `CssSelect`, the
executor correctly reported `Skipped`, the report panel rendered
the typed reason. Working as designed.

### What this run tells us

- **0 of 3 recipes produced a record.** The fetch executor's
  effective record-production rate on a typical real-world plan is
  currently zero. F (pre-fetch + URL discipline) is the bottleneck.
- **None of the three recipes used JsonPath.** Session 9's JSON
  wiring did not exercise in production on this plan. It's working
  per the live test against `datasets/country-list`, but the real
  authoring distribution on this run was 2× CSV-or-similar and 1×
  CssSelect. Most authoritative sources on the open web are HTML.
- **CssSelect being unwired is now the second-largest gate on
  effective source coverage.** On this run it cost us 1 of 3
  outcomes. On any plan that nominates a webpage-only source
  (regulators, election commissions, court records, parliamentary
  publications, most government index pages), CssSelect is what a
  Level-2 author will pick.

## Session 10 priority — recommended order

The recommendation has sharpened from "B or F" (the previous version
of this handoff) to a clear **F first, E second**.

### Top: F — Pre-fetch excerpt for Level-2 authoring

Carried over from Session 8 → Session 9, now blocking. Two parts:

1. **Pre-fetch step in the executor.** Before calling
   `recipe_author::author_recipe`, the executor should fetch a
   small excerpt of the real source (a documented well-known URL
   per source-descriptor entry, or a HEAD-then-GET against the
   nominated endpoint) and pass that excerpt + the real URL into
   `AuthoringContext`. Session 3's deleted demo binary had this
   pattern and the code is worth reviewing in git history.
2. **Source descriptor extension.** `config/sources.toml` entries
   need an `endpoint_hint` (or similar) field naming a stable URL
   the pre-fetch can hit. Without it, the executor has no way to
   know what to fetch for "GDELT" — only that it's a real source
   id.
3. **Level-2 prompt patch.** The prompt should teach the LLM to
   write a real source URL into the recipe (derived from the
   excerpt and the endpoint_hint) rather than echoing any
   placeholder. The current prompt likely doesn't address this
   explicitly because the placeholder didn't exist when it was
   written.

This is a half-day to a day of work. It's the single change that
moves "0 records produced" toward "records produced per real plan."

### Second: E — Promote CssSelect

Same shape as Session 9's JSON work: flip the dispatch arm in
`run_one_recipe` from `Skipped` to a real `run_css_recipe`. The
recipe_apply runtime already handles `CssSelect`. Mirror the JSON
patch's tests; replace the `_skips_unwired_extraction_modes` test's
representative from CssSelect to PdfTable or RegexCapture.

Ship after F so that CssSelect recipes have real URLs to fetch
from. Without F, this is just adding a third way to fail at Fetch.

### Lower: B — Coverage report

ADR 0007 specifies a `CoverageReport` describing which expectations
were filled and which weren't. The plumbing exists in `research.rs`.
After F + E ship, real plans will start producing records, and the
coverage report becomes the right way to surface "did the plan get
covered." Today, with 0 records produced per run, the coverage view
would always show 100% gap and the user can already see that from
the bucket counts.

### Lower: D — Per-recipe outcome persistence

Unchanged in priority — useful, not urgent. Today's synchronous
report is enough for the immediate UX.

### Deferred: C — Re-author on failure

The World Bank `"null"` failure is exactly the shape of recipe drift
that re-authoring is designed to address. But ADR 0011 explicitly
defers this until the failure-mode taxonomy is better understood.
After F + E ship and we have weeks of real failure data, this
becomes the right next thing. If you take it on, it warrants ADR
0012.

## Things I'd flag for review (updated)

1. **Synthetic URL — promoted from "review" to "Session 10 P1."**
   See Option F above.

2. **The `"null"` Apply-failure design question.** When an extractor
   produces a sentinel-null value (literal string `"null"`, JSON
   `null`, empty string), what should happen? Current behavior:
   typed Apply failure, recipe-level outcome reported, no record
   produced. Alternative: field-level silent skip with a per-field
   "absent" marker, recipe still produces a record with the
   present fields. The alternative is more forgiving but obscures
   coverage signal. Pick a discipline before the next executor
   feature lands; a field-level absent-marker convention may
   require schema work.

3. **Known intermittent: live classifier test.**
   `live_classify_topic_against_xai_produces_valid_plan` failed
   once with `Vocab(InvalidUnit("metric tonnes LCE"))`. The xAI
   model produced a prose unit ("metric tonnes LCE", a real
   lithium-industry convention) where the prompt asks for a
   UCUM-compatible token (`t`, `kt`, etc.). The Unit validator
   correctly rejected it. Three options: do nothing (re-run, it's
   noise), patch the prompt with a worked example for industry
   prose units (LCE, Mt, tcf), or loosen the Unit validator to
   allow internal whitespace (ADR territory). Recommend
   "do nothing" until the failure is reproducible across multiple
   runs — Session 5's discipline ("prompt revisions are small,
   prompted by a real plan that came back weak, never
   speculative") applies.

4. **Boot-banner string drift.** The desktop binary boots with
   `"Stockpile desktop boots (Session 6 — GUI)"`. Cosmetic but
   real. One-character sweep when the next session touches
   `apps/desktop/src-tauri/src/main.rs`.

5. **Carried forward from prior sessions.** Anthropic provider
   stub, apply-runtime permissive deserialization, PdfTable
   unimplemented, authoring latency 30–60s, `SecureHttpClient`
   doesn't surface response headers, crate-level `#![allow(...)]`
   suppressions still need a sweep.

## Hard rules (carry-over)

- LLM-free at runtime. Period. Don't add provider calls inside
  `run_one_recipe` or below. The `UnreachableProvider` discipline
  in tests is the enforcement.
- DuckDB ALTER trap: never `ADD COLUMN ... NOT NULL DEFAULT ...` in
  one statement, and never use the split form on a table with
  indexes. Fresh tables in `CREATE TABLE` are fine — read v5's
  comment block.
- Patch packaging: tar.gz extracted at `~/Downloads/`, applied via
  `tar -xzf ~/Downloads/patch.tar.gz --strip-components=1 -C .`
  from the repo root. **No `#`-prefixed hashtag comments in
  copyable shell commands** — they break zsh.
- One `SecureHttpClient`. Never `reqwest::Client::new()`.

## What Session 10 is explicitly NOT (predictively)

- Re-authoring on failure (Option C; deferred, ADR 0011).
- Per-recipe persistence (Option D; deferred).
- Coverage report (Option B; deferred until F + E land).
- Background scheduling. One plan, one call, user-initiated.
- A complete UI for fetch-run history beyond the timeline strip.
- Promoting PdfTable or RegexCapture (PdfTable is `NotImplemented`
  per ADR 0007 Session-3 review note; RegexCapture is a long tail).

## Continuity note

Session 9 stayed on plan and shipped what it promised. The "all
green except classifier flake" / "0 of 3 recipes produced a record"
result on the production run is **not a Session 9 regression** —
the executor is correctly surfacing failure modes that pre-existed
in the architecture. Session 9 added one extraction mode to a
runtime that has multiple structural gaps; the production run made
those gaps visible. That's good news: visible gaps are fixable
gaps.

Session 10 should resist the same temptation Sessions 8 and 9
resisted: pick one item from the shortlist, ship it, write the next
ADR if it warrants one. F is the right pick. E ships next, after
F lands.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff (v2).
