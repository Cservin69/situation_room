# Stockpile — Session 6 Handoff

Continuation document for the next session. Covers the state of the
codebase as of end of Session 5, what works, what's still imperfect,
and what Session 6 should pick up.

## State of the codebase

**Phase 4 is complete.** The research function (ADR 0007) runs
end-to-end. A user types a topic; the LLM classifies it into a
structured `ResearchPlan` carrying topic tags, geographic scope with
session-register display labels, expectations across the six record
types, and prioritized source nominations; the plan persists to
DuckDB; the user can list recent plans without making any LLM call.

| Phase | Status |
|---|---|
| 4a — Level-1 classification | done, live-verified |
| 4a.1 — `ResearchPlan::id` threading | done |
| 4a.2 — `research_classifier` module | done |
| 4a.3 — Classifier prompt | v1.2 |
| 4b — Level-2 recipe authoring | done since Session 3, live-verified Session 4 |
| 4c — `GeoScope { code, display }` shape | done |
| 4d — Plan persistence (`research_plans` table) | done |
| 4e — Situation room CLI binary | done |

The workspace is now seven library crates plus two binaries:
`core`, `storage`, `llm`, `pipeline`, `secure`, `api`,
`apps/desktop/src-tauri`, `apps/situation_room`. No `analytics`,
no `sources`, no `demo`. Every commodity-flavored adapter and
config file was purged in Session 5; the LLM is the only thing
that knows about specific commodities.

## What works

- **End-to-end Level-1 classification.** `cargo run -p
  stockpile-situation-room -- "any topic"` returns a structurally
  validated plan, persists it to `./stockpile.duckdb`, prints
  pretty JSON to stdout and a one-line summary to stderr.
- **Existing-topics injection.** Topics from past sessions are
  surfaced to the LLM as context. Demonstrated working on the
  cobalt-after-lithium classification — `lithium`,
  `battery_supply_chain`, and `critical_minerals` were correctly
  reused as canonical tags rather than reinvented.
- **Source descriptors injection.** The LLM sees the registered
  sources from `config/sources.toml` and produces
  `document_sources` hints with real ids and priority ordering.
  Demonstrated working: USGS MCS and SEC EDGAR coming first for
  commodities topics, IMF WEO and World Bank for sovereign-debt
  topics.
- **Geographic scope with session register.** `GeoScope { code,
  display }` carries the canonical machine code (ISO 3166 alpha-2
  or `lowercase_snake_case` region) and the LLM-produced
  session-register label (`Magyarország`, `Hungary`, `Ungarn`).
  Storage and cross-session joins use only `code`; `display` is
  render-only and persists with the plan.
- **Format-only validation.** The classifier rejects malformed
  plans (bad topic strings, oversized display labels, control
  characters, out-of-range historical windows, entirely empty
  expectations) but enforces no opinion on content. Richness
  lives in the prompt, not the validator.
- **The trust-paragraph contract.** `interpretation` is prompted
  explicitly as the moment of trust before fetching. Validation
  rejects empty interpretations.
- **Recipe authoring (Phase 3c).** Untouched in Session 5 and
  still works. Live test (`live_author_recipe_against_xai_*`) was
  green during the duplicate-Content-Type fix earlier this
  session.
- **Plan persistence.** `Store::insert_research_plan`,
  `Store::get_research_plan`, `Store::recent_research_plans(n)`,
  `Store::count_research_plans`. Migration `0004_research_plans`
  is registered and runs on first open.

## Test count

97 tests green (90 pipeline + 26 storage + 31 core + 20 secure +
10 llm + 6 situation_room) plus four ignored live tests:

- `live_xai_returns_nonempty_completion` (llm)
- `live_xai_returns_structured_json_when_schema_requested` (llm)
- `live_author_recipe_against_xai_produces_valid_recipe` (pipeline)
- `live_classify_topic_against_xai_produces_valid_plan` (pipeline)

The latter two have been run against today's xAI gateway and
both passed. They're worth re-running before any session touches
the LLM provider or the prompts.

## Known imperfections

### 1. The classifier prompt is v1.2; refinement is empirical

The prompt has been bumped twice in Session 5 based on real
classifications. Concrete weaknesses observed and fixed:

- Cobalt classification came back with three empty buckets (no
  events, no entities, no doc-sources) — a pattern-matching
  failure where the lithium worked example over-anchored. Fixed
  in v1.1 by adding a second worked example (OFAC SDN, a
  documents-and-events topic with four empty buckets *by
  design*) and a "when you're tempted to leave buckets empty"
  checklist.
- Hungarian sovereign debt classification came back with empty
  `geographic_scope` despite being a Hungary-specific topic, on
  the apparent grounds that the `interpretation` paragraph
  already said "Hungary-focused" — fixed in v1.2 by an explicit
  rule that `geographic_scope` must be populated whenever the
  topic has any scope, regardless of whether `interpretation`
  mentions it.

The pattern is: each prompt revision is small, prompted by a
real plan that came back weak, and never speculative. Continue
this discipline. Three or four more topics across genuinely
different shapes (an event-heavy one like "EU AI Act
enforcement", a metric-only one like "container shipping rates",
a person-centric one like a named CEO transition) before
deciding whether v1.3 is needed.

### 2. No GUI yet

The CLI is the only way to see a plan today. Every plan is
JSON-on-stdout. The user has to read the JSON to verify the
classification, decide whether to accept it, and there's no
mechanism to *reject* a plan (a wrong classification is just a
persisted-but-ignored row). This is what Session 6 is about.

### 3. The situation room v1 stops at persistence

`classify_topic + persist` is wired; recipe authoring is not.
The plan's `document_sources` hints are populated but never
consumed by anything. Wiring `author_recipe` against each plan's
nominated sources is its own session — each recipe-author call
is 30–60 seconds of LLM time, so a plan with four document
sources is two-to-four minutes of LLM work.

### 4. The `recent_research_plans` listing is not paginated

`Store::recent_research_plans(limit)` takes one number and
returns from newest. No cursor, no `since`, no count besides the
top-level `count_research_plans`. Fine for a CLI listing under
~50 plans; insufficient for any UI that wants to browse a
month's worth of research.

### 5. No way to delete or amend a plan

Plans are immutable once written, which is *almost* the right
discipline (re-running classification produces a fresh `id`),
but there's no way to mark a plan as superseded or to delete a
plan the user rejected. The UI will need this.

### 6. Carried forward from Session 4

- Anthropic provider and others are stubs (Session 3 #3).
- Apply-runtime strict deserialization is permissive (Session 3 #2).
- PdfTable extractor is unimplemented (Session 3 #4).
- Authoring latency is 30–60s (Session 3 #5; xAI gateway, not us).
- `SecureHttpClient` doesn't surface response headers (Session 3 #6).
- Crate-level `#![allow(...)]` lint suppressions (Session 3 #7).

## Suggested Session 6 priorities — pivot to GUI

The user has confirmed Session 6 is the GUI session. Concrete
shape suggestions, in rough order of leverage:

### Priority 1 — Plan review screen

The single highest-leverage screen. After classification, the
user sees the plan rendered as a real interactive panel, not as
JSON. Components:

- **Header**: topic, plan id, created_at, classified_by.
- **Trust paragraph panel**: `interpretation` rendered prominently
  with an "Accept / Reject / Re-classify" trio of actions.
- **Topic tags**: rendered as chips, with hover showing usage
  count (linking to existing-topics injection — "you've used
  this tag in N past sessions").
- **Geographic scope**: rendered using the `display` label from
  each `GeoScope`, falling back to `code` when display is empty.
  This is where the GeoScope work pays off visually.
- **Six-bucket panels**: one panel per record type
  (Observation, Event, Entity, Relation, Document, Assertion).
  Each shows the expectations as a list, with rationales
  expandable. Empty buckets are rendered as "(no expectations
  for this type — by design)".
- **Source nominations panel**: `document_sources` ordered as
  the LLM ordered them, with `preferred_source_ids` shown as
  badges. Sources outside the registered list are shown with a
  visual distinction.

### Priority 2 — Plan listing screen

A simple list of recent plans, filterable by topic. Each row
shows topic, created_at, a short summary from the bucket counts
("4 obs, 3 events, 4 entities …"). Click to open the review
screen.

### Priority 3 — Topic-input screen

The thing that classifies. A single text box, a submit button,
and a small spinner for the 5–10 second classification. While
it's running, surface what's happening on the wire (calling
xAI, validating, persisting). If classification fails, show the
`ClassificationError` clearly enough that the user can fix
their input.

### Priority 4 — Wire decisions, not aesthetics

Tauri 2.x is already the framework (per `apps/desktop/`). React
or Svelte on the frontend; the project doesn't have an
established preference yet. The classifier output is plain JSON
that any frontend can render. The Tauri command surface needs
three things minimum:

- `classify(topic: string) -> Plan`
- `list_recent_plans(limit: number) -> Plan[]`
- `get_plan(id: string) -> Plan | null`

These are thin wrappers on the existing pipeline functions.
There is no need to refactor the pipeline crate to support the
GUI; the surface is already right.

## What Session 6 should NOT do

- **Build recipe authoring into the UI.** Authoring is 30–60s
  per source per plan; a UI that surfaces it as a synchronous
  action will feel broken. Authoring belongs in a background-job
  pattern (a job table, a worker, a status view) that's its own
  session-shaped piece of work.
- **Re-architect plan storage.** The shape lands well. Pagination
  and soft-delete are real needs but they're cheap additions to
  the existing storage; resist the urge to redesign.
- **Add a settings screen for source descriptors.** Editing
  `config/sources.toml` from the UI sounds nice but it's UI
  surface that the user doesn't need yet, and it pulls in
  filesystem-permission complexity in Tauri. Edit the TOML in
  a text editor for now.
- **Implement multi-language UI strings.** The point of the
  GeoScope `display` field is that the LLM picks the register
  per-session, not that the UI itself is localized. A UI in
  English that shows `Magyarország` because the LLM produced
  that label is the right behavior. Don't introduce an i18n
  framework.
- **Touch the prompt.** v1.2 is what the GUI session uses; bumps
  come from observed classifications, not from speculation.

## Patches shipped in Session 5

For history.

1. **Adapter and analytics purge.** Removed `crates/sources/`,
   `crates/analytics/`, `config/sources/` directory, all
   `apps/demo/`. Updated workspace `Cargo.toml` and dependents.
   The codebase no longer carries any commodity-specialized
   scaffolding. Golden rule preserved: one research topic, the
   LLM classifies it.

2. **xAI duplicate `Content-Type` header fix.** The
   `extra_headers` argument to `SecureHttpClient::post_json`
   was carrying `("content-type", "application/json")` which
   appended a *second* Content-Type header on top of the one
   `.json(body)` already sets. xAI's gateway returns 415 on
   that. Removed the redundant header from the xAI provider's
   call site; added a clear "do not pass content-type in
   extra_headers" warning to the `post_json_bytes` doc comment.

3. **`research_plans` storage.** New migration
   `0004_research_plans.sql`, new `crates/storage/src/research_plans.rs`
   module with `Store::insert_research_plan`,
   `Store::get_research_plan`,
   `Store::recent_research_plans(limit)`,
   `Store::count_research_plans`. Mirrors the shape of
   `recipes.rs`. Storage stays opaque on the inner JSON shape;
   typed marshalling lives in `pipeline::research_plans_store`.

4. **`pipeline::research_plans_store`.** Typed helper that
   serializes `ResearchPlan` to `ResearchPlanRow` and back.
   `save_research_plan(store, plan, classified_by)` and
   `load_research_plan(store, id)`. Mirrors `recipes_store.rs`.

5. **Situation room CLI.** New `apps/situation_room/` workspace
   member. Binary `situation-room`. Wires CLI → store →
   classify → persist → print. Production prompt loaded via
   `include_str!`. `recent` subcommand for listing without
   making LLM calls.

6. **`config/sources.toml`.** Single-file source descriptor
   list (replacing the deleted `config/sources/` directory). Ten
   entries spanning authoritative primary, authoritative
   secondary, industry trade press, and general news tiers. No
   adapter framing — these are descriptors the LLM uses for
   nomination, nothing more.

7. **`GeoScope { code, display }` shape.** `ResearchPlan`'s
   `geographic_scope` changed from `Vec<String>` to
   `Vec<GeoScope>`. `code` is canonical (ISO 3166 alpha-2 or
   snake_case region), `display` is the LLM's session-register
   label (≤ 64 chars, any script). Storage and cross-session
   reasoning use only `code`. Validator rejects empty `code`,
   oversized `display`, and control characters in `display`.
   Eight new tests cover these.

8. **Classifier prompt v1.1 → v1.2.**
   - v1.1 added the OFAC SDN worked example, the "when you're
     tempted to leave buckets empty" checklist, and the new
     `geographic_scope` object shape.
   - v1.2 added the explicit "populate `geographic_scope`
     whenever the topic has any scope" rule, prompted by
     observing the empty scope on "Hungarian sovereign debt".

## Architectural decisions ratified in Session 5

- **The LLM is the only specialist.** No Rust adapters per
  commodity, per data source, per anything else. Source
  *descriptors* are inert TOML; *adapters* (when they exist)
  will be generic recipe-driven runtimes, not pre-written
  per-source code.
- **Format vs content split holds for GeoScope too.** The code
  validates structure (code non-empty, display bounded). The
  prompt teaches register selection. The user sees whatever
  label the LLM picked.
- **Plans are immutable, identity is per-classification.** A
  re-classification produces a fresh `id` rather than updating.
  Soft-delete and supersession are real future needs; mutation
  in place is not.
- **Empty-string is the strict-mode "absent" wire form.** xAI's
  structured output rejects schemas with `Option<T>` at the top
  level; we use `String` with empty-string as "absent" for
  fields like `unit_hint`, `assertion_guidance`, and the new
  `display`. Documented in the classifier module's `Authored*`
  type docs and reinforced by validation that treats empty
  strings as absence.

## Files to read first when starting Session 6

In order of importance:

1. `docs/adr/0007-research-function.md` — architectural contract.
2. `crates/pipeline/src/research.rs` — the `ResearchPlan` and
   `GeoScope` shapes the GUI will render.
3. `crates/pipeline/src/research_classifier.rs` — Level-1
   classifier (the GUI's `classify` command will call
   `classify_topic` directly).
4. `crates/pipeline/src/research_plans_store.rs` — typed save/load
   helpers for the GUI's `list` and `get` commands.
5. `crates/storage/src/research_plans.rs` — storage layer behind
   the typed helpers.
6. `apps/situation_room/src/main.rs` — the CLI's wiring, which
   the GUI's Tauri commands will mirror almost exactly.
7. `config/prompts/research_classifier.md` — the v1.2 prompt
   the GUI loads via the same `include_str!` path.
8. This file.

## Rules of the road (carried forward, with a Session-5 addition)

- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Classification produces RecordExpectations, not new schemas.
- Closed enum of 5 extraction modes. Adding a sixth needs an ADR.
- UUIDv7 + dedup_key for identity.
- Security primitives in stockpile_secure. No
  `reqwest::Client::new()` anywhere.
- Structure follows code, not anticipates it. No empty folders.
- Code validates format, prompt teaches content. The LLM is
  trusted for what to put in the plan; the code is responsible
  for what shape it must take.
- **New for Session 5**: when the user pushes back on a
  category of work that "keeps clinging back", purge it. Demo
  binaries, half-baked specialized adapters, and pre-LLM-era
  scaffolding all anchor design choices toward what existed
  before, not what's being built now. Future sessions inherit
  whatever sits in the tree as if it were endorsed; deletion is
  the correct response to "this is no longer the architecture",
  not "leave it for later cleanup".
