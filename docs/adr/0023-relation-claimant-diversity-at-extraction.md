# ADR 0023 — Relation-promotion claimant diversity at extraction time (Session 91)

**Status**: Proposed (Session 91 — code-and-prompt landed; live verification deferred)
**Date**: 2026-05-17
**Related**: ADR 0004 (assertion promotion model), ADR 0021 (consensus
promotion stage), ADR 0022 (authority registry DB-backed),
ADR 0017 (closed-vocabulary discipline), `project_sr_no_source_routing`

## Context

Session 90 verified ADR 0022 Stage 2's `agency:document` seed and
confirmed it unblocks `entity_attribute` promotion at N=1 fast-track
(10 `entity_attributes` promoted on the PBR plan). The same verify
run surfaced a second gap the seed does **not** address:
**`relations_emitted = 0`** on the same pass. 17 relation Assertions
sat unpromoted across the system, fragmenting like this:

```
agency:miningweekly · relation · 10
company:microsoft   · relation ·  5
agency:xbox         · relation ·  2
```

These are ten different (kind, from, to) triples claimed by
`agency:miningweekly`, five by `company:microsoft`, two by
`agency:xbox`. Each triple has **one** distinct claimant. N=3
consensus is structurally unreachable. The `agency:document` seed
that fast-tracked entity_attributes doesn't help because the relation
extractor (Session 77 `extract_and_persist_assertions`) stamps each
Assertion with the publisher's claimant, not the synthetic
`agency:document` the entity-attribute path uses.

Three architectural paths were available:

**Path A — claimant diversity at extraction time.** Change the
extractor so a single Document can emit multiple Assertions for the
same triple, each carrying a distinct claimant the document
attributes the claim to. Two sub-options:

- **A1** — multi-row emission from the extractor: prompt teaches the
  LLM to emit one Assertion per (triple, distinct claimant) found in
  the document body. Publisher + cited third parties + quoted
  authorities all surface as separate Assertion rows. The wire shape
  and the persistence path don't need to change because each
  Assertion is already independent today; the change is purely in
  what the LLM is asked to emit.
- **A2** — lower N for relation kinds globally. ADR 0004 amendment
  that gives `RelationContent` its own quorum bar (e.g. N=2). Cheap
  to implement (one config knob), but lowers the corroboration
  threshold for a whole content type to compensate for a measurement
  artifact in the extractor — closes the symptom, not the root cause.

**Path B — operator-curated authoritative registry entries.** Add
rows like `agency:miningweekly` / `metric: supplies_to` /
`consensus_quorum = 1` to the registry. Closes individual gaps; per
`project_sr_no_source_routing` this is the curation pattern memory
exists to prevent inside code/prompts/fixtures (operator-added DB
rows are configuration, not code, but the curation surface to add
them doesn't exist today, and growing the seed list past a handful
creates a junk-drawer policy gap ADR 0022 already flagged).

## Decision

**Adopt Path A, sub-path A1: extraction-time multi-claimant
emission.** The relation extractor's prompt is bumped to
`document_assertions.md v1.2` to teach the multi-claimant shape; the
wire-level `RawExtractedAssertion` schema and the
`extract_and_persist_assertions` orchestrator stay unchanged
(multi-row already supported — N rows for the same triple, distinct
claimants, get persisted as N independent Assertion rows that the
consensus pass naturally groups on content-hash).

Path A1 is the right shape because:

1. **It addresses the root cause.** Real-world relation triples
   typically have multiple claimants in their source documents
   (publisher + cited reporting + quoted authority + first-party
   denial). The v1.0 / v1.1 prompt collapses these to one Assertion
   per (Document, triple) because the worked example showed a
   single-claimant `agency:reuters` row. The fragmentation we see is
   prompt-shaped, not data-shaped.
2. **It generalizes.** Every future Document that produces relations
   benefits from the v1.2 prompt — no per-source curation, no per-
   kind quorum tuning. The fix grows with the corpus.
3. **It preserves the corroboration bar.** N=3 stays the default
   quorum (ADR 0004). A triple still needs three independent
   claimants to promote — it just gets them from one well-attributed
   document instead of waiting for three separate publications.
4. **It honors closed-vocab discipline.** The prompt names *kinds*
   of claimants (publisher, cited third party, quoted authority) and
   the `prefix:slug` closed-vocab shape, not specific source strings.
   No host strings in code, prompt, or schema.

Path A2 was rejected because lowering N for relation kinds
universally is the cheapest possible fix but the wrong shape:
relation Assertions from a single source aren't more trustworthy
than observation Assertions from a single source, and the operator
read of "N=3 across content types" stays intuitive only if it
actually means three across content types.

Path B was rejected explicitly by the operator at Sn-91 kickoff:
"curation doesn't generalize." We accept that this leaves the
curation pattern available as a future fall-through (the registry
already supports it; the operator surface ADR 0022 sketched would
make it usable) but it is not the first lever.

### Concrete prompt changes (v1.2)

`config/prompts/document_assertions.md` is bumped from v1.1 to v1.2.
The wire shape — the `assertions` list of
`{claimant, stance, subject, predicate, object, confidence}` items —
is unchanged. What changes is the guidance the LLM reads:

- A new section "Multi-claimant attribution" explains that when a
  document attributes the same triple to multiple distinct claimants
  by name, the LLM emits **one Assertion per claimant**. Same
  subject/predicate/object, different claimant + stance pairs.
- The worked example grows to show the multi-claimant shape:
  Reuters reporting that USTR cited an industry analyst on a supply
  relationship produces three Assertions (publisher, cited agency,
  quoted analyst) with the same triple and three distinct claimants.
- Stance discipline is sharpened per-claimant: the publisher's
  Assertion is `reported`; the cited agency's Assertion is
  `asserted` (or whatever the cited agency's framing is); the
  quoted authority's Assertion is whatever stance the quote
  carries (`asserted` for a confirmation, `denied` for a denial).
- A cap is named to bound LLM cost: **at most 4 Assertions per
  (triple, document)**. Beyond 4, the LLM picks the most prominently
  attributed claimants. This prevents one source-rich article from
  ballooning the Assertion table.
- The closed-vocab claimant guidance from v1.1 is preserved: every
  claimant is `prefix:slug` shape, where the prefix is one of
  `agency:`, `publisher:`, `company:`, `person:`, `source:`, or
  `unknown` (the fallback). Host strings remain forbidden.

The validator (`crates/llm/src/extraction.rs::validate_one`) does
not need changes — it already accepts any well-shaped row, and
multiple rows with the same triple validate independently. The
`AssertionDraft → Assertion` orchestrator
(`crates/pipeline/src/extract.rs::build_assertion`) does not need
changes either — it builds one Assertion per draft, and the consensus
pass groups by content hash, not by claimant or row.

### What is NOT changing

- **`PromoteConfig::min_independent_claimants` stays at 3.** No ADR
  0004 amendment lands with this ADR. The default quorum bar
  preserves ADR 0004's corroboration commitment.
- **The authoritative registry stays as-is.** No new seed entries.
  ADR 0022 Stage 2's seed ships `agency:document` only; that is the
  whole closed-vocab seed today.
- **The wire shape stays as-is.** `RawExtractedAssertion` keeps its
  single `claimant` field. The multi-claimant case becomes "the
  `assertions` list has multiple rows for the same triple."
- **No retry logic.** v1.2 keeps the v1.0 "single-shot, lenient
  parse, drop garbage" contract.

## Rationale

### Why not change the wire shape to `{triple, claimants: [...]}`

A nested shape would more explicitly express the multi-claimant
structure, but it would also:

- require the validator and the pipeline orchestrator to flatten
  before persistence — adding a transformation surface for marginal
  value;
- diverge from the event / observation / entity_attribute extractor
  wire shapes that all sit on the flat row model — a divergence
  that adds cognitive load every time the four extractors are
  reasoned about together;
- create a re-prompt path mismatch (the nested shape needs a nested
  re-prompt for the validation-exhausted case, which doesn't fire
  today but is named in `ExtractionError::ValidationExhausted`).

The flat shape with N rows for N claimants is the same expressive
power with less plumbing.

### Why a 4-row cap

Three reasons. **Cost containment** — the workhorse-tier LLM call
already produces up to ~20 assertions per Document under the 2048
max-tokens budget; a 4× explosion on a source-rich article would
push the budget. **Diminishing returns** — past 3 claimants the
triple already promotes; the 4th adds confidence-averaging slack
without unlocking new promotions. **Quality discipline** — the LLM
must pick the *most prominently attributed* claimants, which biases
toward the publisher and the most explicitly cited authorities,
exactly the claimants whose stances are most legible.

### Why this is an ADR, not a one-line prompt edit

`feedback_no_easy_wins`: the bias to a quick fix here would be (a)
add `agency:miningweekly` to the registry seed (Path B), or (b)
lower N to 2 for relations (A2). Both close the immediate gap. The
operator named both off-limits because both bias the *system's
contract* in a way that compounds across future sessions — Path B
grows into a junk drawer, A2 weakens corroboration globally. The
prompt change is small in lines of diff but large in stance: it
asserts that the extractor's claimant attribution is **plural by
default** when the source supports it, and the consensus pass should
see that plurality on the first ingest rather than waiting for
cross-source corroboration that may never come.

## Alternatives considered

### A2 — lower N for relations globally

Rejected. Cheapest, but drops the corroboration bar on a whole
content type to fix a measurement artifact in the extractor.
Operator-facing: a single news article makes a relation triple a
"fact" at N=2, where today the bar is three. The disagreement-as-
signal queries ADR 0004 names depend on the bar staying meaningful.

### B — operator-curated registry entries

Rejected at kickoff. Doesn't generalize; grows past a small N into
junk-drawer territory; requires a curation surface that doesn't
exist. The registry remains the right home for *named-source*
overrides when the operator decides one — Path A1 doesn't preclude
this; it just isn't the first lever.

### Nested wire shape `{triple, claimants[]}`

Rejected on plumbing grounds (above).

### Re-prompt the extractor on every triple with N<3

Rejected. Doubles or triples LLM cost per document, and the answers
the re-prompt would get are exactly the ones the v1.2 prompt is
designed to surface on the first pass.

### Synthesize claimants at extraction time (no prompt change)

E.g. always emit a parallel `agency:document` claimant for every
relation triple. Rejected because it's the same closed-vocab violation
the entity_attribute path got away with structurally (per-Document
synthetic claimant) but in a content type where the publisher's
attribution is real and meaningful — overwriting it with a synthetic
would destroy the disagreement-as-signal property.

## Consequences

### Positive

- Triples that today fragment 1× across N publications become
  3-4× *per Document* when the document attributes the claim to
  multiple parties. N=3 consensus fires on first ingest in the
  multi-attribution case.
- The fix grows with corpus quality. Articles with rich attribution
  chains (most reputable journalism) benefit most; articles with
  one-shot reporting still produce the singleton today.
- No new code surfaces, no new schema. Diff is dominated by the
  prompt update.
- The disagreement-as-signal queries (ADR 0004) gain richer source:
  when a publisher reports a triple that the quoted authority
  denies, the two Assertions surface side-by-side with their per-
  claimant stances, instead of one collapsed `reported` row.

### Negative

- LLM cost per Document rises modestly. The 4-row cap and the
  workhorse-tier budget bound the worst case; the typical Document
  with one attribution still emits one Assertion per triple.
- Some triples remain unpromoted: documents with truly one-shot
  attribution (e.g. company press release with no third-party
  citation) still produce singleton Assertions. Cross-document
  consensus is the right answer for these; v1.2 doesn't pretend to
  manufacture corroboration that isn't in the text.
- The 4-row cap is operator-invisible. If a future investigation
  surfaces a case where 5+ claimants matter, raising the cap is a
  prompt edit, not a schema change.
- LLM behaviour drift: prompt v1.2's "emit per-claimant" instruction
  may interact with the workhorse-tier model's training in ways that
  produce false multi-row emissions on single-claimant documents.
  Mitigation: the validator drops rows with invalid claimant
  EntityIds, and the consensus stage's dedup_key still groups by
  content so a duplicate-claimant-row pair from one Document
  collapses on the consensus side. Worst case is a small noise floor;
  the operator can spot-check via the Document drawer.

### Neutral

- Pre-Session-91 relation Assertions in storage are unaffected. They
  remain unpromoted singletons until either (a) a second Document
  surfaces the same triple from a distinct claimant, or (b) the
  operator runs a re-extraction pass on the existing Documents using
  the v1.2 prompt (a deferred future-session lever).
- The `Stance` vocabulary doesn't change. v1.2 just clarifies which
  stance maps to which claimant role.

## Measurement gating

This ADR is committed only because the Sn-91 measurement SQL
(`session91-measure.sql`) was run first and its output framed the
choice. The SQL produces:

- **B1 histogram** — distinct-claimants-per-triple distribution
  across all plans. If the histogram is dominated by N=1, Path A1's
  upside is high (most triples are amenable to multi-claimant
  extraction once the prompt asks). If a meaningful fraction is
  already N≥2, Path A1's marginal improvement is smaller and Path
  A2 (lower N) starts to look closer to optimal.
- **B2 / B3** — sample lists of singleton and pair triples, so the
  operator can eyeball whether the underlying documents plausibly
  carry the additional attribution v1.2 would extract.
- **E2** — rough prefix-shape mention counts in document bodies
  (`agency:` / `company:` substring proxies). Upper bound on how
  many cited claimants the LLM *could* find under v1.2. Not a
  guarantee — most prefix-shaped mentions in arbitrary text won't
  resolve to entity IDs the topic's vocabulary recognises.
- **F1** — Path B counter-check. Counts singleton claimants and how
  many triples each holds back. Confirms that even if Path B were
  pursued, the curation cost grows with claimant count.

If B1 shows the singleton-triples bucket is small and N≥2 dominates,
the path choice may want to revisit A2 specifically; the operator
should re-open this ADR before landing the prompt change. The
expected outcome (per Sn-90 handoff) is a heavy N=1 bucket; in that
case Path A1 ships as written.

## Code references

- `config/prompts/document_assertions.md` — prompt v1.2 (this ADR
  ships the bump).
- `crates/llm/src/extraction.rs::validate_one` /
  `extract_assertions_from_document` — wire-level validator. No
  changes; the multi-row case validates independently.
- `crates/pipeline/src/extract.rs::extract_and_persist_assertions` /
  `build_assertion` — orchestrator. No changes; each draft becomes
  one Assertion row.
- `crates/pipeline/src/promote.rs::group_assertions_for_consensus` /
  `content_hash_for` — consensus pass. No changes; groups by
  content hash, distinct-claimants count rises naturally.
- `session91-measure.sql` — the measurement gating this ADR.

## Review notes

This ADR adopts the same Stage-1 / Stage-2 split posture ADR 0022
used: the prompt change is the Stage-1 commit. Stage 2 — if needed —
is a re-extraction pass over existing Documents under v1.2 so the
pre-Session-91 Assertion pile benefits from the multi-claimant
shape. Re-extraction is operator-triggered and out of scope for this
ADR; it would land as a separate Tauri command + the same prompt v1.2
the live runtime uses.

If a future session changes any of:

- the per-Document claimant cap (4 → other),
- the wire shape (flat → nested),
- the default quorum (3 → 2 for relations specifically),
- the relation-specific authoritative registry entries (none → some
  curated set),

it should land an amendment rather than rewrite this ADR.

### Status path

- **Proposed (Session 91, this commit)**: prompt v1.2 + multi-claimant
  validator/orchestrator tests landed. Measurement SQL drafted.
  No live re-extraction has run; the prompt change fires only on
  net-new fetches under the next executor pass.
- **Accepted (next session)**: required evidence —
  (1) `session91-measure.sql` output's B1 histogram shows the N=1
  bucket dominates over N≥2 (confirms Path A1 over A2);
  (2) a fresh-fetch live extraction pass produces ≥1 Document with
  ≥2 distinct claimants on the same triple, surfaced in the
  Assertions panel.
  (3) the consensus pass groups the multi-claimant rows on
  identical content hash (the new `build_assertion_groups_…` test
  pins this offline; live evidence is the same triple promoted at
  N≥3 from a single Document).
