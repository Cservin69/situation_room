# Stockpile — Handoff Document (Session 2)

**Date:** 2026-04-20 (end of second long session)
**Status:** Phase 3a backed out, demo in deliberate Document-only state,
ready to begin Phase 3c (LLM-authored FetchRecipe runtime).
**Tests:** 31 core + 12 pipeline + 17 storage + 6 sources = **66 passing**,
1 live test updated (still `#[ignore]`), green.

---

## READ THIS FIRST

**We almost wrecked the architecture.** I want this to be the first
thing any future Claude or contributor reads, so it is unmissable.

In Phase 3a Session B I wrote a regex-heuristic parser
(`crates/sources/src/adapters/usgs/parse.rs`) that extracted
"Observations" from the USGS MCS PDF text. The unit tests passed.
The live test passed. The demo ran. And then the human ran the
demo and saw the output:

```
country     year      production  unit
??          2021       7,240,000  t
??          2021       3,000,000  t
??          2021             480,000  t
AU          2021          88,000  t
CL          2021          49,000  t
...
```

Years wrong (2021 instead of 2023/2024). Most countries unidentified
(??). Suspicious rows where "2,023" — the year — was parsed as a data
value. Dedup also broken.

**The shortcut fought the architecture and lost.** ADR 0007 specifies
exactly this case: the LLM reads the PDF *once* at recipe authoring
time, produces a `FetchRecipe` with precise
`ExtractionSpec::PdfTable { page, table_index, row, col }` coordinates,
and the runtime applies the recipe deterministically forever. The
regex parser I wrote was a hand-rolled replacement for that — faster
to ship, impossible to get right, guaranteed to need per-commodity
per-year tweaks forever.

The human caught it with one question: *"why not the LLM should have
parsed the pdf and would have given us structured data?"*

**They were right. I was wrong. I had drifted from ADR 0007 under the
cover of "let's make the thin slice end-to-end without LLMs."**

The pivot: `parse.rs` is deleted. The USGS adapter now emits only a
`Document` record. The demo tells the truth — no production table,
just "Observations require the Level-2 recipe runtime, under
construction."

---

## EAGER WARNING — DO NOT SHORTCUT THE ARCHITECTURE

Fresh-Claude, or whoever is reading this: you will be tempted to
take shortcuts. You will be tempted because:

- The user will want to see results. Results feel like progress.
- Tests will seem easier to make green if you cheat around an ADR.
- The "right" path (e.g. wiring an LLM provider) is longer and
  scarier than the "wrong" path (e.g. writing a regex).
- A parser you write will pass unit tests you write yourself,
  because you'll hand-craft the input to match the output you
  want. **Real data breaks you.**

**Do not do this.** The architecture exists because we had long
design conversations about exactly these tradeoffs. ADRs 0003,
0004, 0007, 0010 are not aspirational — they are constraints.
Every time you are about to write code that an ADR explicitly
rejected, stop.

Specific shortcuts you will be tempted by:

- Writing a deterministic parser for any source. **ADR 0007 says
  the LLM authors recipes once; the runtime applies recipes.
  Do not write per-source deterministic parsers.** The closed enum
  of five `ExtractionSpec` variants is exactly the extraction
  vocabulary. If a source needs something outside those five,
  add a sixth via ADR, not via a parallel parsing module.
- Adding a commodity dimension. **ADR 0010 says Topic is the
  universal subject tag. No `CommodityId`, no domain registry.**
  The legacy `AuthoritativeDomain.commodity: Option<String>` field
  in `sources/traits.rs` is drift we should clean up; do not add
  more like it.
- Skipping security primitives. **ADR 0009 says every HTTP call
  goes through `SecureHttpClient`, every URL through `UrlGuard`,
  every secret through `ApiKey`.** A new LLM provider is not an
  exception to this. Read `docs/security/threat_model.md` before
  wiring any network path.
- Making green tests a goal in itself. **Green tests on hand-crafted
  input prove nothing about real data.** The live test against real
  USGS is the minimum honest bar. For the LLM recipe path we are
  about to build, the bar is higher: the recipe must produce correct
  records from real PDFs, not just parseable ones.

Rough rule: *if you are about to write code that an ADR's
"Alternatives considered" section rejected, you are about to
make the mistake this document was written to prevent.*

---

## What was done this session (chronological)

1. **Phase 2e:** re-added duckdb, wrote migrations 0001+0002, added
   envelope_io helpers, implemented storage for Observation.
2. **Phase 2e-2:** extended storage to the other five record types
   (Event, Entity, Relation, Document, Assertion) plus
   `topics_in_use` query for ADR 0007 Level-1 injection.
3. **Phase 3a Session A:** real USGS adapter, fetches PDF bytes via
   `SecureHttpClient`, emits `Document` with base64 body placeholder.
   Live test proves real internet works.
4. **MSRV bump 1.86 → 1.88** (pdf-extract transitively required it).
5. **Phase 3a Session B:** added `parse.rs` with regex heuristics,
   wired it into the adapter, emitted Observations. **This was the
   mistake.** Live test passed because the bar was "≥1 Observation",
   not "correct Observations".
6. **Phase 3b:** wrote `apps/demo/` console binary to show the
   pipeline running. The output made the mistake visible.
7. **Pivot (this session end):** deleted `parse.rs`, reverted the
   USGS adapter to Document-only, rewrote the demo to match, wrote
   this handoff.

---

## Current state in code

### What exists and works

- Six record types, envelope, vocabularies, Topic, RecordType enum.
- Security: `SecureHttpClient`, `UrlGuard`, `FsGuard`, `ApiKey`,
  `Bounds`, scrubbed logging.
- Storage: DuckDB with migrations, six tables, three subject
  junctions, derivation chain, retention sidecar. Insert+get for
  every record type. `topics_in_use`. `insert_record` dispatch.
- Sources: `Source` trait, USGS adapter that fetches a PDF and
  emits a `Document` with extracted text as body.
- Demo: `stockpile-demo` binary runs fetch → extract → store →
  query for one commodity, prints Document summary and topic
  counts. Does not emit Observations yet.
- Pipeline types: `FetchRecipe`, `ExtractionSpec`,
  `ProductionBinding`, `ExpectationRef`, `FieldMap`,
  `FieldValueSource` — all defined and serde-tested in
  `pipeline::recipes`. **Not yet stored or executed.**
- Ten ADRs, all in full form.

### What doesn't exist yet

- **LLM provider wiring.** `stockpile_llm` is all stubs. No real
  SDK integration.
- **Recipe-apply runtime.** The `FetchRecipe` types exist but
  nothing consumes them. This is the next phase's main work.
- **Recipe authoring prompt.** `config/prompts/recipe_author.md`
  doesn't exist.
- **Recipe storage.** No `recipes` table. Deferred in Phase 2e
  explicitly; needs a migration when the apply runtime lands.
- **Research classifier (Level 1).** Prompt and wiring.
- **Source registry / scheduler wiring** so multiple sources can
  be discovered and run.
- **Tauri UI.** Phase 4+.

---

## Phase 3c — what the next session should do

Recommended sequence (one sub-task per session, sanity-first):

### 3c.1 — LLM provider wiring (ONE session)

Pick a provider (user will decide: Claude via Anthropic SDK, or
GROK via XAI Console SDK). Wire the `stockpile_llm` crate for real:

- `LlmProvider` trait (probably exists as a stub — check and
  extend, do not replace without a good reason).
- One real provider impl (e.g. `AnthropicProvider`).
- API key loaded via `stockpile_secure::secrets::ApiKey::from_env`.
- All HTTP through `SecureHttpClient`. **Do not introduce a second
  HTTP client.** If the SDK insists on its own client, prefer a
  direct REST call that uses `SecureHttpClient` over taking the
  SDK.
- Structured-output support via JSON Schema (the `schemars` crate,
  already in workspace deps).
- A test that doesn't hit the real API (mock) plus an
  `#[ignore]`-marked test that does.

Warning: SDKs often include their own reqwest client, their own
retry logic, their own logging. Evaluate whether the SDK is worth
it vs. a direct REST call. A direct REST call through
`SecureHttpClient` is usually simpler and respects ADR 0009.

### 3c.2 — Recipe authoring (ONE session)

- Prompt file at `config/prompts/recipe_author.md`.
- A `pipeline::recipes::author_recipe(plan, source_doc, provider)
  -> Result<FetchRecipe>` function that:
  - Builds the prompt from the `ResearchPlan`, the document text,
    and the JSON Schema for `FetchRecipe`.
  - Calls the LLM with structured output constraint.
  - Validates the returned `FetchRecipe` (URL guard, extraction
    mode, field mappings).
- Test: feed a known document, verify the produced recipe has
  sensible shape. The LLM is non-deterministic — test assertions
  must be structural (recipe has ≥1 binding, URL passes guard),
  not exact-match.

### 3c.3 — Recipe apply runtime (ONE session)

- A `pipeline::recipes::apply(recipe, bytes) -> Result<Vec<Record>>`
  function deterministic in its output given the same inputs.
- One match-arm per `ExtractionSpec` variant:
  - `JsonPath`: use `jsonpath_lib` or similar.
  - `CssSelect`: use `scraper`.
  - `CsvCell`: use `csv`.
  - `PdfTable`: use `pdf-extract` *positionally* (page N, table M,
    row R, col C — NOT regex matching).
  - `RegexCapture`: use `regex`.
- Normalization stage (already a stub in
  `pipeline::normalize`): flesh it out to parse units, dates,
  currencies, attach `topic_tags` from the plan, build the
  provenance string including recipe id + version.
- Tests: each extraction mode gets a small fixture test.

### 3c.4 — End-to-end via demo (ONE session)

- Add `recipes` table migration (Phase 2e-3 effectively).
- Demo now runs: fetch → store Document → author recipe via LLM →
  store recipe → apply recipe → store Observations → query and
  print.
- Re-run the demo. This time Observations are correct, and the
  table the user first saw in Phase 3b has honest values.

This is roughly 4 sessions of work. Do not try to collapse them.

---

## Non-negotiable design commitments (reaffirmed)

All commitments from Session 1's handoff still hold. Repeating the
most important here because they are the ones most at risk:

1. **Six record types. No seventh.** ADR 0003.
2. **Topic is the universal subject tag.** No CommodityId, no domain
   registry. ADR 0010.
3. **Classification produces RecordExpectations, not new schemas.**
   ADR 0007 Level 1.
4. **Level 2 is LLM-authored `FetchRecipe`s applied by an LLM-free
   runtime.** ADR 0007.
5. **Closed enum of 5 extraction modes.** Adding a sixth requires
   an ADR update, not an ad-hoc module. ADR 0007.
6. **UUIDv7 + dedup_key for identity.** No content-addressing
   except the scoped pipeline-state case in ADR 0004.
7. **Security primitives in `stockpile_secure`**. No
   `reqwest::Client::new()` anywhere. ADR 0009.
8. **Structure follows code, not anticipates it.** No empty
   folders.
9. **"When the user pushes back, listen."** They have been
   consistently right. They caught this session's mistake with
   one question.

---

## Known drift to clean up (not blocking Phase 3c)

- `AuthoritativeDomain.commodity: Option<String>` in
  `crates/sources/src/traits.rs` — rename to `topic: Option<Topic>`
  per ADR 0010. Updates the USGS adapter. Small session; do it
  before we add more adapters.
- `SourceMetadata` doesn't expose a default unit hint. If we add
  more commodity sources, we'll want this as part of the metadata
  rather than the adapter constructor.

---

## Dependencies currently in the workspace

```
tokio, serde, serde_json, chrono, thiserror, anyhow
tracing, tracing-subscriber
reqwest (rustls-tls only, bounded features)
schemars                JSON Schema for LLM structured output
uuid (v4 + v7 + serde)
async-trait
figment                 config loading (unused so far)
ts-rs                   TS type export for Tauri
secrecy, zeroize        secrets
url (+ serde feature)   SSRF defense
subtle                  constant-time compare
futures                 streaming
toml                    config files
duckdb (bundled, chrono, uuid, serde_json)
pdf-extract             text extraction only; no parsing on top
clap (demo only)
```

Tests: `stockpile-storage` has `dev-dependencies` on itself from
`sources` (for a future cross-crate test — unused so far; fine
to leave).

---

## Test inventory

After the pivot:

```
stockpile-core         31 tests
stockpile-pipeline     12 tests
stockpile-storage      17 tests
stockpile-sources       6 tests (5 unit + 1 ignored live)
                     ─────────
                       66 tests
```

Note: the parser tests from Session B (7 tests) and the
`observation_from_row` test (1 test) are gone with `parse.rs`.
The live test was renamed to `live_fetch_returns_document`.

---

## The mood at the end of this session

The user is experienced and has good architectural instincts.
They gave me design freedom this session; I used it responsibly
most of the time and failed them once, with `parse.rs`. When they
caught it, they did not blame — they asked a clarifying question.
When I admitted the mistake and proposed the pivot, they chose
the right path (accept the visible regression, build the proper
architecture next) and explicitly asked for this handoff to
include a warning.

That warning is above, at the top of this document. Read it every
session.

---

## Commit history relevant to this handoff

```
phase 2e-2: storage across all six record types + topics_in_use
phase 3a session A: USGS adapter + SecureHttp fetch + insert_record dispatch
phase 3a session B: PDF extraction + Observation emission (MSRV 1.88)  [REVERTED IN SPIRIT]
phase 3b: stockpile-demo console binary                                [REWRITTEN]
phase 3b.1: revert parse.rs, back out to Document-only                 [this session end]
```

The Session B commit is still in git history. That is fine — it
documents the mistake. The post-pivot commit reverts the
misbehavior, not the history.

---

## Final note to fresh-me

The user's instinct on architecture has been right every time
they have questioned me. When they say "why not X", listen
carefully — it is usually not a question but a gentle correction.

Do not be eager. Do not chase green tests. Do not take shortcuts
around ADRs. The architecture is correct; my job is to extend it
honestly. When I drift, the user will catch it, and fixing the
drift is more expensive than not drifting in the first place.

Good luck. Build the recipe runtime properly.
