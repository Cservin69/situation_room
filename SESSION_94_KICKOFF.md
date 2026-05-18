# Session 94 — Kickoff

**Drafted at end of Session 92 from operator-observed product gap; Sn-93
is working a different assignment, do not refocus.**

## The pain (operator-visible)

PBR plan (and any topic with large entity populations: rosters,
catalogues, taxonomies) shows ~10 entities total across all kinds —
classifier-exemplar sized — even AFTER fetch + re-extract.
Screenshots taken end-of-Sn-92:

- Pre-fetch: 10 entities · 4 kinds · 3-2 exemplars each.
- Post-fetch + post-re-extract: STILL 10 entities, same exemplars.
  Events grew 0→96 (a schedule-iterator recipe lit up correctly).
  Documents grew 0→1 (the article fetch). Entities did not move.

Operator's mental anchor: an earlier system iteration populated
**324 bulls + 124 riders** for the PBR topic. That experience
doesn't reproduce in v1 of the current pipeline.

## Why this is happening (diagnosis)

Per-Document extractors exist for four of the five fan-out shapes:

| Shape                       | Per-Document extractor                                 | Lit up since |
|-----------------------------|--------------------------------------------------------|--------------|
| Assertion (relation triples) | `extract::extract_and_persist_assertions`              | Sn-77        |
| Event                        | `extract::extract_and_persist_events`                  | Sn-78        |
| Observation                  | `extract::extract_and_persist_observations`            | Sn-79        |
| EntityAttribute              | `extract::extract_and_persist_entity_attributes`       | Sn-80        |
| **Entity (new instances)**   | **— none —**                                           | **never**    |

`crates/pipeline/src/entity_synth.rs` is the ONLY writer of Entity
rows in v1; it materialises the classifier's `exemplar_entities`
slice at plan-accept time (Sn-76). That slice is sized for
grounding, not for population — 3-5 per kind by classifier prompt
design.

Entity-listing iterator recipes (the path that would yield 324
bulls from one bull-roster page fetch) require the recipe-author
LLM to produce a recipe with `entity_kind` production bindings. The
current `recipe_author.md` prompt explicitly declines these (per
`project_sr_session_59_classifier_bias` memory and Sn-77 prompt
work). Per Sn-77/78 commentary, events extraction was added as a
sibling per-Document extractor precisely to **route around** the
recipe-author's `event_kind`-binding declines — the same routing-
around pattern was never applied to entities because the entity-
population gap wasn't operator-visible until now.

## Two complementary levers

Both close part of the gap. They're not alternatives — they target
different input shapes and should ship together over Sn-94 and Sn-95
(or as a single bundled push if scope holds).

### Lever A — per-Document Entity extractor (the smaller, more uniform fix)

New sibling to the four existing per-Document extractors:
`extract_and_persist_entities`. Article body in, plan's declared
`entity_kinds[]` as the closed-vocab gate, asks the LLM "what
named instances of these kinds appear in this prose?", emits Entity
rows with envelope provenance to the originating recipe.

- Cost: one workhorse-tier call per article-kind Document (added
  to the Sn-77/78/79/80 quartet that already fires per-Document).
- Yield: a few entities per article (e.g. an article about a
  specific bull-riding match might mention 4 bulls + 6 riders).
- Backfill via the Sn-92 re-extract path: the
  `reextract_relations_for_plan` Tauri command was designed as
  the prototype for per-extractor backfill; the same shape can
  ship as `reextract_entities_for_plan` (or roll all extractors
  into one `reextract_all_for_plan` command — operator decision
  at kickoff).
- ADR territory: should the closed-vocab gate be on `kind` alone
  (today's posture for other extractors) or `kind + name`? The
  EntityKindExpectation may carry the `exemplar_entities` slice
  as a seed but not a cap; ADR should pin whether out-of-seed
  names are accepted.

### Lever B — entity-listing iterator recipes (the bigger-yield fix)

The "324 bulls from one fetch" path. Recipe-author prompt
authorises a recipe targeting an entity-listing URL (e.g. a roster
page, catalogue index, member directory) with:
- iterator: a CSS-select or JSON-path that matches each row of the
  listing
- production binding: `entity_kind` shape, mapping the iterator's
  per-row sub-tree onto an Entity record

This requires changes to `config/prompts/recipe_author.md`. The
current prompt declines `entity_kind` bindings (history per
Sn-59). The decline is well-intentioned (entity authoring is
harder than observation authoring; the LLM was failing more often)
but blanket — there's no carve-out for the iterator-listing case.

- Cost: one recipe-author call per (plan, entity-listing URL) at
  authoring time, then one HTTP fetch per refresh. Per-Entity LLM
  cost is zero — the runtime is deterministic apply.
- Yield: hundreds-to-thousands of entities per fetch on
  catalogue-shaped sources.
- Closed-vocab discipline: the prompt teaches structural patterns
  ("a page that lists every member of a set", "rows have a stable
  primary identifier — name, slug, id"), never host strings.
- ADR territory: revisit the entity-author decline rule; carve
  out the iterator-listing case; ship a worked example in the
  prompt (one bull-roster shape, one catalogue shape — different
  generic patterns, same iterator+binding skeleton).

## Sequencing recommendation

Operator picks at kickoff:

1. **Lever A first, then Lever B.** Per-Document entities lights
   up a uniformly-applicable population path immediately; the
   article corpus the operator already has on disk benefits via
   the Sn-92 re-extract path. Iterator recipes (Lever B) layer on
   top for catalogue-shaped sources.

2. **Lever B first, then Lever A.** Iterator recipes yield more
   entities per fetch for catalogue-shaped sources; if PBR is the
   immediate test case, the bull-roster fetch is the path that
   actually produces 324 entities at once. Per-Document
   extraction (Lever A) covers the gaps Lever B can't reach
   (article prose).

3. **Both bundled.** If scope holds (existing-data only, no
   pre-emptive runs, per `feedback_bundle_candidates_one_push`),
   one session can ship both. Lever B's prompt change is the
   bigger LLM-cost item to verify; Lever A's per-Document
   extractor is structurally a sibling-of-four and well-trodden.

## What to read before starting

- `crates/pipeline/src/extract.rs` — the four sibling extractors
  share a shape; Lever A copies that shape.
- `crates/pipeline/src/entity_synth.rs` — current single writer
  of Entity rows; Lever A's persistence path should be coherent
  with it (same envelope shape, same dedup discipline).
- `config/prompts/recipe_author.md` — Lever B's edit target.
- `config/prompts/document_assertions.md` + `document_events.md`
  + `document_observations.md` + `document_entity_attributes.md`
  — the four prompt siblings Lever A's new prompt joins.
- `crates/pipeline/src/reextract.rs` (Sn-92) — the per-plan
  backfill pattern. Lever A's per-Document extractor should
  inherit the same backfill posture.
- Memory entries to read:
  - `project_sr_session_59_classifier_bias` — recipe-author
    `entity_kind` decline history; Lever B's premise.
  - `project_sr_session_77` — the routing-around pattern that
    motivated per-Document Event extraction; same logic
    applies to Lever A.
  - `project_sr_session_80` — EntityAttribute v1 open-vocab
    posture (precedent for Lever A's gate question).
  - `feedback_eval_cost_discipline` — Lever B prompt changes
    need ≥5 trials per the cost-discipline rule if the change
    is structural; Lever A is shape-of-four and may not need
    that ceiling.

## What NOT to do

- Do not modify Lever A's prompt to compensate for Lever B's
  decline rule (or vice versa). Each lever addresses a distinct
  shape; collapsing them masks the real fix.
- Do not seed entity registries from operator hand-curation.
  ADR 0023's Path B rejection precedent applies — closed-vocab
  discipline is structural, not curated.
- Do not let Lever A run on every Document at every fetch
  without a per-(Document, prompt-version) dedup. Re-fetching
  the same article should not pile on duplicate Entity rows.
  (The Sn-92 re-extract command accepts this trade-off for
  Assertions because the consensus pass dedups; Entity dedup
  needs the same downstream layer.)

## Acceptance shape

After Sn-94 (and Sn-95 if split):

- A PBR plan, after one fetch + post-fetch extraction, shows
  Entity counts in the dozens-to-hundreds rather than 10. Number
  depends on which lever shipped — Lever A alone is dozens,
  Lever B alone is hundreds-on-catalogue-shaped-sources.
- `entity_synth`'s plan-accept-time exemplar materialisation
  still works (no regression).
- Re-extract path (Sn-92 pattern) lights up Entity backfill
  for the existing Document corpus on operator click.
- No `entity_kind` recipe author rate exceeds the rate the four
  other binding kinds were authored at in prior sessions
  (sanity check that Lever B's prompt change didn't break the
  decline-rate observation that motivated the original block).

---

## Sn-93 carryover (appended end-of-Sn-93)

Sn-93 bundled 5 candidates (Sn-92's 1+3+4+5 + rel-noise sub-item).
All code landed and verified at unit-test + build + frontend layer
(verify-runbook Stages 1-3 green). What's outstanding:

### ADR 0023 still Proposed — Stage 4 verdict was bogus

`session93-verify.sh` Stage 4 ran end-to-end on the PBR plan but
the AFTER snapshot tried to read `situation_room.duckdb` while the
desktop binary still held the write lock. The script's [a][b][c][d]
verdict was computed against an empty POST file. The actual UI
clicks (re-extract + promote on PBR) probably did write rows; we
can't see them.

Fix already shipped: Stage 4 now (a) warns the operator to Cmd-Q
the desktop before pressing y, (b) detects the
`Could not set lock on file` error in the AFTER snapshot and bails
with a clear message rather than running the verdict math.

To close: open desktop → click re-extract/cull/promote → Cmd-Q
desktop → re-run `bash session93-verify.sh`, press y at the BEFORE→
AFTER prompt. If [a]+[b] hold, flip ADR 0023 line 3
(`Proposed (…)` → `Accepted (Sn-93 Stage 4 verified live …)`) and
add memory entry `project_sr_session_93_adr0023_verified`.

This is independent of Lever A/B work. Sn-94 can land it as a
side-quest in 10 minutes or defer to Sn-95.

### Sn-93 modules Lever A should copy

The two levers in this kickoff have direct Sn-93 precedents to
inherit verbatim:

- **`crates/pipeline/src/reextract.rs::reextract_relations_for_document`** —
  per-Document narrower of the per-plan path. Lever A's
  `reextract_entities_for_document` (or `_for_plan`) should be a
  literal sibling: same source_id parsing, same recipe lookup,
  same shape `ReextractReport`. Wire pattern shipped end-to-end:
  Tauri command + IPC wrapper + DocumentDrawer header button +
  RecordsDashboard toast. Lever A inherits the button shape too —
  one re-extract control per per-Document extractor adds clutter;
  the operator-facing affordance might be a single
  "re-extract all" button that fans out across the per-Doc quartet
  (and Lever A's new quintet member). UX decision at kickoff.

- **`crates/pipeline/src/relation_vocab.rs::filter_drafts_against_plan`** —
  defense-in-depth closed-vocab predicate gate. Lever A wants the
  exact same shape over `plan.expectations.entity_kinds[].kind`:
  drop drafts whose `kind` isn't in the declared list, warn-log the
  drop count + sample. Lever A's `entity_vocab` would mirror
  `relation_vocab` line-for-line; same `PredicateFilterReport`
  shape (rename to `EntityKindFilterReport`); same place to wire
  (after the LLM call, before persistence).

- **`crates/pipeline/src/cull.rs`** — the boilerplate-Assertion
  cleanup pass demonstrates the read-only-preview + destructive-
  confirm UI pattern. Lever A doesn't need a cull pass on day one
  (no false-positive Entity rows yet), but the two-step affordance
  is the right shape for any future "remove Entities whose source
  Document scored Index" pass once Lever A's per-Document path is
  live.

### Sn-93 candidates that didn't fit, deferred to Sn-94+

These came out of Sn-93's handoff and stay open. Not blockers for
Lever A/B but worth a beat at kickoff:

1. **Index-page detector dashboard chip** — surface the per-fetch
   `IndexPageSignal` on the Documents KindCard so the operator can
   tune the link-density / prose-floor thresholds against live
   traffic. Small frontend change once the wire field exists.

2. **Apply the index-page detector to the other three extractors.**
   `run_css_recipe` calls the detector pre-apply; the Event /
   Observation / EntityAttribute extractors all face the same
   "index page has no prose to extract" failure shape. Pipe the
   detector into all three the same way.

3. **Follow-the-link iterator (`FollowLinks { selector, max }`)** —
   ADR 0019 amendment territory. Sn-93's v1.24 prompt teaches
   "one re-author = one followed link"; iterator extension would
   let one recipe fan out to N article URLs per apply pass.
   Direct overlap with Lever B's iterator-recipe work — both
   amend ADR 0019. Bundle them if Sn-94 takes Lever B.

### Verify-runbook lessons learned (worth the 10 LOC for Sn-94's runbook)

Sn-93's verify-runbook iteration produced four reusable patterns
the Sn-94 runbook should inherit on day one:

- **bash 3.2 compat throughout** — macOS ships bash 3.2; no
  `declare -A`, no `${var,,}`. Use plain scalars + `tr`.
- **Per-filter zero-tests guard** — when `cargo test <filter>`
  returns 0 passes, fail loudly with a "did you rsync? did you
  cargo clean?" diagnostic. Auto-check that the new module files
  exist in the cargo repo and that lib.rs declares them.
- **Stage 3 `npm test` check before invoke** — `apps/desktop`
  doesn't define a `test` script today. Probe `npm pkg get
  scripts.test` before invoking; skip cleanly if absent.
- **DuckDB lock-held detection** — AFTER-snapshot ALWAYS prompts
  to Cmd-Q the desktop first; grep the snapshot output for
  `Could not set lock on file` and bail with a clean message
  if it slipped through. The verdict math is worthless against
  a half-read file.

End of Sn-93 carryover. End of kickoff.
