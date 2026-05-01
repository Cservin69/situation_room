# 2026-05-01 — EUR-Lex CELEX instance URL with naive selector for multi-event plan

**Observed:** Session 15 Phase D testing (case C.2,
"EU AI Act high-risk system enforcement timeline").
**Status:** Diagnosed. Fix shipped in recipe-author prompt v1.5
(Session 16). Verification pending re-run.

## The plan

Topic, typed verbatim into the classifier:

```
EU AI Act high-risk system enforcement timeline
```

Classifier output (under v1.4) was correct. The relevant fields
the recipe author saw:

```
expectations.event_types:
  [0] enforcement_milestone
  [1] guidance_published
  [2] national_implementation
expectations.document_sources:
  [0] preferred_source_ids: ["eur_lex"]
```

The plan's `event_types` bucket holds three distinct event-type
expectations. The plan's `document_sources` nominates `eur_lex`
as the binding source. `eur_lex` in `config/sources.toml` carries
an `endpoint_hint` that points at the EUR-Lex search index
(`https://eur-lex.europa.eu/search.html?...`), described in the
TOML comment as the place to author a `css_select` recipe
"targeting the result-list rows".

## Observed recipe

The Level-2 author produced one recipe (Session 15 fetch):

```
source_url: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32024R1689
extraction:
  mode: css_select
  selector: "title"
produces:
  - record_type: event
    expectation: { event_type: { index: 0 } }   # enforcement_milestone
    field_mappings: [...]
```

Three things wrong, in increasing order of severity:

1. **Naive selector.** Even on the right URL, `"title"` alone
   doesn't reach a per-record headline structure on EUR-Lex's
   listings. On the chosen CELEX page it matched nothing.
2. **Single binding for a three-expectation bucket.** The
   author produced one binding for one event type
   (`enforcement_milestone`) and silently ignored the other
   two (`guidance_published`, `national_implementation`). No
   surface anywhere told the user the recipe under-covered the
   plan.
3. **Instance URL for a multi-event plan.** The recipe points
   at the EUR-Lex CELEX page for a single specific regulation
   (`32024R1689`, the AI Act). An instance page for one
   regulation can produce at most one event item per fetch —
   it cannot structurally yield three event-type records, no
   matter how the rest of the recipe is shaped.

The failure was graceful: the recipe failed at apply (selector
matched nothing), the run closed with `succeeded=0 records=0`,
no garbage was inserted, no crash. The deterministic-runtime-
catches-the-LLM property held. The *recipe-quality* problem is
the issue.

## What was wrong

The author's job was to produce a recipe that, given the plan's
three event-type expectations, would extract event records from
a source that structurally supports event listings. The right
recipe shape:

```
source_url: https://eur-lex.europa.eu/search.html?...   # listing endpoint
extraction:
  mode: css_select
  selector: <a precise per-result-row selector>
produces:
  - record_type: event
    expectation: { event_type: { index: 0 } }   # one binding
    field_mappings: [...]
```

— a listing endpoint (so the recipe stays useful as items roll
over), a structurally-grounded selector (so apply doesn't
return empty), and one binding for the most-load-bearing
expectation. (Under today's runtime, a single recipe can't
iterate per-item across the listing and emit a record per item;
honest narrow coverage is the right move. See the v1.5 changelog
for the architectural note.)

What the author did instead was anchor on a regulation number it
likely saw in the document excerpt (the AI Act's CELEX number
`32024R1689` is the most recognizable regulation in any AI-Act-
related EUR-Lex pre-fetch) and pick the URL closest to that
number, regardless of whether the URL's *tier of resource*
matched the plan's *tier of need*.

## Chain of contamination

This failure differs in shape from the Session 14 classification
case (`classification/2026-04-30-udb-eu-ai-act-framing-leak.md`).
There was no classifier misframing. The plan correctly
identified three event-type expectations and nominated EUR-Lex
as a listing source. The plan's `interpretation` paragraph was
not contaminated.

The chain ran entirely through the recipe-author prompt:

1. Plan correctly nominated `eur_lex`.
2. Executor pre-fetched `eur_lex`'s `endpoint_hint`
   (`.../search.html?...`) and handed the bytes to the recipe
   author.
3. Recipe-author prompt v1.4 had a "URL discipline" section that
   told the author what URLs to *avoid* (`example.invalid`,
   wrong host) but said nothing about *which tier of URL* on the
   right host the author should pick. The instance-vs-listing
   distinction was implicit at best.
4. Recipe-author prompt v1.4 also said nothing about how the
   `produces` array should mirror the plan's bucket size.
   "≥ 1 binding" was the only structural guidance. A single
   binding for a three-expectation bucket was permitted by both
   the prompt and the validator.
5. The author chose the CELEX URL it saw mentioned in the
   excerpt and wrote one binding pointing at the first event
   type. Recipe persisted; ran; failed gracefully at apply.

The failure was visible only because the user was looking at the
SatisfactionPanel record count and saw zero. Without that
panel the silent partial coverage would have been invisible
indefinitely.

## Diagnosis

Two gaps in `config/prompts/recipe_author.md` v1.4:

**Endpoint-tier discipline was missing.** The "URL discipline"
section forbade synthetic placeholders and wrong hosts but did
not address the instance-vs-listing distinction. The prompt's
`endpoint_hint` was treated as informational by the LLM, not as
the maintainer's considered choice for "where the source's
listing lives." For multi-expectation plans, deviation from the
hint toward an instance URL was an unforced error the prompt
didn't push back on.

**Coverage discipline was unspecified.** The prompt told the
author it could produce ≥ 1 bindings (capped at 20) and that
two bindings on the same expectation were rejected. It said
nothing about how `produces.len()` should relate to the bucket's
expectation count. The validator's structural constraints were
the only guidance, and the validator was permissive of
under-coverage by design.

Compounding factor: because the apply runtime extracts one
scalar per fetch (`crates/pipeline/src/recipe_apply.rs`), and
the executor authors one recipe per source per call
(`crates/pipeline/src/fetch_executor.rs::load_or_author_recipes`),
honest per-item iteration over a listing endpoint isn't
expressible in a single recipe today. Any prompt-level guidance
on coverage has to acknowledge this constraint rather than
prescribe a shape the runtime can't deliver.

## Fix (Session 16)

Recipe-author prompt v1.5
(`config/prompts/recipe_author.md`) adds:

- A new subsection inside "URL discipline" titled "Endpoint
  discipline — instance vs listing". It instructs: when the
  plan's matching bucket holds two or more expectations of the
  same record type, the URL must be a listing endpoint. If the
  source has a registered `endpoint_hint`, prefer it; deviate
  only with a clear source-specific reason.
- A worked anti-example covering this exact case (three
  event-type expectations, EUR-Lex CELEX URL, why it's wrong).
- A new top-level section "Coverage discipline — bindings vs
  expectations" that names the runtime constraint (one scalar
  per fetch), describes when multiple bindings off one scalar
  constitute genuine vs fake coverage, and instructs the author
  to prefer honest narrow coverage over padded bindings when
  the single extraction can't honestly populate the full
  bucket.
- Two new bullets in "What NOT to produce": one against
  instance URLs for multi-expectation buckets, one against
  padded `produces` arrays.

The output contract is unchanged — same JSON Schema, same field-
source kinds, same binding rules. Recipes already authored
remain valid; recipes that exhibit the symptoms above can be
rejected and re-authored under v1.5 by the user.

## Verification

Pending. Verification is: re-run the C.2 fetch (topic
"EU AI Act high-risk system enforcement timeline") under recipe-
author prompt v1.5 against `eur_lex`. Confirm that the produced
recipe's `source_url` is on `eur-lex.europa.eu/search.html...`
(or another listing endpoint) rather than on
`/legal-content/EN/TXT/?uri=CELEX:...`. Confirm that the
`produces` array's coverage is either honestly narrow (one
binding for the most-load-bearing expectation) or honestly
broader (multiple bindings each pointing at a distinct
expectation index where the single extracted scalar genuinely
populates each), with no padding. Recorded here on completion.

## What this case taught

1. **The validator's structural constraints aren't a coverage
   contract.** A recipe with one binding for a three-
   expectation bucket passes validation cleanly. The user only
   sees the under-coverage at apply time, by inspecting record
   counts. Prompt-level coverage discipline is the right place
   to fix this; structural rules in the validator would need to
   distinguish honest narrow coverage from silent partial
   coverage, which the runtime can't do.

2. **`endpoint_hint` is load-bearing, not informational.** When
   it points at a listing endpoint (as the EUR-Lex hint does),
   it's the maintainer's considered choice for the right tier
   of resource. The recipe-author prompt should say so
   explicitly rather than trust the LLM to read the comment in
   `config/sources.toml`.

3. **One-scalar-per-fetch is a real architectural constraint
   the prompt has to be honest about.** The handoff for
   Session 16 suggested "produce N bindings (or N recipes if
   the source can't cover them all in one)"; the "or N recipes"
   path is currently not supported by `author_recipe`'s
   one-recipe-per-call API, and the "N bindings off one scalar"
   path produces fake coverage when the bindings don't actually
   differentiate. The prompt's job is to nudge toward the
   shapes the runtime can deliver honestly, not the shapes a
   future runtime could.

4. **Recipe-quality failures need their own evidence trail.**
   The classification README explicitly carved out classifier
   misframings as a separate category from ADR 0012's runtime
   classes. This case carves out the third category (recipe-
   author prompt quality) by extension, parallel to both. See
   this directory's README.
