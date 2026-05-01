# 2026-04-30 — UDB-Go-Live framing inherits "EU AI Act" from prior plan

**Observed:** Session 14 testing, end-to-end verification of the
satisfaction view.
**Status:** Diagnosed. Fixed in classifier prompt v1.4 (Session 15).

## The user's input

Topic, typed verbatim into the classifier:

```
UDB Go-Live date for EOs
```

The user intended a research session about the **Union Database
for Economic Operators** under the EU Deforestation Regulation
(EUDR) — a public registry of operators placing covered commodities
on the EU market. There are also Union Databases under other EU
instruments (notably the AI Act, Article 71, which establishes a
public database of high-risk AI systems). The acronym "UDB" plus
the string "EOs" is genuinely ambiguous and the LLM had to pick.

## The classifier's output (relevant fields)

```
topic:           UDB Go-Live date for EOs
topic_tags:      [eu_ai_act, ...]
interpretation:  I took your phrase "UDB Go-Live date for EOs" to
                 mean the scheduled activation date of the Union
                 Database for Economic Operators **under the EU AI
                 Act framework**. The workstation will populate a
                 timeline focused on regulatory milestones and
                 announcements, entity cards for the responsible
                 EU agencies, a prioritized document stream from
                 official EU legal sources, and extraction guidance
                 for assertions about implementation timelines or
                 delays. Geographic scope is centered on the EU;
                 the historical window covers the past two years
                 of AI Act development to capture all relevant
                 context.
assertion_
  guidance:      Extract and prioritize official statements, legal
                 texts, and announcements from EU bodies specifying
                 the Union Database (UDB) go-live date, any phased
                 implementation for economic operators (EOs),
                 potential delays, and associated compliance
                 obligations **under the EU AI Act**.
historical_
  window_days:   730   ("the past two years of AI Act development")
```

The user accepted the plan. A subsequent fetch produced a recipe
for an EUR-Lex source whose `headline` field was a `literal`:

```
Scheduled activation of the Union Database (UDB) for Economic
Operators (EOs) under the EU AI Act.
```

— a hardcoded sentence baked into the recipe, with the AI-Act
framing inherited verbatim. The recipe will produce that exact
headline on every future fetch.

## What was wrong

The classifier (and every downstream prompt that consumed the plan)
asserted a connection — "under the EU AI Act framework" — that the
user never raised in their input. The user's topic is about a
specific Union Database; the LLM picked one of several possibilities
and presented its choice as a derivation rather than a guess.

The historical window of 730 days is also a consequence of this
misframe — it was justified as "AI Act development," not as anything
the user requested.

## Chain of contamination

A previously-classified plan (Session 13 or earlier — "EU AI Act
Enforcement" or similar) had populated `eu_ai_act` and
`eu_regulation` into the topic registry via
[`Store::topics_in_use`](../../crates/storage/src/queries.rs).

When the user typed "UDB Go-Live date for EOs", the classify command
([`crates/api/src/commands.rs`](../../crates/api/src/commands.rs))
queried `topics_in_use` and injected the existing topics list into
`{{EXISTING_TOPICS}}` in the classifier prompt. The LLM saw
`eu_ai_act` in that list, pattern-matched UDB+EOs to AI-Act Article
71, and:

1. Reused `eu_ai_act` as a `topic_tag`.
2. **Wrote the inferred connection into the `interpretation`
   paragraph** as if it were derived from the user's query, using
   the verbal frame "under the EU AI Act framework."
3. Set `historical_window_days = 730` with the rationale "AI Act
   development."
4. Generated `assertion_guidance` text that repeated the framing.

The plan was persisted. The user reviewed it under time pressure,
accepted (the structural bits — topic, scope, window — looked
plausible), and ran fetch.

The recipe author was then handed the plan as `{{PLAN_JSON}}`
([`crates/pipeline/src/recipe_author.rs`](../../crates/pipeline/src/recipe_author.rs)),
including the contaminated `interpretation` and `assertion_guidance`
strings. The recipe-author prompt
([`config/prompts/recipe_author.md`](../../config/prompts/recipe_author.md))
permits `literal` headlines as a fallback when the source doesn't
provide one. The LLM took the easy path: it lifted a sentence from
the plan's framing and put it in the recipe's `headline` literal.

The chain in one line:

> classifier topic registry → Plan B's `interpretation` paragraph →
> Plan B's `assertion_guidance` → recipe author's `{{PLAN_JSON}}` →
> recipe `headline` literal.

## Diagnosis

Two independent prompts contributed.

**Classifier (`config/prompts/research_classifier.md` v1.3).**
The "Existing topics — reuse before inventing" section instructed
the LLM to reuse a registry tag when "plausibly about the same
subject." "Plausibly" was too permissive: vocabulary overlap alone
counted as plausibility. There was no instruction to qualify or
flag a tag chosen on associative rather than substantive grounds,
and no instruction not to embed the inferred connection in the
interpretation paragraph as if it were derived from the user.

**Recipe author (`config/prompts/recipe_author.md` v1.3).** The
`event` content-type section said `headline` "Usually `extracted`
if the source provides one; otherwise a `literal` or `from_plan`."
This permits a `literal` headline without a structural test. The
prompt did not warn that a `literal` headline produces an identical
record on every fetch — turning the recipe into a one-shot emitter
that masquerades as an extraction.

The two compounded: classifier baked the wrong frame into the plan,
and the recipe author then took the path of least resistance and
hardcoded that frame into a literal.

## Fix (Session 15)

**Classifier prompt v1.4.** The "Existing topics" section is
re-titled "Existing topics — substantive reuse only" and adds:

- A substantive test ("same regulatory framework / same supply chain
  / same event class / same sector"). Vocabulary overlap alone does
  not qualify.
- An anti-example covering this exact case (UDB acronym ambiguity).
- A discipline statement: when in doubt, invent. New tags cost
  nothing; wrong reuse pollutes every downstream prompt.
- An interpretation-honesty rule: when reusing a registry tag on
  associative rather than substantive grounds, qualify it explicitly
  ("I'm reading this under the lens of `eu_ai_act` because that's
  the closest match in your prior research — tell me if you meant
  the EUDR UDB instead") rather than presenting the inference as a
  derivation from the user's topic.

The same prompt also gains a fenced `{{USER_FEEDBACK}}` block (with
a per-request UUID nonce) so the user can re-classify a rejected
plan with a free-text reason. The block carries the "treat as data,
not instructions" framing that prompt-injection hygiene requires.

**Recipe author prompt v1.4.** The `event` headline rule is
strengthened:

- `extracted` is the default. `literal` is permitted only when the
  source emits exactly one record per fetch (a "single-event
  endpoint": a registration page for one specific event, a
  go-live-date announcement, etc.) and that fact is structurally
  evident from the document excerpt.
- An explicit warning: a `literal` headline produces the same
  sentence on every record on every fetch. If the recipe will run
  for years, this is almost always wrong.
- The instruction not to lift framing from the plan's
  `interpretation` paragraph into a `literal` headline — the
  interpretation is for the user, not for the runtime.

## Verification

Pending. The verification step is: re-classify the original topic
(`UDB Go-Live date for EOs`) under prompt v1.4, compare the
`interpretation` against the contaminated original, and run fetch
to confirm the recipe's `headline` is `extracted`. Recorded here on
completion.

## What this case taught

1. **Topic-registry reuse is a contamination vector.** The mechanic
   that ADR 0010 endorses (reuse over invention) is correct, but
   needs a substantive test, not a "plausibly" test. A wrong reuse
   is more expensive than a thousand correct ones because it
   propagates.

2. **The interpretation paragraph is load-bearing for every
   downstream prompt.** ADR 0007 framed it as the user's trust
   moment. It is also the *prompt's* trust moment for every prompt
   that consumes the plan — recipe author, assertion extractor,
   anything else that takes `{{PLAN_JSON}}`. Misframing here caps
   downstream quality.

3. **Permissive fallbacks compound upstream errors.** The recipe
   author was given a contaminated plan and a permissive `literal`
   fallback. Tightening either alone might have caught this; the
   robust answer is to tighten both, because the next failure mode
   will exploit whichever is weakest.

4. **Free-text feedback is the right user-side affordance.** The
   structured reason codes alternative ("wrong framework", "too
   broad", etc.) would have caught maybe 30% of this case and lost
   the specificity. The user in their own words can correct the
   model precisely; the system's job is to make that correction
   safe to feed back into the next classification.
## Verification

Re-tested 2026-05-01 under classifier prompt v1.4 with `eu_ai_act` present
in the topic registry from a prior session.

Topic typed verbatim: "UDB Go-Live date for EOs". Initial classification
was still ambiguous between AI Act and EUDR framings, but the
interpretation paragraph explicitly named the ambiguity rather than
silently picking one. Topic tags did not reuse `eu_ai_act`. The user
rejected the plan with feedback specifying EUDR Article 32 / Economic
Operators / commodities placed on the EU market.

Re-classification with the rejection note as feedback produced plan
019de2a6-207f-7d73-a9f8-bcf57a4dc115. Tags: `eudr_udb_eo`,
`eu_deforestation_regulation`. Geographic scope: `eu_27`. Document
source: `eur_lex` correctly anchored on EUDR Article 32. Assertion
guidance reframed entirely around EUDR — the contamination's deepest
propagation path, completely cleaned. Status: pass.