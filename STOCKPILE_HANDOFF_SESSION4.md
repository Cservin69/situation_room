# situation_room — Session 5 Handoff

Continuation document for the next session. Covers the state of the
codebase as of end of Session 4, what works, what's known to be
imperfect, and what the next session should pick up.

## State of the codebase

**Phase 4a is complete.** Level-1 classification (the other half of
ADR 0007) is implemented. Combined with the Phase-3c Level-2
recipe machinery, the research function now closes the loop: a
free-text topic from the user produces a structured `ResearchPlan`,
which a Level-2 recipe author can consume to write deterministic
extraction recipes for registered sources.

| Phase | Status | Tests |
|---|---|---|
| 4a.1 — `ResearchPlan::id` | done | regression test on `recipe_author` plan_id threading |
| 4a.2 — `research_classifier` module | done | 20 unit + 1 ignored live |
| 4a.3 — Classifier prompt v1 | done | (prompt; empirical refinement is its own work) |

## What works

- **`ResearchPlan` carries an id.** UUIDv7, set at construction.
  Threaded through `recipe_author::build_validated_recipe` so
  `FetchRecipe::plan_id` is no longer a placeholder. The
  per-run rotation of `dedup_key` that was Session 3's known
  imperfection #1 is fixed; the same logical recipe authored
  twice now converges on a single row via dedup.
- **Level-1 classification end-to-end.**
  `pipeline::research_classifier::classify_topic` takes a topic
  string, an existing-topics injection list, and a registered-
  sources injection list, and returns a validated `ResearchPlan`.
  Mirrors the Level-2 author's shape: `Authored*` types with
  `JsonSchema` derived, schema-constrained LLM call, server-side
  stamping of identity fields, structural validation through the
  existing `situation_room_core::vocab` newtypes.
- **Format-only validation, content-agnostic.**
  The classifier rejects malformed plans (bad topic strings,
  invalid units, out-of-range historical windows, entirely-empty
  expectations) but does not enforce content rules ("must have N
  metrics", "must include China for trade topics"). Richness lives
  in the prompt, not the validator. A documents-only plan for
  "OFAC SDN list updates" is structurally valid; an empty plan for
  any topic is a classification failure.
- **The trust-paragraph contract.** The `interpretation` field is
  prompted explicitly as the moment the user verifies the system
  understood them before any data is fetched. Validation rejects
  empty interpretations.
- **Source priority discipline in the prompt.** The classifier
  prompt walks the LLM through the source-priority hierarchy
  (authoritative primary > authoritative secondary > industry
  trade > general news) and asks for `document_sources` to be
  ordered accordingly. The ordering is the LLM's responsibility;
  the prompt teaches it; no code rule enforces ranks.

## Known imperfections

These are conscious leftovers, not surprises. Each is small enough
to fit in a focused session.

### 1. The classifier crate doesn't talk to the source registry directly

`ClassificationContext::registered_sources` is a `Vec<SourceDescriptor>`
that the **caller** populates. The pipeline crate doesn't depend on
`situation_room-sources`, so anyone wiring up Level-1 has to translate
`SourceMetadata` → `SourceDescriptor` themselves.

This is deliberate (keeps the crate dep graph clean) but means
there isn't a one-liner to "go get classified plan for topic X."
A small `apps/situation_room/` binary or `pipeline::session` module
is the natural place for that wiring; not yet written.

### 2. Topic-injection storage query exists, no caller yet

`Store::topics_in_use(limit)` is in storage and returns
`Vec<TopicUsage>`. The classifier consumes a `Vec<TopicUsage>` of
the same shape. The wiring (storage query → classifier context)
hasn't been done in code yet because there's no Level-1 caller
yet — see #1.

### 3. The classifier prompt is v1 and unrefined

`config/prompts/research_classifier.md` is a careful first version,
not a battle-tested one. Real plans against real topics will
expose rough spots — places where the LLM goes vague, picks weak
metrics, lazily reuses topic tags that don't quite fit. Refinement
is empirical: run classification on five or six varied topics
("lithium supply chain", "EU AI Act compliance", "container
shipping rates", "Hungarian sovereign debt", "OFAC SDN updates",
"semiconductor export controls"), inspect the rendered plans,
patch the prompt where the LLM under-delivers, bump to v1.1.

### 4. Geographic scope is `Vec<String>`, not typed

`ResearchPlan::geographic_scope` accepts both ISO 3166 codes and
free-form region strings ("east_asia", "lithium_triangle"). The
ADR doesn't require typing; the prompt enforces "ISO when
applicable, lowercase region descriptor otherwise." If a future
session wants to type it (a `GeoScope::Country(CountryCode) |
Region(String)` enum), it's a small refactor — but the current
shape is what the rest of the codebase already accepts.

### 5. `apps/demo/` is gone

The Phase-1 single-commodity demo (`situation_room-demo`) and the
Phase-3c end-to-end demo (`situation_room-e2e`) have both been
deleted, on the principle that the situation-room is the real
product and demo binaries shouldn't shape the design.
Consequence: there is currently no executable that exercises
`classify_topic + author_recipe + recipes_store + recipe_apply`
end-to-end. Tests cover each stage; no integration binary covers
the chain. Adding `apps/situation_room/` (or a `bin/` target on
the pipeline crate) is the natural next step — it would also be
the place to wire #1 and #2.

### 6. Carried forward from Session 3

Imperfections #2 (apply-runtime strict deserialization),
#3 (other LLM providers are stubs), #4 (PdfTable extractor
unimplemented), #5 (authoring latency 30-60s),
#6 (SecureHttpClient doesn't surface response headers), and
#7 (crate-level `#![allow(...)]` lint suppressions) from
Session 3's handoff are all still as described.

## Suggested Session 5 priorities

In rough order of leverage:

1. **`apps/situation_room/` binary.** A real executable that
   takes a topic on the command line, calls `classify_topic`
   against xAI, prints the plan, optionally calls
   `author_recipe` against each registered source for each
   document_source hint, persists everything, and prints the
   final state. This is the moment situation_room becomes a usable
   thing rather than a library, and it forces the wiring
   questions in #1, #2, #5 above.

2. **Empirical prompt refinement.** Run #1 against five or six
   varied topics, read the plans, patch
   `config/prompts/research_classifier.md` where the LLM
   under-delivers. This is the kind of work that feels small
   in scope but shapes the product's first impression more than
   any code change. v1 → v1.1 → v1.2.

3. **Anthropic provider.** Reuses the xAI scaffolding pattern;
   lets you A/B model quality on real classification tasks.
   Carried forward from Session 3.

4. **Apply-path strict deserialization.** From Session 3's #2.
   Now that more of the system is real and exercised, the silent-
   field-drop bug is more dangerous.

5. **PdfTable mode.** Bigger; gates USGS demos. Its own session.

I'd suggest priorities 1 and 2 form a natural Session 5 ("ship the
situation room with a prompt that actually works"), with 3 as a
stretch.

## Patches shipped in Session 4

For history.

1. `ResearchPlan::id: Uuid` — added to the struct, threaded
   through `recipe_author::build_validated_recipe`, all four
   call-site struct literals updated. Regression test
   `build_validated_recipe_threads_plan_id` guards the wiring.

2. `pipeline::research_classifier` module — new module, ~870
   lines, mirrors the shape of `recipe_author`. `AuthoredResearchPlan`
   and bucket mirrors with `JsonSchema` derived,
   `ClassificationContext { existing_topics, registered_sources }`,
   `ClassificationError`, `build_prompt`, `classify_topic`,
   `build_validated_plan`. 20 unit tests + 1 ignored live test.

3. `config/prompts/research_classifier.md` v1 — situation-room
   prompt with worked example, ISO/snake_case discipline, source
   priority hierarchy, the trust-paragraph contract for
   `interpretation`.

4. `apps/demo/` deleted — removed both `situation_room-demo` and
   `situation_room-e2e`, removed the workspace member entry. situation_room
   is being built as the situation room; demo binaries should not
   shape the design.

## Architectural decisions ratified in Session 4

- **Level 1 validates format, not content.** The architectural
  intuition that "we trust the LLM for content" gets enforced as
  "code rejects only what's structurally degenerate or
  format-violating." Vocab newtypes (`Topic`, `Unit`,
  `EventType`, `EntityId`) are the format gate. No code rule
  about how many metrics a plan should have, what countries
  should be in scope for which topics, etc.
- **Source registry is injected at Level 1, not just Level 2.**
  An extension to ADR 0007's original design: the classifier sees
  the registered sources via `ClassificationContext` so its
  `document_sources` hints reference real source ids and have
  meaningful priority ordering. Documented in
  `research_classifier.rs` module docs.
- **Server stamps `topic` verbatim.** The LLM's `interpretation`
  is its own paragraph; the user's literal topic string is what
  goes into `ResearchPlan::topic`. The classifier never lets the
  LLM rewrite the user's words into the canonical record.
- **Demo binaries are not products.** The whole `apps/demo/` was
  deleted because it was implicitly anchoring design choices.
  situation_room's product is the situation room; binaries should
  serve that product, not test scaffolding.

## Files to read first when starting Session 5

In order of importance:

1. `docs/adr/0007-research-function.md` — architectural contract
   (carried forward from Session 4).
2. `crates/pipeline/src/research.rs` — the `ResearchPlan` shape.
3. `crates/pipeline/src/research_classifier.rs` — Level-1.
4. `config/prompts/research_classifier.md` — Level-1 prompt.
5. `crates/pipeline/src/recipe_author.rs` — Level-2 (mirror
   pattern; the classifier closely follows it).
6. This file.

## Rules of the road (carried forward from Sessions 2–3)

- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Classification produces RecordExpectations, not new schemas.
  (ADR 0007 Level 1)
- Closed enum of 5 extraction modes. Adding a sixth needs an ADR.
- UUIDv7 + dedup_key for identity.
- Security primitives in situation_room_secure. No `reqwest::Client::new()`
  anywhere. (ADR 0009)
- Structure follows code, not anticipates it. No empty folders.
- When the user pushes back, listen. (Position A vs B was Session
  3's case study; "purge the demo" was Session 4's.)
- **New for Session 4**: code validates format, prompt teaches
  content. The LLM is trusted for what to put in the plan; the
  code is responsible for what shape it must take.
