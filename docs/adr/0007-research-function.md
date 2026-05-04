# ADR 0007 — Research function: two-level LLM architecture

**Status**: Accepted
**Date**: 2026-04-20
**Supersedes**: the Phase 1 stub of this ADR, which predated both the
`RecordExpectations` design and the Level-2 decision.
**Related**: ADR 0003 (six record types), ADR 0010 (topic-based
subjects), ADR 0009 (security posture)

## Context

Situation_room's product surface starts with a text box. The user types a
topic — anything from "lithium production" to "EU AI Act compliance"
to "container shipping rates" — and the system populates a
workstation with traceable data within seconds. That experience
decomposes into two questions, and they want different machinery:

1. **What should we research?** Given "chip production", which
   metrics, events, entity kinds, and relations matter? Which
   topics should records be tagged with? What's the geographic and
   temporal scope?
2. **Where does the data live, and how do we pull it?** Given a plan
   from step 1 and a set of candidate sources, what exact URLs do we
   fetch, what exact fields do we extract, and how do they map to
   panel cells?

Question 1 is *semantic* — it benefits from an LLM that understands
what "chip production" entails. Question 2 is *structural* — it is an
integration problem that an LLM can solve *once per source* but
shouldn't be asked to solve *per query*.

The failure mode we're avoiding: running an LLM on every page load
to re-read sources, re-derive the same extraction, re-interpret the
same JSON field. That's slow, expensive, non-deterministic, and makes
"every number traceable to a source" harder rather than easier —
because the trace becomes "the LLM said so" instead of "this URL,
this field, this time".

## Decision

Research is a **two-level LLM architecture**. Both levels produce
structured, storable artifacts. Neither level runs during panel
rendering.

### Level 1 — Classification

- **Input:** the user's free-text topic, plus the set of Topic
  strings already in use across past sessions (capped at a sensible
  N — initial target 200, tunable).
- **Output:** a `ResearchPlan` containing a `RecordExpectations`
  structure, as defined in `crates/pipeline/src/research.rs`.
- **Job:** interpret the topic, pick Topic tags (reusing existing
  strings where they plausibly fit, introducing new ones where they
  don't), enumerate the metrics / event types / entity kinds /
  relation kinds / document sources the session expects to find,
  and set the historical window and geographic scope.

The existing-topics injection is the hygiene mechanic. It makes
"chip production" and "wafer supply" converge on the same topic
strings when they're about the same subject, without a registry or
a governance step. The classifier is *shown* existing topics; it
isn't *constrained* to them. See "Level 1 prompt discipline" below
for the exact shape.

Level 1 runs **once per session**, at session start. Its output is
stored with the session and is the single source of truth for "what
is this research about".

### Level 2 — Recipe authoring

- **Input:** the `ResearchPlan` from Level 1, plus a set of candidate
  sources (from the source registry), plus a one-time fetch of each
  candidate source's current content (the LLM sees the actual page
  or API response, not just a description of it).
- **Output:** a set of `FetchRecipe` records — one per *source-binding
  pair*, where a binding is "this source contributes to this
  expectation from the plan".
- **Job:** for each binding, write a recipe the runtime can apply
  deterministically. The recipe specifies the URL to fetch, the
  extraction mode (JSONPath, CSS selector, CSV cell, PDF table cell,
  regex capture), and the mapping from extracted values to record
  fields.

Level 2 runs **once per session, then on demand** when a recipe
fails validation (source moved, format changed, schema drifted).
Failure is per-recipe — a broken USGS recipe doesn't force rewriting
a SEC recipe.

### The runtime path

Once both levels have run, the runtime is LLM-free:

```
fetch(url from recipe) → apply(extraction from recipe) → normalize
  → emit record of the type the binding specifies
  → envelope populated from session's topic_tags + recipe's provenance
```

Normalization is a small, deterministic stage that sits between raw
extraction output and record emission. It handles unit parsing, date
parsing, currency normalization, and attaches the session's Topic
tags plus the recipe's provenance to the record's envelope. It is
not an LLM call. It is plain Rust.

## Rationale

**Why two levels, not one.** A single combined call would have to
both *decide what matters* and *know where it lives*, which means
passing the LLM both the topic and the entire source registry on
every run. That call gets large, slow, and brittle. Splitting it
lets Level 1 stay small and fast (topic in, plan out) and lets
Level 2 be per-source (one LLM authoring pass per source-binding,
then cached forever-until-it-breaks).

**Why Level 2 outputs a recipe, not records.** If Level 2 produced
records directly, the LLM is in the hot path for every refresh —
every price update, every daily fetch. That is exactly the cost we
rejected. A recipe is a cached, inspectable, debuggable artifact
that turns integration from an LLM question into a parser question.
When a recipe breaks, we can *see* what was supposed to happen.

**Why the extraction modes are a closed enum.** An open-ended DSL
("run this JavaScript against the page") reinvents web scraping at
runtime and makes execution expensive and hard to sandbox. A closed
enum of five modes handles the sources we care about (structured
APIs, HTML tables, CSVs, PDF reports, the occasional unstructured
page) while keeping every mode cheap and deterministic. Adding a
mode is a deliberate schema change with a PR and a test — not
something an LLM can invent on a whim.

**Why recipes are not a seventh record type.** Recipes are
*instructions*, not facts. They aren't claims about the world; they
are rules for how to produce claims about the world. The six record
types describe what Situation_room knows; recipes describe how Situation_room
learned it. Conflating them would muddy the schema. Recipes live in
their own storage table, referenced by records via
`Envelope::provenance` (the `source_id` will include the recipe id
so a record traces back not just to a URL but to the exact recipe
version that produced it).

**Why Level 1 runs once and Level 2 runs per-failure.** Level 1's
output describes the *intent* of the session — changing it mid-
session means the user is researching a different thing, in which
case they should start a new session. Level 2's output is
*mechanical* — a source changed its HTML, a JSON API added a nesting
level — and updating it is routine maintenance. The split matches
the underlying stability of each layer: intent is stable per session,
integration is not.

**Why one recipe per source-binding, not per source.** A single
source often feeds multiple panels (a USGS PDF fills price,
production, *and* reserve estimates). Per-binding recipes mean a
schema change affecting only reserves doesn't invalidate the price
recipe. Independent failure is worth the slight redundancy.

## Alternatives considered

**One-level: LLM-as-runtime.** Ask the LLM every time the user
refreshes: "here's the topic, here's the source, give me the
numbers." Rejected: cost, latency, non-determinism, weak provenance.
The thing we're building is a workstation that trusts its numbers;
"the LLM said 142,000 tonnes" is not a trustable provenance chain.

**One-level: LLM writes records directly.** Level 2 returns
records, not recipes, and Level 1 merges into it. Rejected for the
same reason: the LLM is still in the hot path on every refresh, and
we've lost the ability to re-run ingest cheaply.

**Open-ended extraction DSL.** Let the LLM emit arbitrary
expressions (JavaScript, jq, XPath with functions). Rejected:
sandbox surface area, debugging opacity, performance cliffs, and
the LLM's tendency to produce clever-but-brittle expressions when
a simple path would do. Closed enum of modes forces the LLM toward
the simplest correct extraction.

**Recipe-per-source (not per-binding).** One blob per source that
handles all its bindings at once. Rejected: coupled failure. A
fifteen-field recipe where field 3 breaks would invalidate the
whole recipe instead of just field 3's binding.

**No existing-topics injection at Level 1.** Let the classifier
invent freely every time and rely on query-time fuzzy matching to
bridge synonyms. Rejected during ADR 0010 review: pushes hygiene
into the query layer, where every panel query has to know about
synonyms. Injection pushes it up to authoring time, where the LLM
handles it once.

## Specifications

### Level 1 output — `ResearchPlan`

Already defined in code at
`crates/pipeline/src/research.rs::ResearchPlan`. Core field:
`expectations: RecordExpectations`. No schema change needed here;
the Phase 2c design holds.

### Level 1 prompt discipline

The prompt at `config/prompts/research_classifier.md` (to be
written as part of Phase 3) must include:

1. The user's topic, verbatim.
2. The list of existing Topic strings in use, sorted by frequency
   (most-used first), capped at N=200.
3. Explicit instruction: "Prefer an existing topic string if a new
   query is plausibly about the same subject — e.g. `chip_production`
   and `wafer_supply` should resolve to the same topic set if the
   user's intent is the semiconductor supply chain. Introduce a new
   topic string only if the query is genuinely about something the
   existing topics don't cover."
4. The JSON Schema for `ResearchPlan`, via `schemars`, as structured-
   output constraint. The LLM cannot return free text; it returns a
   validated `ResearchPlan` or fails.

The existing-topics query is a dependency on the storage layer
(Phase 2e): a `topics_in_use(limit: usize) -> Vec<(Topic, usize)>`
function returning topics and their usage counts. Until storage is
in place, Level 1 runs with an empty existing-topics list, which
degrades hygiene but does not block the architecture.

### Level 2 output — `FetchRecipe`

New type, to live in `crates/pipeline/src/recipes.rs`:

```rust
pub struct FetchRecipe {
    pub id: Uuid,                         // UUIDv7, per ADR 0003
    pub plan_id: Uuid,                    // back-reference to ResearchPlan
    pub source_id: String,                // registered source
    pub source_url: Url,                  // exact URL the runtime fetches
    pub extraction: ExtractionSpec,       // see below
    pub produces: Vec<ProductionBinding>, // what records this recipe emits
    pub authored_at: DateTime<Utc>,
    pub authored_by: ApiKeyFingerprint,   // which LLM key authored
    pub version: u32,                     // incremented on re-author
}

pub enum ExtractionSpec {
    JsonPath   { path: String },
    CssSelect  { selector: String, attribute: Option<String> },
    CsvCell    { column: String, row_filter: Option<RowFilter> },
    PdfTable   { page: u32, table_index: u32, row: u32, col: u32 },
    RegexCapture { pattern: String, group: u32 },
}

pub struct ProductionBinding {
    pub record_type: RecordType,          // which of the six
    pub expectation: ExpectationRef,      // which expectation this fulfills
    pub field_mappings: Vec<FieldMap>,    // extracted value → record field
}
```

Final field set will be refined during implementation, but this is
the skeleton. Notable commitments:

- `Url` here is the standard `url::Url`, validated through
  `situation_room_secure::url_guard::UrlGuard` before storage. A recipe
  whose URL fails the URL guard is rejected at authoring time, not
  at runtime. (ADR 0009.)
- The recipe's `source_id` must resolve against the source registry;
  recipes can't point at unregistered sources. Registration is the
  gate where license, robots.txt, rate-limit, and fetch-through-
  `SecureHttpClient` policy get applied.
- `authored_by` is the fingerprint (not the raw value) of the API
  key of the LLM that authored the recipe. Lets us audit provenance
  of the recipe itself. Follows `situation_room_secure::secrets::ApiKey`
  conventions.

### Level 2 runtime

1. Read `ResearchPlan` for the session.
2. For each expectation, query source registry for candidate sources.
3. For each (expectation, source) pair, fetch the source once (via
   `SecureHttpClient`) and pass to LLM with the expectation
   description.
4. LLM returns a `FetchRecipe` or explicitly declines ("this source
   does not cover this expectation"). Declines are stored too —
   they're how the `CoverageReport` knows the gap is real, not
   untried.
5. Recipes pass validation (URL guard, source registered, extraction
   parses, field mappings reference real record fields) then persist.

### Recipe failure and re-authoring

A recipe fails when:

- HTTP fetch errors (4xx/5xx that aren't transient).
- Extraction returns no match or a type-mismatched value.
- Normalization rejects the extracted value (unit unparseable, date
  invalid, value out of plausible range — the latter configurable
  per metric).

Failure is logged as an event, the recipe is marked degraded, and a
Level-2 re-authoring is queued for that recipe only. Re-authoring
increments `version`; old versions are retained so we can
distinguish "record was produced by recipe v1" from "record was
produced by recipe v2" in the provenance chain.

### Versioning vs migration — the distinction

Not every change to a recipe is equal, and the two categories are
handled differently:

- **Semantic changes** (different URL, different extraction path,
  different field mapping, unit change, new extraction mode in use)
  bump the recipe's `version`. The old version stays in storage,
  append-only. Records stamped with an old version remain truthfully
  traceable to the exact recipe that produced them. This is load-
  bearing for the "every number traceable to a source" promise —
  particularly when debugging a systemic error, where we need to
  identify which records were produced by buggy logic versus fixed
  logic.
- **Cosmetic changes** (recipe struct field renamed, serialization
  format changed, internal representation refactored) are handled
  by migration, not versioning. Records don't care about these;
  their provenance string references `recipe:{id}@v{version}`, and
  as long as that identifier continues to resolve, the on-disk
  layout can evolve freely.
- **Engine-level semantic changes** (what "apply a recipe" means
  globally — e.g., moving normalization into the apply step) are
  handled by the engine-version mechanic described below, not by
  per-recipe versioning.

The rule of thumb: **version what affects what records mean; migrate
what affects how recipes are stored.** Contributors should err on
the side of versioning when unsure; the cost of a duplicated recipe
is bounded and local, while the cost of a lost provenance trail is
unbounded and only discovered when it's too late.

### Normalization stage

Lives in `crates/pipeline/src/normalize.rs` (already stubbed in
Phase 1). Deterministic transforms on recipe output:

- Strings → `Unit`, `Currency`, `CountryCode`, `EntityId` via the
  existing `vocab` validators.
- Date strings → `DateTime<Utc>` via `chrono` parsers. Ambiguous
  dates reject at normalization time, not later.
- Numeric ranges / uncertainty intervals → `value_uncertainty`
  fields on `ObservationContent`.
- Topic tag attachment: the session's `topic_tags` attach to every
  record produced, as part of `Envelope::subjects.topics`.
- Provenance attachment: `Envelope::provenance.source_id` is set to
  `{registered_source_id}#recipe:{recipe_id}@v{version}` so the
  trace includes which recipe version produced this record.

Normalization failures reject the record and surface as ingest
errors — they don't silently produce a record with a `null`
somewhere.

## Consequences

**Positive**

- Runtime is LLM-free. Fast, cheap, deterministic refreshes.
- Provenance chain is concrete: URL + recipe id + recipe version +
  field path. A user clicking a cell can see exactly where it came
  from.
- Recipes are inspectable and debuggable. When something breaks,
  the fix is a recipe update, not a prompt rewrite.
- Source integration cost is frontloaded and amortized. Paying once
  at Level 2 to integrate a source forever (until it changes).
- The six record types remain the only schema surface. Recipes
  route around them; they don't add to them.

**Negative**

- Level 2 is the most expensive LLM operation in the system
  (one authoring pass per source-binding, each with a full source
  document in context). A large plan with many sources costs real
  money. Mitigation: per-binding caching, explicit re-authoring
  only on failure, and coverage reports that let the user prune
  the plan before Level 2 runs.
- The closed extraction-mode enum will eventually hit a source
  that doesn't fit. When it does, we add a mode with a PR. We
  accept this cost in exchange for the sandbox/perf/debuggability
  guarantees.
- Recipe versioning requires storage schema space. Old recipes
  can't be discarded naively without invalidating the provenance
  of records they produced. The storage design (Phase 2e) must
  treat recipes as append-only with version bumps.
- Recipe refactoring has a subtle semantic-drift risk. The
  recipe shape (types, fields, extraction modes) can evolve
  freely — old records are unaffected because their provenance
  strings are opaque. But if a refactor changes what "apply a
  recipe" *means* (e.g., moving normalization into the apply
  step, or altering how extraction failures degrade), then
  records stamped with a given recipe version before the refactor
  were produced by a different effective pipeline than the same
  version would produce after. The mitigation is a **recipe
  engine version** stamped alongside the recipe version in
  provenance (`recipe:{id}@v{v}/engine:{e}`). We don't need it
  until a semantic-changing refactor is on the table; we commit
  here to adding it *before* any such refactor, not after.

**Neutral**

- A session with no matching sources produces a valid `ResearchPlan`,
  zero `FetchRecipe`s, and an all-gaps `CoverageReport`. The
  workstation shows the research scope with every cell empty and
  labeled as uncovered. This is a feature: users see that the
  system tried, and what's missing.

## Dependencies

- **Storage (Phase 2e):** needs a `topics_in_use` query and a
  `recipes` table with versioning. Until both exist, Level 1 runs
  with empty injection and Level 2 results live in memory only.
  Usable but not production-grade.
- **Source registry (Phase 3):** Level 2 queries it for candidates.
  Registry must expose license, fetch policy, and a stable `source_id`
  for recipe back-references.
- **LLM provider (Phase 4):** both levels require structured-output
  via `schemars`-generated JSON Schema. Providers without
  JSON-Schema-constrained output can't drive the pipeline.
- **Secure HTTP (already in `crates/secure/`):** every fetch at
  Level 2 authoring time and at runtime must go through
  `SecureHttpClient`. Recipes store URLs; they don't store fetch
  credentials.

## Code references

- `crates/pipeline/src/research.rs::ResearchPlan` —
  Level 1 output (exists, Phase 2c).
- `crates/pipeline/src/research.rs::RecordExpectations` — the typed
  intent shape (exists, Phase 2c).
- `crates/pipeline/src/recipes.rs` — Level 2 types (to be added).
- `crates/pipeline/src/normalize.rs` — normalization stage (stub
  exists, Phase 1).
- `config/prompts/research_classifier.md` — Level 1 prompt
  (to be written, Phase 3).
- `config/prompts/recipe_author.md` — Level 2 prompt (to be written,
  Phase 3).

## Review notes

Reviewed 2026-04-20. This ADR was substantially rewritten from the
Phase 1 stub to cover both Level 1 and Level 2 of the research
function. Level 1 was already designed in Phase 2c
(`RecordExpectations`); Level 2 is a new decision captured here for
the first time.

The human reviewer framed the two-level split as "Level 1 decides
what to query, Level 2 decides where and how" and specifically
flagged that Level 2 should return "format with links that we do
not have to use LLMs to scrape again the sources" — i.e. the runtime
is LLM-free. That constraint drove the recipe-not-records decision
and the closed-extraction-mode enum.

Design decisions the reviewer delegated to the author of this ADR,
captured here for traceability:

- Recipes live in `pipeline::recipes`, not a separate crate.
- One recipe per source-binding pair.
- Extraction modes are a closed enum of five.
- Level 2 runs once per session, re-runs per-recipe on failure.
- The LLM reads sources at Level 2 authoring time; runtime does not.
- Normalization is a separate deterministic stage, not part of the
  recipe.
- Recipes are stored but not part of the six record types.

Any of these can be revisited if implementation reveals a problem;
the commitment is to the two-level split and the LLM-free-runtime
property, not to every subordinate choice.

---

Reviewed 2026-04-22 (Session 3, Phase 3c.3 kickoff). Before
beginning the runtime apply work, the architecture was deliberately
re-tested against an alternative: move extraction to the LLM on
every refresh, making recipes advisory hints rather than machine-
executable instructions. The alternative was considered because PDF
table extraction in pure Rust is genuinely hard and the "closed
enum of five extraction modes" may eventually hit a source that
doesn't fit.

The alternative was rejected. Rationale reaffirmed:

- **Runtime determinism and cost are load-bearing.** A workstation
  whose numbers can shift between refreshes because the LLM
  interpreted the same source slightly differently is not a
  workstation we want to build.
- **Provenance weakens materially under the alternative.** "URL +
  recipe + field path" degrades to "URL + recipe + LLM model + call
  id" — and the latter is not trustable enough to support "every
  number traceable to a source."
- **The cost shape inverts.** Level 2 is a per-session
  authoring pass (expensive but bounded); LLM-on-refresh is
  unbounded and scales with user activity. The first shape is
  sustainable offline, cacheable, and can run cheaply for months
  between authoring passes. The second cannot.
- **Smartness belongs at Level 1.** The product's intelligence
  should be concentrated in classification — picking the right
  metrics, the right entities, the right sources — and in Level 2's
  authoring of precise coordinates. Runtime should be dumb and
  fast. This is a deliberate choice to push complexity upward,
  into the prompt and schema surfaces, not sideways into the
  runtime.

The practical consequence: the closed extraction-mode enum is now
load-bearing for the runtime. Adding a sixth mode remains an
ADR-level decision. Where a source doesn't fit an existing mode, the
right responses are (a) improving Level 1's hints so Level 2 picks
a better mode, (b) pre-processing the source at authoring time, or
(c) adding a mode via ADR. Not (d) running an LLM at refresh.

One concession made explicit: `ExtractionSpec::PdfTable` remains in
the closed enum but its runtime implementation is deferred. Pure-
Rust positional PDF table extraction is a known hard problem and
the first runtime ships with four of five modes implemented. The
`PdfTable` arm returns a structured `NotImplemented` error with a
clear reason so recipes authored for PDFs are correctly shaped and
stored, but fail predictably at apply time rather than silently or
wrongly. The demo binary (Phase 3c.4) will use a non-PDF source to
demonstrate end-to-end correctness; USGS / PDF sources unblock when
`PdfTable` extraction lands as its own focused session.

---

Reviewed 2026-04-30 (Session 12). Two amendments to the runtime
path, both prompted by real-plan runs since Session 9.

**Amendment 1: CssSelect promoted from skipped to wired.** The
runtime now dispatches `ExtractionSpec::CssSelect` through the same
fetch → apply → insert pipeline as `CsvCell` and `JsonPath`. The
`recipe_apply` extractor for CssSelect has existed since Session 3
(via the `scraper` crate); what was missing was the executor-level
arm. With this change, three of five modes are wired (`CsvCell`,
`JsonPath`, `CssSelect`); `RegexCapture` is still surfaced as
`Skipped { reason }` pending its own promotion session;
`PdfTable` continues to return `NotImplemented` per the
2026-04-22 review note above. The closed-enum invariant is
unchanged — promotion is an executor-side wiring step, not a
schema change.

**Amendment 2: Known limitation — date-keyed object responses do
not fit the closed extraction vocabulary cleanly.** Surfaced by the
Session 11 first-real-plan run on "Swiss national debt." The
recipe author chose IMF's
`https://www.imf.org/external/datamapper/api/v3/GGXWDG_NGDP@WEO/CHE`
endpoint, whose response shape is

    { "values": { "GGXWDG_NGDP": { "CHE": { "1980": 24.06, "1981": ..., "2024": 38.1 } } } }

— `values` is an object keyed by year, not an array. Standard
JSONPath has no `[-1]` semantics over object members and cannot
express "the most recent year" without hardcoding the year value
in the path. The runtime correctly rejected `$.values[-1]` with
`path matched no nodes`; no code change is warranted — the
vocabulary genuinely can't address this shape.

The right responses, in order of cost:

1. **Steer the LLM at authoring time toward array-shaped
   endpoints** (cheapest; prompt work). Many APIs offer multiple
   shapes for the same data — e.g. World Bank's
   `country/CHE/indicator/...` returns `[<metadata>, [<datapoints>]]`,
   which JSONPath addresses cleanly. The recipe-author prompt
   could be amended to prefer such shapes when they exist.
2. **Pre-process at authoring time so the LLM sees an
   array-shaped excerpt.** The Session-10 Option-F pre-fetch is
   already in place; a transform step that flattens a year-keyed
   object into `[{year, value}, ...]` before passing it to the
   author would make the date-keyed shape addressable. This is a
   modest extension to the existing pre-fetch pipeline, not a new
   extraction mode, and preserves the closed-enum invariant.
3. **Extend `RowFilter` (or add a sibling) to express
   "last lexical key of an object."** This is more invasive
   because `RowFilter` is currently a CSV concern; reusing it for
   JSON would conflate two extractors. A cleaner shape would be a
   small post-extraction selection step in `normalize` keyed by a
   recipe-level hint. Expensive to design correctly; defer until
   recurrence justifies it.
4. **Add a sixth extraction mode.** ADR-level decision. Not
   warranted on one example.

The 2026-04-22 review note stated: "where a source doesn't fit an
existing mode, the right responses are (a) improving Level 1's
hints so Level 2 picks a better mode, (b) pre-processing the
source at authoring time, or (c) adding a mode via ADR. Not (d)
running an LLM at refresh." This amendment is consistent with that
disposition: response (1) is a Level-1/prompt fix, response (2) is
authoring-time pre-processing, response (3) is a normalize-stage
refinement (still LLM-free at runtime), and only response (4)
would expand the closed enum. None of (1)–(4) is taken in
Session 12; this amendment exists so the next person who hits a
date-keyed object response finds the failure-shape already named
and the response options already enumerated.

The Session 11 production run also revealed two adjacent failure
shapes the prompt may need to address eventually:

- **Country-code format inconsistency.** Same plan, same model
  run: the LLM produced ISO 3166 alpha-3 (`CHE`) for the IMF
  recipe and alpha-2 (`CH`) for the World Bank recipe — World
  Bank requires alpha-3. One data point isn't a pattern; the
  prompt stays at v1.3 until recurrence.
- **JSONPath syntax synthesis errors.** The same Swiss-debt run
  produced JSONPath `1[0].value` (missing `$.` prefix and
  separator); the Session 12 "italy gdp" run produced
  `$.['NGDP@WEO'][43]` (quoted-bracket form `jsonpath_rust`
  rejects). Two data points; still below the threshold for a
  prompt-level intervention.

Both go in the failure-mode taxonomy that the deferred ADR 0012
(re-author-on-failure) will need; neither prompts code or prompt
changes in Session 12.

---

Reviewed 2026-05-01 (Session 18). Amendment 3 to the runtime path,
prompted by the recurring problem of PDF-only sources sitting
outside the addressable extraction vocabulary.

**Amendment 3: Recipe-level `static_payload` field — the bake path
for un-addressable sources.** A new optional field on
[`FetchRecipe`](../../crates/pipeline/src/recipes.rs):

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub static_payload: Option<String>,
```

When `Some(payload)`, the runtime serves those bytes to the apply
stage in place of an HTTP fetch. When `None` (the default), the
runtime fetches `source_url` normally — preserving every existing
recipe's behavior verbatim.

**The bytes' provenance is orthogonal to the extraction mode.** A
baked CSV is still a `csv_cell` recipe; a baked JSON is still a
`json_path` recipe. The runtime branches on this field at
byte-acquisition time only — `apply()` never sees the distinction.
This is the architectural reason `static_payload` is a recipe-level
field rather than a sixth extraction mode (option (b) below).

### Why this exists

The closed extraction vocabulary's `pdf_table` mode exists for
PDF sources but is not yet wired in the runtime (the prerequisite
table-detection libraries, page rasterization, and positional
addressing each carry enough complexity to deserve their own
focused session). Until `pdf_table` lands, PDF-only sources are
stuck: the LLM authors recipes that target the PDF, the runtime
returns `Skipped { reason: "pdf_table not implemented" }`, and
the user sees no records.

The Session 17 motivating cases were:
- USGS MCS chapters (PDF as primary publication, HTML available
  alongside);
- SEC EDGAR 10-K filings (PDF and HTML both addressable);
- EUR-Lex regulation full texts (PDF and HTML renderings);
- press releases from sources whose websites only publish PDFs
  (e.g. small central banks, regulatory notices).

The first three have HTML equivalents and the right move — taught
in recipe-author prompt v1.7's "Strategy for PDF sources" section
— is to author against the HTML. The fourth has no HTML route.
For the fourth, the LLM can read the prefetched PDF excerpt at
authoring time, transcribe the relevant fields into a small JSON
document, and bake that document into the recipe via
`static_payload`. The runtime then serves the baked bytes through
the same `json_path` extraction the LLM authored, producing
records on every fetch.

### Bake-time-frozen freshness

A recipe with `static_payload = Some(payload)` produces the same
records on every fetch, until re-authored. There is no live data
path. **The cost is freshness; the benefit is that PDF-only and
otherwise un-addressable sources become expressible without
expanding the closed extraction-mode enum.** The closed enum stays
at five.

The freshness model is materially different from the common
HTML-addressable case, so the UI shows it explicitly:

- A visible **BAKED badge** in the recipe head, with a tooltip
  explaining the freshness contract;
- A **collapsible payload preview** showing the raw baked bytes
  the runtime feeds to extraction.

A user looking at the recipes panel for a plan can tell at a
glance which recipes are live and which are bake-time-frozen.

### Why option (b), not option (a)

Two architectural shapes were considered:

**(a) A sixth extraction mode `static_payload`.** A new variant
on the closed `ExtractionSpec` enum carrying the baked bytes
directly. *Rejected.* The bytes are not an extraction strategy —
they're an alternative *source* of bytes the existing extraction
strategies operate on. Mixing the two collapses a clean
distinction. A baked CSV needs `csv_cell` extraction; a baked
JSON needs `json_path` extraction; a baked HTML page would need
`css_select`. Forcing a sixth mode would either duplicate every
existing mode (`static_csv`, `static_json`, `static_css`, …) or
collapse them into a single `static_payload` mode that re-invents
the dispatch ladder `apply()` already runs.

**(b) A recipe-level field orthogonal to extraction mode.**
*Selected.* A new optional field on `FetchRecipe`. The closed
extraction enum stays at five; the runtime gains one branch (at
byte-acquisition time, before `apply()`); existing recipes
deserialize unchanged via serde defaults; existing extraction
modes operate on baked bytes exactly as on fetched bytes.

The decisive consideration: the bytes' *provenance* (network vs
baked) is genuinely orthogonal to *what to do with the bytes*
(extraction mode). Modeling the orthogonal axes orthogonally is
the cheaper long-term shape.

### Validation discipline

The wire form is empty-string-as-absent (xAI's structured-output
schema rejects top-level `Option<T>` for some shapes; the
`unit_hint`, `assertion_guidance`, and `display` fields use the
same idiom). The Rust validator (`build_validated_recipe`) does:

1. Empty / whitespace-only string → `None`. Common case for
   every HTML-addressable recipe.
2. Non-empty string → `serde_json::from_str::<Value>(payload)`
   to validate well-formedness. The parsed Value is discarded —
   storage carries the raw string verbatim — but unparseable
   input is rejected at authoring time rather than at every
   subsequent fetch.

JSON-only validation reflects the prompt's bake discipline (the
LLM is taught to bake JSON documents the recipe's extraction
mode can address). If a future session needs to relax this for
non-JSON payloads (CSV, HTML), the validator softens then. For
now, stricter is correct.

### Migration shape

Migration `0008_recipes_static_payload.sql` adds the column as
nullable TEXT. **Additive, backward-compatible, no re-authoring
required.** Existing recipes carry NULL; the runtime treats NULL
as "fetch normally," which is the pre-Amendment-3 behavior. New
recipes default to NULL except where the LLM explicitly bakes a
payload.

The DuckDB-ALTER trap from migrations 0005 and 0007 applies (an
`ADD COLUMN ... NOT NULL DEFAULT ...` is rejected; index
dependencies block the split-then-set-NOT-NULL path); the
nullable column sidesteps it. The Rust type
`FetchRecipe.static_payload: Option<String>` is the load-bearing
invariant — same posture as `PlanStatus` in 0005 and
`rejection_reason` in 0007.

### Code references

- Field definition:
  `crates/pipeline/src/recipes.rs::FetchRecipe::static_payload`
- Authoring validation:
  `crates/pipeline/src/recipe_author.rs::build_validated_recipe`
  (step 6 — collapse + JSON-parse + reject unparseable)
- Authoring wire form:
  `crates/pipeline/src/recipe_author.rs::RecipeAuthoringOutput::static_payload`
- Runtime short-circuit:
  `crates/pipeline/src/fetch_executor.rs` — inlined in all four
  `run_X_recipe` functions (CSV, JSON, CSS, regex), per Session 9's
  duplication-with-comments-over-premature-unification rule
- Apply boundary:
  `crates/pipeline/src/recipe_apply.rs::apply` — does NOT branch
  on `static_payload`; the executor decides byte provenance before
  calling apply
- Storage schema:
  `migrations/0008_recipes_static_payload.sql`
- Storage row:
  `crates/storage/src/recipes.rs::RecipeRow` and `StoredRecipe`
- Wire DTO:
  `crates/api/src/types_export.rs::RecipeDto::static_payload`
- UI rendering:
  `apps/desktop/src/components/RecipesPanel.svelte` (BAKED badge,
  collapsible payload preview)
- Authoring discipline:
  `config/prompts/recipe_author.md` v1.7, "Strategy for PDF
  sources — HTML first, static payload fallback" section

### Bounded scope

Amendment 3 does **not** wire `pdf_table` extraction. It
sidesteps the need for `pdf_table` for the cases where bake-
time transcription is sufficient. `pdf_table` may still be
implemented later for cases where freshness matters and the
HTML route doesn't exist (live regulatory PDFs, dated annual
reports the user wants tracked over time without re-authoring).
The closed-enum invariant is unchanged — implementing
`pdf_table` would be wiring an existing enum variant to a
runtime path, not adding a sixth.

Amendment 3 also does **not** affect the runtime's LLM-free
invariant (ADR 0007 §"runtime path"). The bytes are baked at
*authoring* time by the LLM and stored verbatim; the *runtime*
serves stored bytes deterministically. The two-level split holds.

---

## Amendment 4 (Session 28) — Decline path + schema-aware authoring

**Status**: Accepted, in effect.
**Scope**: Adds an honest exit on the LLM's authoring output for
sources that don't admit a recipe under the closed extraction
vocabulary; adds three prompt placeholders that give the LLM
better grounding (the actual JSON Schemas of the target record
types, the prior recipe's failure message when re-authoring, the
operator's transient one-off note from the re-author dialog).
The closed extraction-mode enum is unchanged. The two-level LLM
architecture is unchanged. The runtime stays LLM-free.

### What this amendment adds

- **`decline_reason: String` on `RecipeAuthoringOutput`.** Empty-
  string-as-absent (matches the existing `static_payload` /
  `unit_hint` / `display` idiom; xAI's structured-output schema
  rejects top-level `Option<String>` for some shapes). When non-
  empty after trim, `build_validated_recipe` returns
  `AuthoringError::Declined { reason }` **before** any URL,
  extraction, or binding validation — a declined output isn't
  required to populate those fields meaningfully, and applying
  the secondary validators would surface a confusing "your URL
  is invalid" error subordinate to the actual decline.
- **`AuthoringError::Declined { reason }`** error variant. Length-
  bounded by `Bounds::DECLINE_REASON` (2 000 chars; over-bounded
  output surfaces as `InvalidRecipe`, not `Declined` — we got a
  decline, but we can't accept its size).
- **`RecipeOutcome::Declined { source_id, reason }`** outcome
  variant on the executor's per-recipe outcome enum. **No
  `recipe_id`** because no recipe was ever created. Surfaces in
  `FetchReport.outcomes` so the operator sees the LLM's
  explanation in the UI alongside any subsequently-run recipes.
- **`load_or_author_recipes` now returns
  `(Vec<FetchRecipe>, Vec<RecipeOutcome>)`.** Decline outcomes
  from the per-source authoring loop are lifted into the second
  Vec; the executor's `run_fetch_for_plan` prepends them to
  `outcomes` before iterating recipes. They do **not** count
  toward `recipes_attempted` (no recipe was attempted) or
  `recipes_succeeded`. Declined sources never produce a recipe
  to attempt, by design.
- **Three new prompt placeholders.**
  - `{{TARGET_RECORD_SCHEMA}}` — the schemars-derived JSON
    Schemas for the three authorable record-content types
    (`ObservationContent`, `EventContent`, `RelationContent`),
    wrapped as a single object keyed by snake_case record-type
    name. Computed at every call by
    `pipeline::recipe_author::target_record_schemas`. Adds a few
    KiB to the prompt's final size; well within
    `Bounds::LLM_PROMPT_BODY` (256 KiB). Gives the LLM the wire
    truth of the records it's authoring against rather than
    relying on prompt-side prose for type expectations.
  - `{{PREVIOUS_FAILURE_REASON}}` — the verbatim apply-stage
    error message from the prior recipe, populated from
    `AuthoringContext::previous_failure_reason` (set by
    `reauthor_recipe`). Plain prose framing, **no fence** — the
    failure message is the executor's own error chain, not
    operator-supplied text, so there is no injection vector to
    defend against. Empty string when fresh-authoring.
  - `{{OPERATOR_GUIDANCE}}` — the transient one-off note the
    operator typed in the re-author dialog (Session 27's Track
    A surface). Fenced with the same per-call UUID nonce
    treatment as `{{RECIPE_FEEDBACK}}` (the channel is
    operator-supplied free text, so the byte-walk-with-nonce
    discipline applies). Empty string when no guidance was
    given.
- **Schemars derives on the core content types.** `JsonSchema`
  added to `vocab::{Topic, CountryCode, EntityId, EventType,
  Unit, Currency}`, `schema::geometry::{Position, Geometry,
  PointGeom, LineStringGeom, PolygonGeom, MultiPolygonGeom}`,
  and `schema::content::{ObservationContent, EventContent,
  RelationContent, ObservationPeriod, EventDirection}`. These
  are the types `target_record_schemas` exposes; the derive
  ripples through their dependencies but stops at the content
  layer. Envelope, full record types, and assertion shapes do
  NOT derive `JsonSchema` — they are not in the LLM's authoring
  surface, and adding the derive would expand the schema the
  LLM sees with metadata it has no business populating.
- **`Bounds::DECLINE_REASON = 2 000`.** Sized to match
  `RECIPE_FEEDBACK` and `REJECTION_REASON`: long enough for the
  LLM to explain itself in a sentence or two, short enough to
  keep the channel from drifting into narrative invention.
- **Recipe-author prompt v1.9.** Four new prose sections:
  - "When no recipe is honestly possible — the decline path"
    teaches the LLM the four recurring decline shapes (JS SPA,
    paywall, dead endpoint, structurally-inappropriate source)
    and the framing that decline is not failure, but also not
    the easy way out.
  - "What the records you produce look like" introduces
    `{{TARGET_RECORD_SCHEMA}}` and tells the LLM the schemas
    are wire-truth, not prompt prose.
  - "Type honesty" names the two recurring runtime-caught
    failure modes (null-where-number, numeric-string-where-
    number) and tells the LLM that when the source's type
    can't be cleanly translated, decline is the right move.
  - "Zero records is a valid outcome" acknowledges legitimate
    empty-result-set cases so the LLM doesn't fabricate
    placeholder records.
  - "Defensive variants" surfaces the BBC CDATA case from
    Session 13 and the JSON-key-presence-may-vary case as
    examples of the structural-variant problem the single-shot
    author can't see from one excerpt.

### Why a flat `decline_reason` field, not a discriminated output

The natural alternative was a top-level discriminated union:
`Result<RecipeAuthoringOutput, DeclineOutput>` with `kind:
"declined"` vs `kind: "authored"`. We chose the flat field
deliberately:

- xAI's structured-output schema gateway has historically
  rejected top-level discriminated unions for nontrivial inner
  shapes; the empty-string-as-absent idiom is the existing
  workaround the codebase uses (see `static_payload`,
  `unit_hint`, `display`).
- A discriminated union forces the LLM to choose between two
  top-level shapes *before* knowing which path applies, which
  in practice yields more "I will try anyway" outputs (the LLM
  picks `authored` because that is the longer / more detailed
  branch and feels like the "main" output).
- The flat shape lets the LLM author normally and only set
  `decline_reason` when the obstacle is genuine — the schema
  shows decline as a sibling field, not a competing top-level
  alternative.

A future session that finds the flat shape produces too many
"declined for trivial reasons" outputs may revisit this; the
test scaffold (`DecliningProvider` in `fetch_executor::tests`)
makes the alternative cheap to A/B.

### Why `JsonSchema` only on content types, not on full records

The LLM's `field_mappings` populate fields **inside** the bare
content types — `metric`, `value`, `headline`, `event_type`,
`from`, `to`. The full record types
(`Observation`, `Event`, `Relation`) wrap the content with an
`Envelope` (subjects, time scope, provenance, confidence) that
the runtime stamps server-side from the plan's `topic_tags` and
the recipe's `provenance` field. The LLM never authors envelope
fields; surfacing the envelope schema would invite the LLM to
populate fields that the runtime overwrites, producing
confusing-to-the-operator schemas that don't reflect what the
recipe actually controls. Stopping the derive at the content
layer keeps the schema the LLM sees focused on what the LLM
actually authors.

### What this amendment does NOT do

- It does not wire `pdf_table` extraction (still
  `RecipeOutcome::Skipped { reason: "...not implemented" }`;
  Track C in the Session 25 spec, deferred).
- It does not implement the automated re-author retry loop
  (ADR 0012 §"When to automate"; the gate count remains 2/10).
  The new `decline_reason` channel is operator-visible but the
  decision to re-run, re-author, or drop the source is still
  manual.
- It does not change the closed extraction-mode enum. Adding a
  sixth mode still requires an ADR update.
- It does not affect the runtime's LLM-free invariant. The
  decline check runs at *authoring* time; once a recipe is
  persisted, no LLM call participates in fetching or applying
  it.
- It does not change the wire shape of any existing
  `RecipeOutcome` variant. The `Declined` variant is purely
  additive; existing frontend consumers that don't yet know
  about it will fall through to their default-case rendering
  (the TypeScript discriminated union check will flag the
  missing arm at compile time once the regenerated DTO lands).

### Code references (Track B, Session 28)

- `crates/core/src/vocab.rs` — `JsonSchema` on the six newtype
  vocab identifiers.
- `crates/core/src/schema/geometry.rs` — `JsonSchema` on the
  geometry shapes.
- `crates/core/src/schema/content.rs` — `JsonSchema` on the
  three authorable content types and their enum dependencies.
- `crates/secure/src/bounds.rs` — `Bounds::DECLINE_REASON`.
- `crates/pipeline/src/recipe_author.rs` — `decline_reason`
  field on `RecipeAuthoringOutput`, `AuthoringError::Declined`
  variant, `target_record_schemas` helper,
  `render_previous_failure_reason` and `render_operator_guidance`
  helpers, parametric `sanitize_for_fence_named` refactor, the
  three new placeholder substitutions in
  `build_prompt_with_fence_id`, the step-0 decline check in
  `build_validated_recipe`.
- `crates/pipeline/src/fetch_executor.rs` —
  `RecipeOutcome::Declined`, the new return type on
  `load_or_author_recipes`, the decline-prepending behaviour
  in `run_fetch_for_plan`, the `DecliningProvider` test
  scaffold.
- `crates/api/src/types_export.rs` —
  `RecipeOutcomeDto::Declined` variant + `From` arm.
- `apps/desktop/src/lib/api/types/RecipeOutcomeDto.ts` —
  regenerated to include the `declined` branch.
- `apps/desktop/src/lib/outcomes.ts` — `'declined'` tone,
  `outcomeKey` helper, `outcomeForRecipe` narrowing fix.
- `apps/desktop/src/components/FetchReport.svelte` — keyed-each
  switched to `outcomeKey`, declined-row rendering with the
  `decl·` marker, `[data-tone="declined"]` CSS.
- `config/prompts/recipe_author.md` v1.9 — four new prose
  sections + three placeholder hookups.
