# situation_room — Session 36 handoff

You are starting Session 36. This handoff closes Session 35 (which
shipped two patches and surfaced an architectural finding bigger
than either of them) and frames the next two sessions:

- **Session 36** — ADR design only. No code. Produce a draft of
  ADR 0015 (or ADR 0007 amendment 7 — the choice itself is part
  of the design conversation) ratifying the finding from
  Session 35's live runs. Bring the draft back for review before
  Session 37 starts.
- **Session 37** — implementation per the accepted ADR. Schema
  changes, classifier prompt change, executor change, memory
  query, migration of `config/sources.toml`. One focused
  session of code.

Read this whole document before writing anything. The ADRs in
docs/adr/ are still authoritative; this handoff is the layer
above them, not a replacement. The most important reading order
is at the bottom.

## Status at end of Session 35

Two patches shipped. One worked at the layer it targeted; one
worked at the layer it targeted; the combination surfaced that
the layer underneath both is the actual constraint on the
product.

### Patch 1 (recipe_author v1.11 + research_classifier v1.5 + ADR 0007 amendment 6 + README + CONTRIBUTING + doc purge)

Five-file patch plus a 15-file doc purge. Inverted the recipe
author's frame from source-anchored to plan-anchored;
established 5–10 source nominations per plan as the
classifier-side architectural norm; ratified both as principles
in ADR 0007; replaced the stale Session-15-patch README with a
real README; removed eight misdirecting Phase-1 docs and seven
stale handoffs.

**Empirical outcome**: classifier v1.5 worked. Hungarian barley
classification produced 5 well-tiered nominations (KSH,
Eurostat, World Bank, FAOSTAT, EU CAP/DG AGRI). Multi-source
norm landed at the classifier level on the first try.

Recipe author v1.11 did **not** shift the world_bank_indicators
recipe off its GDP shape on the failing case. Same URL, same
positional path, same `unit: USD`. The frame inversion + the
relocated plan block + the strengthened decline path — none
moved the LLM. This is the empirical signal Session 35's
followup said would route to the reauthor flow rather than to
v1.12 prose. The reauthor flow remains untested for this
specific failure family (the operator used reclassify, not
reauthor — they are different mechanisms; see "Carried-forward"
below).

### Patch 2 (sources.toml expansion: 12 → 17 sources)

One-file config patch. Added `eurostat`, `oecd`, `faostat`,
`ksh_hungary`, `eu_cap`, each with a real `endpoint_hint`
matching the file's existing discipline. No prompt changes, no
code, no migration.

**Empirical outcome**: executor reach went from 1 source per
plan to 4. Hungarian barley re-classified after the registry
expansion nominated KSH, FAOSTAT, Eurostat, EU CAP — all four
with non-empty `preferred_source_ids`. World Bank was correctly
dropped this round (its catalog doesn't fit Hungarian barley).
Plan reached `total_sources=4` in the executor.

But all four recipes failed:

- **KSH** — failed at apply with `bytes were not UTF-8: invalid
  utf-8 sequence of 1 bytes from index 2134`. Real runtime bug
  in the apply path's byte-decoding step. STADAT serves some
  fragments in non-UTF-8 encodings; the apply runtime assumes
  UTF-8 and panics-with-error on first non-conforming byte.
  Not an LLM problem.
- **Eurostat** — recipe author picked `path: "$.value"` against
  the JSON-stat envelope. JSON-stat's `value` is the flat
  values array indexed by the cartesian product of dimensions,
  not a leaf scalar. Apply correctly bounded the failure
  (24,894 bytes vs. the 2,048-byte field cap) and surfaced the
  legible "container vs leaf" error from the Session-32b
  bound. Real prompt failure shape; the patch 2 doc warned
  about it.
- **EU CAP** — pre-fetch landed on a JS-rendered SPA shell.
  Recipe author picked json_path against partial HTML, also
  caught by the same container-vs-leaf bound. Predictable;
  patch 2 documented it as expected.
- **FAOSTAT** — pre-fetch returned 401. The endpoint I assumed
  was open requires authentication. My mistake in patch 2's
  endpoint_hint choice.

Three of the four failure shapes are *not* prompt failures:
they're either runtime bugs (KSH UTF-8 decode), descriptor
errors (FAOSTAT 401), or known-difficult source shapes (EU
CAP JS-rendering). Only Eurostat is a prompt failure family
in the standard sense, and it's a recurring one — JSON-stat
envelope traversal will trip recipes the same way World Bank's
country-indicator JSON did until the prompt teaches it (or the
LLM learns it via reauthor with `{{PREVIOUS_FAILURE_REASON}}`
carrying the legible apply error).

### What both patches together prove

The **classifier** can produce multi-source plans across tiers
when the registry covers the topic. The **executor** can run
recipes against multiple sources in sequence, surface
per-recipe outcomes legibly, and degrade gracefully when
individual recipes fail. The **prompt-level** decline path
under-fires (comtrade and gdelt produced implausible recipes
for Hungarian barley in an earlier run rather than declining;
v1.11's strengthened decline-path prose did not appear to
trigger when the source-plan mismatch was structural rather
than topical).

The patches did what they were designed to do. The constraint
they revealed sits one layer beneath them.

## The architectural finding

The operator surfaced this directly at the end of Session 35
with one question: *"what if I start to research quantum
computer production pipelines?"*

For Hungarian barley, the multi-source story works because
Session 35 patch 2 happened to add agricultural sources to the
registry. For quantum computer production pipelines, the
classifier sees the same 17 sources and reaches for whatever
vaguely fits — `sec_edgar` for IBM/Rigetti filings,
`world_bank_indicators` for nothing useful, `gdelt` and
`rss_feeds` for news. It would not nominate arXiv, IEEE Quantum
Week proceedings, the National Quantum Initiative reports, the
relevant fab-capacity data from semiconductor industry
associations, the patent databases (USPTO/EPO), the EU Quantum
Flagship deliverables, McKinsey Quantum Technology Monitor, or
any of the actual primary sources for the topic. They are not
in `config/sources.toml`.

ADR 0007's golden rule says **the LLM is the only specialist**.
But the classifier's source horizon is bounded by a
hand-curated registry. We are outsourcing topic classification
to the LLM and hard-coding the source universe — exactly the
wrong split. Sources should be the LLM's job. The classifier
already knows the world's data sources; we are constraining it
to a list we maintain by hand.

This is the same anti-pattern Session 5 purged from the
adapter layer and Session 5 again purged from the analytics
layer. `config/sources.toml` is the last vestige. The fix is
not to keep growing the registry (that just delays the failure
to the next un-curated topic); the fix is to **make the
registry a memory of past successes rather than a fence around
what the LLM is allowed to consider.**

The classifier emits `document_sources` with the LLM's own
endpoint URLs. The executor runs against those URLs through
the existing `SecureHttpClient` + `UrlGuard` surface (security
posture unchanged — URL guards run on runtime input the same
way they run on config input). The "registry" becomes a
derived view: a query against the existing `recipes` and
`recipe_fetch_attempts` tables joined by topic_tags, surfacing
"URLs we've successfully fetched against in past sessions
with notes on what worked." Surfaced to the classifier as
context, not as constraint.

The Palantir-shape product the operator has been describing
is fundamentally incompatible with `config/sources.toml` as a
fence. The architecture and the static registry are in
contradiction; one of them has to give.

This is the right empirical moment to make the change: it
landed via four real live runs across three sessions
(Sessions 33–35), it is consistent with the architecture's
golden rule, and it is consistent with the two prior purges of
the same anti-pattern. The change is large enough that it
needs an ADR before code, which is what Session 36 produces.

## Session 36 plan: ADR design only

**Deliverable**: a draft ADR — either ADR 0015 (new) or ADR
0007 amendment 7 (extension). The choice between the two is
part of the design conversation:

- **As ADR 0015**: a standalone decision titled something like
  *"Sources are LLM-emitted per plan; the registry is a memory
  of past successes, not a fence."* Pros: large enough to
  warrant its own ADR; cleanly references ADR 0007's golden
  rule as motivation; future sessions reading the ADR index
  see the decision as a first-class architectural commitment.
  Cons: ADR 0007 is already heavily amended (six amendments
  through Session 35); fragmenting source-related decisions
  across multiple ADRs may obscure how they fit together.
- **As ADR 0007 amendment 7**: keeps the research-function
  decisions co-located. Pros: source emission is the
  classifier's job, and the classifier is governed by ADR
  0007; future sessions reading ADR 0007 see the full lineage
  in one place. Cons: amendment 7 is structurally a *change*
  to how Level-1 output works; that's bigger than what
  amendments 1–6 did, and may belong in its own ADR for
  visibility.

The handoff author leans **ADR 0015** — the change is large
enough and consequential enough that a discoverable
first-class ADR is worth the cost of slightly more
fragmentation. But this is a judgment call for Session 36 to
make first.

### What the ADR must specify

In rough order:

**1. The new shape of `document_sources` in `ResearchPlan`.**

Currently:

```rust
pub struct DocumentSourceHint {
    pub description: String,
    pub preferred_source_ids: Vec<String>,
}
```

Likely new shape (subject to the design conversation):

```rust
pub struct DocumentSourceNomination {
    pub description: String,
    pub endpoint_url: String,        // NEW: classifier provides
    pub priority_tier: PriorityTier, // NEW: explicit
    pub known_id: Option<String>,    // OPTIONAL: matches memory
    pub extraction_strategy_hint: Option<String>,
}

pub enum PriorityTier {
    AuthoritativePrimary,
    AuthoritativeSecondary,
    IndustryTradePress,
    GeneralNews,
}
```

The ADR must decide:

- Whether `endpoint_url` is required or optional. (Required
  closes the loop; optional preserves a "describe-only" path
  for sources the LLM doesn't know a URL for. Handoff author
  leans required, with the "I don't know a URL" case being a
  signal the classifier should not nominate that source.)
- Whether `known_id` is the LLM's responsibility (it
  recognizes the source matches a memory entry) or the
  executor's (it matches by URL host or normalized URL after
  the classifier's emission). Handoff author leans LLM-side
  with executor verification.
- Whether `priority_tier` becomes typed (the enum above) or
  stays implicit-by-order. Handoff author leans typed —
  current order-implied priority loses information when
  rendered, and the executor and UI both want explicit tiers
  for rendering and for cross-source synthesis ranking.

**2. The classifier prompt change.**

Research classifier v1.5's "Source breadth" subsection
currently injects `{{REGISTERED_SOURCES}}` from
`config/sources.toml`. Under the new ADR, the substitution
becomes either:

- `{{SOURCES_MEMORY}}` — a derived view of "URLs we've
  successfully fetched in past sessions" surfaced as context,
  with no constraint on what the classifier nominates.
- Plus instructions in the prompt telling the LLM to emit
  `endpoint_url` directly from its training-data knowledge of
  the topic's authoritative sources.

The ADR must decide what the memory view's shape is, how
deep its history goes, how it filters by topic relevance, and
what its update cadence is (real-time? batched?).

**3. The executor change.**

Today: `fetch_executor::author_recipes_for_plan` resolves
each `preferred_source_id` to a `SourceDescriptor` from
`config/sources.toml`, gets the `endpoint_hint`, pre-fetches
that URL, hands the bytes to the recipe author. After the
ADR: the executor reads `endpoint_url` directly from the
plan's `DocumentSourceNomination`, runs it through `UrlGuard`
(same as today), pre-fetches, hands bytes to the recipe
author. The recipe author no longer needs a source_id to look
up — but recipes still need *some* identifier for storage and
chip rendering.

The ADR must decide:

- What identifier `recipes.source_id` carries when the LLM
  emitted a URL but no `known_id`. Options: hash the
  normalized URL; use the URL host; let the LLM emit a
  short label; reuse the `description` field as the label.
  Handoff author leans URL host (e.g. `apps.fas.usda.gov`,
  `api.worldbank.org`) — stable, human-readable, derivable.
- Whether the executor can run *concurrently* across the
  plan's nominations now that there's no shared registry-
  load step. (Today they're sequential; latency on a 10-
  source plan is the sum of authoring times. Concurrency
  would dramatically improve UX but is a separate
  architectural concern with its own rate-limit, error-
  surface, and ordering implications.)

**4. The memory query.**

The "successful sources" view derives from the existing
`recipes` and `recipe_fetch_attempts` tables. Joins:

```
SELECT
  r.endpoint_url,
  r.source_id,
  COUNT(*) AS attempts,
  SUM(CASE WHEN rfa.records_produced > 0 THEN 1 ELSE 0 END) AS successes,
  ARRAY_AGG(DISTINCT topic_tag) AS associated_topics,
  MAX(rfa.attempted_at) AS last_attempted_at
FROM recipes r
JOIN recipe_fetch_attempts rfa ON rfa.recipe_id = r.id
JOIN research_plans rp ON rp.id = r.plan_id
LATERAL FLATTEN topic_tags FROM rp
GROUP BY r.endpoint_url, r.source_id
HAVING successes > 0
ORDER BY last_attempted_at DESC
LIMIT 50;
```

(Approximate — DuckDB SQL specifics for array_agg + flatten
to be confirmed in Session 37.)

The ADR must decide:

- The size of the memory view surfaced to the classifier
  (top N by recency? by success rate? both?).
- Whether the memory is filtered by the current plan's
  topic_tags (so the classifier sees relevant past sources
  first) or unfiltered (so the LLM has the full picture).
  Handoff author leans hybrid: top-K filtered by topic_tag
  overlap, plus top-M unfiltered as breadth context.
- Whether the memory updates real-time (every recipe success
  immediately changes what the next classifier call sees) or
  is computed once per session start. Real-time is more
  honest; once-per-session is cheaper.

**5. Migration of `config/sources.toml`.**

Three real options:

- **Delete entirely.** The 17 entries become memory-seed
  entries inserted into a one-time migration that creates
  fake "successful" recipe records with the curated
  endpoint_hints. Clean break; the registry concept is gone
  on day one.
- **Keep as a seed file with a clear "this is one-time
  bootstrap" comment**. Read once at first boot, populate
  memory, then become inert. Lower-risk migration.
- **Keep for the demo sources only** (`csv_demo`,
  `json_demo`) used by `#[ignore]` tests, since those need
  stable URLs the test code can rely on. Real sources go to
  memory exclusively.

Handoff author leans option 3 — keep the file but shrink it
to the 2 demo entries. Real sources move to memory. The
`csv_demo` and `json_demo` entries are explicitly test
fixtures, not "the registry."

**6. What happens to existing recipes.**

The ~3 existing recipes in the operator's local DuckDB carry
`source_id` values that match the old registry ids
(`world_bank_indicators`, `comtrade`, `gdelt`). The new
schema doesn't break these; they continue to display and
re-author normally. They don't appear in the memory view
(they pre-date the memory tracking, or they will if the
migration backfills). The ADR should explicitly say "no
retroactive changes to existing recipes," consistent with
ADR 0011's plan-immutability stance.

**7. Alternatives considered.**

Standard ADR section. At minimum:

- Keep the registry, grow it manually as topics arrive.
  (What we've been doing. Doesn't scale; the operator
  surfaced the failure in this very session.)
- Auto-grow the registry from successful runs. (A weaker
  version of the proposed change. Still treats the registry
  as the source of truth; just makes its update mechanism
  less manual. Doesn't address the cold-start problem for
  topics outside the curator's experience.)
- Keep the registry as a hint and let the classifier emit
  *additional* URLs alongside `preferred_source_ids`.
  (Hybrid. Looks like a compromise; in practice probably
  ossifies into "the LLM never bothers emitting URLs because
  the registry is already there." Worth considering for
  comparison, probably not the right answer.)
- Have the LLM emit URLs *and* keep the registry as the
  primary nomination surface, with LLM URLs as fallback.
  (Same critique as the previous.)

**8. Migration sequencing.**

The ADR should specify the order of operations for Session
37 so the implementation is shippable as one cohesive patch
or as a small sequence of patches with each one buildable:

1. New DTO shape (DocumentSourceNomination + PriorityTier).
2. Classifier prompt v1.6 with the new substitution
   (`{{SOURCES_MEMORY}}` instead of `{{REGISTERED_SOURCES}}`)
   and the new emission instructions.
3. Memory query in storage crate.
4. Executor change to consume URLs directly from the plan.
5. `config/sources.toml` shrunk to the 2 demo entries (or
   deleted, depending on ADR decision).
6. Tests updated.

Each step buildable. The classifier emits the new shape; the
storage round-trips it; the executor consumes it.

### What Session 36 should NOT do

- Do not write any Rust code. The ADR is the artifact.
  Spec'ing too late is much cheaper than rewriting code in
  Session 37 because the ADR didn't surface a constraint.
- Do not draft the new prompt in the ADR. v1.6 is Session
  37's work, informed by the ADR.
- Do not re-amend ADR 0007 amendment 6 mid-flight. If the
  new ADR tightens or rephrases what amendment 6 said,
  document the relationship explicitly in the new ADR's
  Context section but leave amendment 6 as-is. ADRs are
  history.
- Do not pre-empt the alternatives section by ruling out
  hybrid approaches. The point of writing them down is to
  surface their cost.

## Session 37 plan: implementation per the accepted ADR

**Deliverable**: one focused session of code shipping the
sequence specified by the ADR. Each step buildable; one
patch tarball at the end with all six steps integrated.

The patch surface, assuming the ADR lands roughly as the
handoff author has sketched:

- `crates/pipeline/src/research.rs` — DocumentSourceHint →
  DocumentSourceNomination + PriorityTier enum.
- `crates/api/src/types_export.rs` — ts-rs DTOs mirror,
  regenerate TS types via `cargo test -p situation_room-api`.
- `crates/pipeline/src/research_classifier.rs` — update
  `AuthoredResearchPlan` shape, validation, prompt
  substitution.
- `config/prompts/research_classifier.md` — v1.6 with
  the new emission instructions and `{{SOURCES_MEMORY}}`
  injection. Probably also a worked-example expansion
  showing the new emission shape.
- `crates/storage/src/sources_memory.rs` (new) — the memory
  query, returning a `Vec<MemorySource>` view.
- `crates/pipeline/src/fetch_executor.rs` — consume
  `endpoint_url` directly from the plan's nominations;
  remove the `SourceDescriptor` resolution step (or keep it
  for the 2 demo sources only).
- `apps/desktop/src-tauri/src/main.rs` — adjust
  composition root if the source descriptor loading is
  removed.
- `config/sources.toml` — shrink or delete per the ADR.
- `crates/pipeline/src/research_plans_store.rs` — migration
  notes if the JSON shape changes (DuckDB stores the plan as
  JSON; the new shape is a superset of the old shape if
  `endpoint_url` is added without removing
  `preferred_source_ids`, which simplifies migration).
- Tests updated: classifier tests, executor tests, the
  ts-rs round-trip test, the live `#[ignore]` classify test.

Estimated patch size: ~600–900 lines of code changes plus
~200 lines of test updates plus the prompt revision.
Comparable in scope to Session 28's track B (decline path +
schema-aware authoring + placeholder wiring) which was a
similar shape of change.

### Session 37 sequencing within the session

Build incrementally. Specifically:

1. New DTOs + ts-rs codegen. `cargo build --workspace` clean;
   `npm run check` clean.
2. Classifier validation + prompt v1.6. Unit tests pass for
   the new `AuthoredResearchPlan` shape including
   round-tripping against `expectations` examples.
3. Storage memory query. Unit test against fixtures.
4. Executor change. Unit tests against in-memory fixtures.
5. `config/sources.toml` shrink. Verify the 2 demo
   `#[ignore]` tests still pass.
6. Live test: classify a topic the old registry could
   never have served (the operator's "quantum computer
   production pipelines" example). Verify the classifier
   nominates real URLs the executor can fetch against.

That order means every step has a green build behind it.
Do not write the entire session and then run cargo check at
the end. Same discipline as previous sessions.

## Carried-forward bugs and items

These do not block Sessions 36 or 37 but should land in
their own follow-up sessions or as opportunistic fixes:

### Real bugs

- **KSH UTF-8 decode failure in apply runtime.** The apply
  path assumes UTF-8; STADAT serves some fragments in other
  encodings. Either (a) detect encoding and decode
  accordingly via `encoding_rs`, or (b) make the decode
  failure produce a legible runtime error rather than a
  panic-with-trap. (Today it surfaces as a structured warn,
  but the recipe is forever-failed against this source until
  the encoding handling is fixed.) Small change in
  `crates/pipeline/src/recipe_apply.rs`.

- **Eurostat JSON-stat envelope is a recurring failure
  family.** The recipe author picks `$.value` (the flat
  values array) instead of a leaf scalar. Same shape as
  World Bank's country-indicator failure family from
  Session 32. Two paths to fix: (a) prompt-side, add
  JSON-stat-specific guidance to recipe_author's "Type
  honesty" section, or (b) reauthor flow with the legible
  apply error feeding back. Per Session 35's followup
  discipline, reauthor is the first response; prompt
  revision only after reauthor doesn't shift the LLM.

- **FAOSTAT 401 on prefetch.** Patch 2's endpoint_hint was
  wrong — the `?area_codes=...&item_codes=...` parameter
  shape requires authentication. The bulk-download endpoint
  may be public; needs investigation. If the new ADR's
  "memory of past successes" path lands, this is moot —
  failed pre-fetches don't enter the memory.

### Untested escalation surfaces

- **Reauthor on world_bank_indicators GDP recipe.** The
  Session 35 followup explicitly named this as the next
  test of the reauthor escalation surface, and it was not
  performed (the operator used reclassify, which is a
  different mechanism). Worth doing in a quiet moment to
  validate ADR 0012 amendment 1's design holds for the
  source-mismatch failure family. The operator note draft
  is in the Session 35 patch 2 doc.

- **v1.11 decline path under-firing.** comtrade and gdelt
  produced implausible recipes for Hungarian barley
  (positional `$` and `$.articles[0].title` for a
  production-tonnes plan) when they should have declined.
  v1.11's decline-path strengthening apparently did not
  fire for these. Whether this is a v1.12 issue or an
  ADR-0015-makes-it-moot issue depends on whether the new
  ADR keeps the recipe-author per-source decline path.
  Likely yes; the decline is still valuable when an LLM-
  emitted URL turns out to be wrong-fit. So a v1.12 prompt
  revision is probably still relevant after Session 37
  ships, focused on this specific weakness.

### Structural items

- **Sequential recipe-author latency.** A 5-source plan
  takes 30–60s × 5 = 2.5–5 minutes wall-clock. The
  executor authors sequentially. Concurrent authoring is a
  separate ADR conversation (rate limits, error
  aggregation, UI streaming). The new ADR may want to flag
  this as out-of-scope-but-noted.

- **Anthropic provider stub.** Carried since Session 3.
  Not blocking anything; xAI works.

- **Lint suppressions in non-`api` crates.** Carried since
  Session 3.

## Hard rules (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`.
  All HTTP through `SecureHttpClient`. Especially relevant
  for the new ADR — LLM-emitted URLs go through the same
  `UrlGuard` as config-emitted URLs do today. The security
  posture does not change.
- Bounds checking on every IPC string input. Especially
  relevant for the new `endpoint_url` field — needs a
  bounded validator, not just URL parsing.
- Tauri commands return `CommandError`, not internal error
  types.
- Generated TS files in `apps/desktop/src/lib/api/types/`
  are written by ts-rs via `cargo test -p
  situation_room-api`. Never hand-edit. The new DTO will
  trigger a regeneration; ship the regenerated files in the
  Session 37 patch tarball.
- ts-rs DTOs and the typed pipeline structs are
  intentionally separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No
  hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Migrations: read previous migration comment blocks before
  writing new ones. DuckDB ALTER traps remain real.
- Code validates format; the prompt teaches content. The
  LLM is trusted for what to put in nominations; the code
  validates structure (URL parses, priority tier is in
  enum, etc.).
- The plan is the specification; the source is a candidate.
  Author when fit; decline when not. (ADR 0007 amendment 6,
  reinforced by anything Session 36 produces.)
- Multi-source by default: 5–10 source nominations per
  plan. (ADR 0007 amendment 6.)
- Prompts teach principles, not source-by-source routing.
  (ADR 0007 golden rule. The new ADR makes this even more
  load-bearing — without per-source routing rules in the
  prompt, the LLM emitting source URLs is itself the
  routing.)
- **New for Session 35**: the sources surface is a memory
  of past successes, not a fence around what the LLM may
  consider. (Pending Session 36 ratification.)

## Continuity note

The operator is rigorous about security ("paranoid about
security" in their own words — earned, not affected),
prefers honesty about uncertainty over false confidence,
and reacts well to direct disagreement when warranted.
Stick to the plan. If you need to deviate, say so and
explain why. The "do not deviate" discipline holds.

The operator's instinct on architecture has been right
every time they have questioned. The Session 35 architect-
ural finding — that `config/sources.toml` is the
contradiction — landed because the operator asked one
question ("what if I research quantum computer production
pipelines?"). When they push back, listen carefully; it is
usually not a question but a gentle correction.

One specific note for Session 36 in particular: the
deliverable is an *ADR*, which is a longer-form artifact
than the prompt patches and tarballs of recent sessions.
The ADR template in docs/adr/ has consistent structure
(Context / Decision / Specifications / Consequences /
Alternatives considered / Code references / Review notes).
Match it. Read three existing ADRs before writing the
fourth; ADR 0007 itself is a good template for structure
since the new ADR is closely related.

Two more carry-forwards from the operator's earlier
feedback in Session 35:

- **No hashtag-prefixed comments in shell commands**. The
  operator's zsh treats `#` as a literal-or-history
  character and breaks. Use `# ` (hash-space) only inside
  files you write to disk; in shell snippets you give the
  operator to paste, lead with the command directly.
- **The operator prefers ruthless deletion to leaving
  half-baked artifacts around**. ADR amendments are not
  deletions; they're additions. But if Session 36 surfaces
  a doc that misdirects future sessions, propose its
  removal.

## Files to read first when starting Session 36

In order of importance:

1. This file.
2. `docs/adr/0007-research-function.md` — the architectural
   contract and amendment 6 specifically.
3. `docs/adr/0010-topic-based-subjects.md` — Topic as the
   universal subject tag, which the new ADR may reference.
4. `STOCKPILE_HANDOFF_SESSION35.md` — the Session 35
   followup with the v1.11/v1.5 lineage. (Patch 2's
   addendum doc, `STOCKPILE_HANDOFF_SESSION35_PATCH2.md`,
   is also worth reading for the registry expansion
   rationale.)
5. `crates/pipeline/src/research.rs` — the `ResearchPlan`
   and `DocumentSourceHint` shapes that will change.
6. `crates/pipeline/src/research_classifier.rs` —
   `AuthoredResearchPlan` and the validation surface.
7. `config/sources.toml` — the file the new ADR replaces or
   shrinks.
8. `config/prompts/research_classifier.md` — v1.5, the
   prompt the new ADR will revise to v1.6.
9. `crates/pipeline/src/fetch_executor.rs` — specifically
   `author_recipes_for_plan` and how it consumes
   `preferred_source_ids` today.
10. `crates/storage/src/recipes.rs` — the recipes table
    that will become the memory's source of truth.
11. `crates/secure/src/url_guard.rs` — the URL validation
    surface that will run on LLM-emitted URLs.

For Session 37, additionally:

12. `crates/api/src/types_export.rs` — the ts-rs DTO
    mirror to update.
13. `apps/desktop/src/lib/api/types/` — the generated TS
    files that will regenerate.
14. `apps/desktop/src-tauri/src/main.rs` — the composition
    root.
15. Existing migrations in `migrations/` — for the
    DuckDB-style discipline (no NOT NULL on existing
    tables, etc.).

End of handoff.
