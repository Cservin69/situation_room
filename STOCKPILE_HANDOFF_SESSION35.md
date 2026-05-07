# situation_room — Session 35 followup

**Trigger:** Session 34's prompt revision (recipe_author v1.10)
shipped a "Plan coherence" subsection inside URL discipline plus
a five-item pre-flight checklist, intended to teach the LLM to
substitute the plan's subjects into the source's URL parameters
before authoring. The next live run of `hungarian barley
production` (2026-05-06, against `world_bank_indicators`) failed
the same way: a GDP-shaped recipe in answer to a barley plan,
URL echoed verbatim from the prefetch (`country=all`,
`indicator=NY.GDP.MKTP.CD`), `path: "$[1][0].value"` positional
not filter-shaped, `unit` literal `"USD"`. The legible
Session-32b apply error fired correctly. v1.10 did not move the
needle for this run.

The operator's diagnosis on review: the prompt arc has been
**source-anchored** since its inception. Each session since 30
has added a finer-grained source-side rule (URL hygiene,
endpoint tiers, parameter substitution, type honesty, plan
coherence as a URL subsection). Each revision improved the
prompt locally but reinforced the source-anchored frame
globally. The LLM read more rules about navigating sources well,
not more rules about whether the source was the right candidate
in the first place. v1.10's "Plan coherence" subsection lived
inside URL discipline — it was a remedial check on the source's
URL, not a frame-setting principle.

The fix is structural: invert the frame. The plan is the
specification. The source is a candidate. Author when its bytes
fit the plan's expectations; decline when they don't. In
parallel, single-source plans are the failure shape that makes
honest decline feel like regression — the architectural norm
needs to be 5–10 source nominations per plan so each individual
decline doesn't empty the plan.

Session 35 ships both shifts: a prompt-level revision that
inverts the recipe author's frame (v1.11), a prompt-level
revision that normalizes multi-source classification (v1.5),
and ADR 0007 Amendment 6 formalizing both as architectural
principles.

## What this patch ships

A four-file patch:

1. `config/prompts/recipe_author.md` → v1.11
2. `config/prompts/research_classifier.md` → v1.5
3. `docs/adr/0007-research-function.md` → Amendment 6 appended
4. `README.md` → replaced (the old "README" was the Session 15
   patch document; future contributors opening the repo got a
   months-old patch description instead of a project intro)
5. `CONTRIBUTING.md` → dead `docs/architecture/overview.md`
   link replaced with an ADR-index pointer

No code changes. No DTO changes. No migration. `cargo check`,
`cargo test`, ts-rs codegen, and the desktop UI are all
unaffected. Previously-authored recipes remain valid as data;
the next time the operator flags a recipe, the v1.11 prompt
loads in reauthor.

### 1. recipe_author.md — v1.11

Three structural changes plus a strengthened decline path:

**New top-level section** *"The plan is your specification —
author from the plan, not from the source"*, inserted
immediately after "Your role" and before "The closed extraction
vocabulary." The section establishes the frame inversion
explicitly: plan is spec, source is candidate, decline is
first-class. Names the source-anchored failure shape (read
endpoint, recognize parameter, write recipe around default
response) as the failure mode it targets. Prescribes a four-step
order of operations: read plan → read source → identify the URL
that serves *the plan's specific data* (subjects substituted,
not prefetch defaults) → author or decline.

The section also includes a paragraph on multi-source plans
explaining that the recipe author is one of several running
against the same plan, that decline doesn't leave the plan
empty when other sources fit, and that authors should not
stretch a recipe to compensate for sources they imagine others
might fail on.

**Plan placeholder block relocated** from line ~240 (between
"Defensive variants" and "The source context") to line ~117
(immediately after the new frame section). The
`{{PLAN_JSON}}` / `{{RECIPE_FEEDBACK}}` /
`{{PREVIOUS_FAILURE_REASON}}` / `{{OPERATOR_GUIDANCE}}` block
travels together. The LLM now reads the plan in document order
*before* the closed vocabulary, the decline path, the source
context, and URL discipline. The `replace()`-based substitution
in `pipeline::recipe_author::build_prompt_with_fence_id` is
position-agnostic, so this is purely a textual move.

A new bullet list at the end of the relocated plan block asks
the LLM to name to itself, before continuing: which expectation
bucket, which metric / event_type / kind, which unit, which
geographic codes, which historical window. These are the
load-bearing values the recipe must serve.

**Strengthened decline path.** Added a new bulleted failure
shape between "Structurally inappropriate sources" and the rest
of the cases:

> Source publishes a related but not-the-plan's-asked-for
> metric. The source has the right shape — country-indicator
> API, statistical agency endpoint, regulatory filing index —
> but the specific metric the plan asks for is not in the
> source's catalog. Substituting parameters into the source's
> default endpoint to fetch a different metric than the plan
> asked for is not authoring; it is wrong by construction.

The anti-example is the GDP/barley case from the Session 35
live run, described by *shape* (country-indicator API; plan
asks for an agricultural metric; catalog has macro indicators
only) without naming `world_bank_indicators` as the condition
of a routing rule. This honors ADR 0007's golden rule: prompts
teach principles, not source-by-source routing.

**v1.10 "Plan coherence" subsection retained but reframed** as a
downstream consequence of the new top-level frame. A short
prefix paragraph ties it back: "This subsection is a downstream
consequence of the top-level rule … Read that section first;
this one is the URL-discipline-specific application." The
substantive content (subject placeholders vs envelope shape,
the substitution order of operations, the country-indicator
anti-example) is preserved — it is still useful URL-discipline
guidance, just no longer carrying the load of being the plan
rule.

**v1.10 Pre-flight checklist retained unchanged.**

### 2. research_classifier.md — v1.5

One new subsection plus a worked-example expansion:

**New subsection** *"Source breadth — multi-source by default"*
inserted into "Registered sources — priority discipline" right
before the `{{REGISTERED_SOURCES}}` injection. Establishes 5–10
source nominations per plan as the target band when the topic
admits it. Rationale: each nominated source is handed to a
separate recipe author; some will decline, some will fail at
apply; a plan with 5–10 nominations is robust against half of
those, a single-source plan is fragile against any one of them.

The subsection includes worked-example breadth guidance per
topic shape: commodities supply chain (6–10), regulatory /
policy (5–8), sovereign / macro (6–9), documents-only thin
topic (2–4 with a note that one is fragile). Explicit note that
the band is a target not a hard floor — twelve or three are
fine when each nomination's angle can be named.

**Lithium worked example expanded** from 3 source nominations
(USGS MCS, SEC EDGAR, Argus/Fastmarkets) to 7 (USGS MCS, SEC
EDGAR, World Bank Pink Sheet, IEA Critical Minerals Outlook,
Argus/Fastmarkets, Australian Office of the Chief Economist,
Reuters/Bloomberg). The post-example commentary is updated to
explain the seven-source ordering as cross-tier triangulation
(authoritative primary → authoritative secondary → industry
trade press → general news for events).

**OFAC second worked example annotated** as the explicit
exception, not the template — a documents-only thin topic
where one canonical feed is the source and "angles" are not
multiple, with a note that even there a more rigorous
classification would add one or two authoritative secondaries.

### 3. ADR 0007 — Amendment 6

Encodes both shifts as architectural principles so future
sessions inherit the frame:

- **Principle 1: Plan-first authoring** — the recipe author's
  primary input is the plan, not the source. Sources whose
  bytes don't fit are declines, not recipes to twist into
  shape.
- **Principle 2: Multi-source as the architectural norm** —
  5–10 source nominations per plan (with documents-only thin
  topics as the documented exception). Single-source-per-plan
  was an interim shape used to harden wiring/storage/UI; it
  was never the product.

The amendment also explains why the two principles travel
together: plan-first authoring with single-source plans makes
honest decline feel like regression, pulling future prompt
revisions back toward "author something, anything"; multi-
source breadth with source-anchored authoring produces
seven plausible-but-wrong recipes per plan. Together they
work; separately they degrade.

### 4. README.md replaced

The old `README.md` was the Session 15 patch document (269
lines, "# Session 15 patch" header). A contributor opening the
repo got a months-old patch description instead of a project
intro. Replaced with a real README: what situation_room is,
how it works (the two-level LLM architecture sketched as ASCII
flow), the stack, how to run locally (`./scripts/run_desktop.sh`
after `.env`), project structure, development task runner,
hard rules, license pointer.

### 5. CONTRIBUTING.md fixed

The link to `docs/architecture/overview.md` was dead — the
overview was deleted in this session as part of the
docs-misdirection purge (see "Doc purge" below). Replaced with
a pointer to the ADR index and the new README.

## Doc purge (Session 35)

Pre-session, the operator authorized `rm -rf` on docs that
would misdirect future sessions. Eight misdirecting docs and
seven stale handoffs were removed:

- `docs/sources/adding_a_source.md` — Phase-1 adapter ghost
- `docs/sources/source_catalog.md` — Phase-1 adapter ghost
- `docs/architecture/overview.md` — seven-crate ghost (sources
  + analytics deleted in Session 5)
- `docs/architecture/record_flow.md` — Phase-2 stub
- `docs/architecture/offline_mode.md` — Phase-2 stub (ADR 0008
  carries the real offline contract)
- `docs/schema/envelope.md` — Phase-2 stub
- `docs/schema/record_types.md` — Phase-2 stub
- `docs/schema/vocabularies.md` — Phase-2 stub
- `STOCKPILE_HANDOFF_SESSION26.md` — Track D scaffold
- `STOCKPILE_HANDOFF_SESSION28.md` — Track B scaffold
- `STOCKPILE_HANDOFF_SESSION29.md` — Track C scaffold
- `STOCKPILE_HANDOFF_SESSION30.md` — flag-from-decline scaffold
- `STOCKPILE_HANDOFF_SESSION31.md` — apply-bytes scaffold
- `STOCKPILE_HANDOFF_SESSION33.md` — runtime-bound scaffold
  (advice now superseded)
- `STOCKPILE_HANDOFF_SESSION34.md` — v1.10 scaffold (advice
  explicitly overruled this session)

The deletions are git-recoverable. Decisions live in ADRs;
handoffs are session scaffolding by design.

## What this patch is NOT

- **Not a code change.** Prompt-level + ADR-level + README/
  CONTRIBUTING-level only. No DTO, no migration, no command,
  no UI. `cargo check`, `cargo test`, ts-rs codegen all
  unaffected.
- **Not a re-author of existing recipes.** The previously-
  authored `019dfe98-a821-7421-ac72-47f70e687e17` GDP-shaped
  barley recipe stays in the store as data. The chip mechanic
  surfaces it; the operator flags it; reauthor loads v1.11.
- **Not source-specific routing.** The new failure shape in
  the decline path describes the GDP/barley case by
  *structure* (country-indicator API; agricultural-metric ask;
  macro-only catalog) without naming `world_bank_indicators`.
  Same discipline as Session 34's anti-example.
- **Not a hard floor on source count.** The 5–10 band is a
  target, not a constraint. Empty-band plans (the OFAC case)
  are still valid; over-band plans (genuinely warranting 12)
  are still valid. The discipline is "do not reflexively
  nominate one or two."
- **Not a guarantee.** v1.10 was prompt-level too and didn't
  hold for this failure family. v1.11's frame inversion is
  larger surgery than v1.10's lift-and-checklist; it should
  produce different behavior. If a re-run still produces a
  source-anchored recipe (the LLM reading the new top-level
  frame and the relocated plan block but still authoring
  source-first), the next move is *not* v1.12 prose — it is
  the reauthor flow with the legible apply error feeding back
  as `{{PREVIOUS_FAILURE_REASON}}`. The prompt has done what
  the prompt can do; further drift is an empirical signal to
  use the escalation surface.

## Apply

```
cd /Users/aben/RustroverProjects/situation_room
tar -xzf ~/Downloads/situation_room_session35.tar.gz --strip-components=1 -C .
```

The patch overwrites: `config/prompts/recipe_author.md`,
`config/prompts/research_classifier.md`,
`docs/adr/0007-research-function.md`, `README.md`,
`CONTRIBUTING.md`. Verify by reading the v1.11 line at the top
of `recipe_author.md`, the v1.5 line at the top of
`research_classifier.md`, and the new "Amendment 6" section at
the bottom of ADR 0007.

There is nothing to compile or test — no Rust source touched,
no migration, no schema. After applying, re-run `hungarian
barley production` to see whether v1.11 holds. The expected
new state of the run:

- Classifier: 5–10 source nominations rather than 1 (v1.5's
  multi-source norm). Hungarian barley admits at least:
  Hungarian Central Statistical Office (KSH), Eurostat
  agricultural production, FAO FAOSTAT, World Bank agriculture
  indicators, USDA PSD international, EU CAP-context
  agricultural databases.
- Recipe author against `world_bank_indicators`: either
  authors a recipe with the URL substituted (`country=HU`,
  `indicator=` an agriculture code from the catalog) and a
  filter-expression path, or **declines** with
  `decline_reason` naming that the source's catalog covers
  macro indicators not agricultural commodities at the metric
  granularity the plan asks for. Both are legitimate v1.11
  outcomes.
- Recipe authors against the other nominated sources: each
  produces a recipe or declines independently. The plan
  surfaces records from those that fit (the multi-source norm
  doing its work).

## Architectural lineage

The sessions-30-through-35 arc as it now reads in the docs:

- **30** revealed the wrong-endpoint pattern.
- **32a** fixed the registered endpoint hint and revealed the
  JSON-null pattern.
- **32b** filtered nulls in the helper error and revealed the
  wrong-page pattern.
- **33** bounded the wrong-page failure so it's chip-readable
  and revealed the wrong-subject pattern.
- **34** taught the LLM to substitute subjects (failed: prompt
  was source-anchored).
- **35** inverted the frame: plan is spec, source is candidate;
  multi-source is the architectural norm.

If 35 holds, the next layer the live runs expose is — by the
arc's pattern — different in shape from the prompt-prose
layers that preceded it. Both prompt revisions have now run
their natural course (v1.11 inverts what v1.10 was patching;
v1.5 ratifies what v1.4 implicitly assumed). Further
empirical signal from re-runs determines what the next
session targets: another prompt revision is unlikely to be
the right tool unless the failure shape is fundamentally new.
The reauthor flow remains the escalation surface; ADR 0007's
golden rule (prompts teach principles, not source routing)
remains the guardrail.

End of followup.
