# ADR 0007 — Research function: two-level LLM architecture

**Status**: Accepted
**Date**: 2026-04-20
**Supersedes**: the Phase 1 stub of this ADR, which predated both the
`RecordExpectations` design and the Level-2 decision.
**Related**: ADR 0003 (six record types), ADR 0010 (topic-based
subjects), ADR 0009 (security posture)

## Context

Stockpile's product surface starts with a text box. The user types a
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
types describe what Stockpile knows; recipes describe how Stockpile
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
  `stockpile_secure::url_guard::UrlGuard` before storage. A recipe
  whose URL fails the URL guard is rejected at authoring time, not
  at runtime. (ADR 0009.)
- The recipe's `source_id` must resolve against the source registry;
  recipes can't point at unregistered sources. Registration is the
  gate where license, robots.txt, rate-limit, and fetch-through-
  `SecureHttpClient` policy get applied.
- `authored_by` is the fingerprint (not the raw value) of the API
  key of the LLM that authored the recipe. Lets us audit provenance
  of the recipe itself. Follows `stockpile_secure::secrets::ApiKey`
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
