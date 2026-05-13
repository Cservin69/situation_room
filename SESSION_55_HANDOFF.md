# Session 55 — Handoff

Session 55 shipped the Patch 3 prompt edits, the Stage 1 and Stage 2
parallelism work, and the Patch 4 prompt edits. One pre-existing
`normalize_numeric_candidate` test failure (estimate-prefix EU-gate
ordering) was fixed in passing. Live-tested on the lithium plan with
a clean 3.9× wall-clock speedup and same record count as Patch 2.

## What landed

**Patch 3 prompt edits** (Session 55 first commit, recipe_author v1.17
+ propose_source_url v1.4). Four sub-pieces in one combined patch:

- **3A** (leaf-not-container): new "Selecting the mode that fits,
  and the selector that targets a leaf" section in
  `recipe_author.md` between the closed extraction vocabulary and
  the decline path. The rule: a binding produces one scalar; a
  selector that returns >2 KB has matched a container, not a leaf
  cell. Verbatim apply-stage decline shape ("selector matches a
  container element (body, div, table) instead of a leaf") quoted
  inline so the LLM sees the same string the validator emits.
- **3B** (mode-vs-content-type coherence): new prose in the same
  section. Each extraction mode requires a specific content-type
  (`css_select` → HTML, `json_path` → JSON, etc.). Cross-mode
  authoring is a category error.
- **3C** (required-field discipline): new pre-flight paragraph at
  the end of "What the records you produce look like" + new sixth
  bullet in the URL-discipline pre-flight checklist. The rule:
  walk the schema's required fields and confirm each has a
  `field_mappings` entry before submitting.
- **3D** (L1 source identity is provenance hint, not contract):
  new section in `propose_source_url.md` between the reasonable-
  shot disposition and "What NOT to propose". The rule: a
  nomination description naming a specific source ("Fastmarkets
  battery-raw-materials price assessments") is a provenance class
  hint, not a binding contract. The reasonable-shot disposition
  applies even when the L1 names an unreachable host.

**Stage 1 parallelism** (Session 55 second commit). In
`crates/pipeline/src/fetch_executor.rs::author_for_nomination`,
the per-target loop became a `futures::future::join_all` over
`targets.iter().map(|t| author_recipe(...))`. The four (or fewer)
author calls per accepted URL run concurrently; results split into
`authored_this_attempt` / `declined_this_attempt` exactly as before.
No `tokio::spawn` — `join_all` keeps futures on the same task, no
`Send` bound on `&dyn LlmProvider` borrows. Added `futures = {
workspace = true }` to `crates/pipeline/Cargo.toml`.

**Stage 2 parallelism** (Session 55 third commit). In
`load_or_author_recipes`, the sequential nomination loop became a
`FuturesUnordered` driven from an `Arc<tokio::sync::Semaphore>`
sized by `SR_LLM_CONCURRENCY` (default 8, min 1, read once at the
start of the function). Inside `author_for_nomination`, every
Cheap-tier `propose_source_url` and Workhorse-tier `author_recipe`
call gates behind `.acquire().await` — permits held only across
the LLM await, never across HTTP prefetch or DuckDB writes. The
semaphore lives entirely inside `load_or_author_recipes`; `Arc`
clones into each `author_for_nomination` future. `ExecutorContext`
struct shape is unchanged, so the 56 test construction sites are
untouched. Test discipline note added to the prompt: cargo test
with `SR_LLM_CONCURRENCY=1` when assertions depend on log-line
ordering or completion order.

**Normalizer fix** (in passing). The `normalize_numeric_candidate`
function in `recipe_apply.rs` ran the EU-locale gate (`,` after
`.` → return None) before stripping estimate prefixes. The prefix
`"est. "` contains a `.`, so an input like `"est. 1,200"` tripped
the gate falsely and returned `None`. Reordered: estimate-prefix
strip runs first, then the gate runs against the post-strip body.
Genuine EU-locale inputs (`"1.234,56"`, `"est. 1.234,56"`) still
decline cleanly. Restored `normalizer_accepts_estimate_prefixes`
test to green.

**Patch 4 prompt edits** (Session 55 fourth commit, recipe_author
v1.18). Five sub-pieces in one combined patch, motivated by two
outside reviews of v1.17:

- **4A** (capability exclusions co-located with decline path): new
  sub-section "Capabilities the runtime gives you — these are NOT
  decline reasons" inside the decline-path section. Negates the
  decline rationales the v1.16 normalizer documentation failed to
  suppress: comma-thousands, currency markers, estimate prefixes,
  trailing units, scientific notation, internal whitespace, time-
  series JSON nulls (with the filter-expression syntax pointer),
  the 2 KB field cap (framed as a container-selector catch),
  iterator + dedup_key_field for listings. Each shape is named as
  a "do not decline citing this" rule. The v1.16 normalizer prose
  in "Type honesty" stays; 4A is a re-statement at the decision
  frame, not a relocation.
- **4B** (decline-conditions checklist): new sub-section
  "Decline-conditions checklist — all four must be true" at the
  end of the decline path. Four conditions: bytes not parseable
  in the prefetch's content-type; no peer publisher in the same
  data class; required fields cannot be sourced; named
  alternative endpoint that would also fail. Item 4 forces the
  LLM to name the alternative it considered. Does NOT invert the
  default disposition — recipe authoring's cost arithmetic
  (recipes run forever, vs. URL proposer's one-attempt cost)
  makes the inversion appropriate for `propose_source_url` (Patch
  1 already landed it there) but inappropriate here.
- **4C** (compress plan-coherence prose): the v1.10 "Plan
  coherence — the URL must serve the plan's subjects" subsection
  duplicated 600 words of explanation that the v1.11 top-level
  frame already covered. Compressed the explanation to one
  paragraph that points back to the top-level frame; kept the
  order-of-operations and the Session-33-followup anti-example.
  Net ~400 words shorter without losing the rule.
- **4D** (imperative schema constraints in "What to produce"):
  cheap insurance restating, in imperative voice, the three
  deserialization-failure shapes (unknown fields, closed-enum
  violations, type mismatches) immediately above the top-level
  shape description.
- **4E** (trim quiet anti-example bullets in "What NOT to
  produce"): two URL-placeholder bullets collapsed into one;
  interpretation-paragraph-lift bullet removed (covered in the
  Content type reference > headline section); "Do not interpret
  the document" reframed positively as "Extract values, do not
  summarize them." 14 bullets → 11 bullets; no lost rules.

## Live-test verification (Patch 3 + Stage 1 + Stage 2)

Lithium plan, 7 nominations, run from 07:58:20 to 08:00:49 (~2:29
wall-clock). Baseline before parallelism was 9:42 on the same
plan — **3.9× speedup, 7m13s saved**. Records produced: 2 (USGS
production index:0 and reserves index:1, both via
`pubs.usgs.gov`). Same record count as Patch 2's verification —
no regression. xAI 429s observed: 0.

Visible verification of each piece:

- **Stage 2 cross-nomination parallelism**: all 7 propose-URL
  calls fired within a 43 ms window (07:58:20.948–.991). Logs
  interleave by completion as expected; `nomination_id` field
  present on every line.
- **Stage 1 per-target parallelism**: USGS Workhorse author calls
  at 07:59:02.579 — four targets returned within the same
  millisecond.
- **Patch 3A (leaf-not-container)**: SEC EDGAR via Yahoo Finance
  attempt declined with `extraction returned 1651394 bytes;
  recipes produce single scalar values and the runtime caps
  individual field values at 2048 bytes. Likely cause: the
  selector matches a container element (body, div, table) instead
  of a leaf` — the verbatim 3A decline shape, caught at authoring
  time. mining.com produced the same shape at 15045 bytes.
- **Patch 3B (mode-vs-content-type)**: not exercised this run (no
  cross-mode attempt landed), but didn't break anything.
- **Patch 3C (required-field discipline)**: not exercised this
  run; the World Bank failure was the related-but-different Piece
  B "string in numeric slot" decline (`invalid type: string
  "Commodity Markets", expected f64`). The missing-required-field
  shape didn't manifest.
- **Patch 3D (L1-source-as-class-not-contract)**: partial. The
  Fastmarkets decline now reads `"(or equivalent coverage
  publisher) yields daily hydroxide/carbonate spot prices without
  login, JS rendering, or guesswork on routes. Decline to avoid
  fabrication."` — the proposer DID consider the disposition.
  Concluded no plausible coverage publisher serves daily lithium
  spot prices without login/JS, which is a defensible read of the
  data class. Reuters nomination took the disposition further,
  with a Reuters → Bloomberg → Mining.com pivot chain across
  three attempts (each blocked or unauthorable, but the chain
  shows the disposition firing).

Patch 4 has not yet been live-tested. The empirical question for
the next session: does the "you-may-NOT-decline-for-these"
adjacency in 4A actually suppress the comma-thousands /
2-KB-cap / iterator-shape declines that v1.16 didn't catch?

## Wall-clock arithmetic

Of the 2:29 total wall-clock, the
`industry.gov.au` nomination's two back-to-back 60s prefetch
timeouts (attempts 1 and 2) account for ~2:00 by themselves —
that nomination's third attempt didn't surface a working URL
either, but it did so via a proposer decline at ~08:00:45. Every
other nomination finished by 07:59:17. Excluding industry.gov.au,
the entire plan completed in ~57 seconds. The wall-clock floor
under the current architecture is the slowest single nomination's
end-to-end time, not the sum of nomination times — Stage 2's
parallelism reaches its asymptotic best when the slowest
nomination dominates.

If we want to go below ~2:00 on lithium-class plans, the lever is
not parallelism. It's either (a) tightening the prefetch timeout
below 60s for hosts with prior-attempt timeout history (a
host-backoff feature, not a routing rule — see the closed-
vocabulary discipline memory), or (b) capping the propose-URL
retry count more aggressively when prior attempts on the same
host all timed out.

## Patch 4 verification — what to look for next session

A live-test on the lithium plan with v1.18 in place. Acceptance
criteria, in priority order:

1. **The Yahoo-Finance-class container selector should not recur.**
   Patch 3A made the apply-stage decline shape visible at
   authoring time; Patch 4A's "you may NOT decline citing 2 KB
   field cap" framing should make the LLM author leaf selectors
   the first time, not learn after the validator declines.
2. **The Fastmarkets-class decline should produce at least one
   coverage-publisher attempt.** Patch 4B item 4 ("name the
   alternative endpoint that would also fail") forces the
   substitution work into the decline rationale. If the rationale
   still reads "no other coverage publisher exists" without
   naming one tried, 4B isn't firing.
3. **The USGS production attempt should succeed without
   commas-formatted-numbers self-decline.** Patch 4A's "you may
   NOT decline citing comma-thousands" should suppress the
   v1.16-residual decline shape.
4. **Record count should be ≥ 2** (no regression vs. Patch 3
   verification).
5. **Wall-clock should be ≤ 2:30** (Patch 4 is prompt-only; no
   architectural change to the parallelism asymptote).

If 1–3 fire as designed, Patch 4 is the right shape and the next
move is the deferred work below. If 1–3 don't fire, the prompt-
engineering ceiling is closer than expected and the
reasoning-block experiment (deferred section below) becomes the
next move.

## Deferred items — worth thinking about, not shipping today

The two outside reviews surfaced three structural ideas that are
worth their own session-or-more of design work. Operator decision
on this handoff is to defer them until Patch 4 is verified live.

### Deferred A — Replace inline anti-examples with positive gold-standards

The current prompt embeds anti-examples inline alongside positive
worked examples (e.g. the EUR-Lex CELEX-instance anti-example in
"Endpoint discipline," the World-Bank-default-indicator
anti-example in "Plan coherence"). The LLM-behaviour evidence
that "negation is weaker than positive pattern exposure" is well
supported in the literature. Replacing some inline anti-examples
with **3–5 gold-standard recipes** that each demonstrate the
positive form (perfect leaf selector, all required fields bound,
correct content-type/mode pairing, reasonable-shot logic on a
borderline source) would reduce the model's exposure to wrong-
shape patterns and tighten the prompt.

This is careful surgery, not wholesale removal. Some
anti-examples are doing real work (the EUR-Lex CELEX failure
hasn't recurred since v1.5). Trim selectively; don't evacuate.
Patch 4 already trimmed two redundant anti-example bullets in
"What NOT to produce"; this deferred item is the larger inline-
example surgery in the body.

The gold-standard recipes should be principle-only (class
shapes, no host strings) per the closed-vocabulary discipline
memory entry.

### Deferred B — Decision-tree restructure (potential v2.0)

The recipe-author prompt is 1,641+ lines and growing. The
information architecture is "reference manual organized by
topic," which assumes the LLM reads sequentially and applies the
right rule at the right moment. Empirically the LLM does not do
this — it pattern-matches on local context and overweights
adjacency. Wise Man #2's diagnosis ("rules in prose 1,000 words
from the moment of decision underfire") is the right diagnosis;
the current prompt's pre-flight checklist exists precisely
because rules buried in prose lose to the most recent rule
adjacent to the JSON output decision.

The proposed v2.0 shape: replace the current section ordering
with a **decision tree** where every major decision point is a
heading the model must answer before proceeding (Step 1: name
the contract; Step 2: choose extraction mode; Step 3: verify
required fields; Step 4: author the selector/path; Step 5: final
coherence check). The model's reasoning would flow through the
steps in order; each step is the rule at the moment of decision.

Why deferred: this is a rewrite, not an edit. Worth doing as a
v2.0 only after a side-by-side eval against v1.18 on the lithium
fixture (and ideally against alternative models — see the
multi-vendor question in the counsel evaluation
`crit_xai_session_55.md` if it gets written). If v1.18's
adjacency-at-the-decision-frame moves are sufficient (Patch 4
verification meets criteria 1–3 above), the v2.0 restructure may
not be necessary; if they're not, the v2.0 restructure is the
clear next move.

### Deferred C — Reasoning block before JSON output (paths 1 + 2 only)

Both outside reviewers proposed a "reasoning block" the model
emits before its final JSON, on the theory that explicit stepwise
reasoning improves structured-output quality. Both reviewers
missed the structured-output channel constraint: xAI's
`StructuredOutputSchema` mode emits one schema-conformant JSON
object as output; you cannot mix reasoning text with it.

Three implementation paths, of which only paths 1 and 2 are
on the table per operator decision (path 3 was reviewed and
omitted):

**Path 1 — Turn off structured output for `recipe_author`,
do schema validation post-hoc.** The LLM emits free-form output
(reasoning block + JSON), the runtime parses the JSON out of the
response and validates against `RecipeAuthoringOutput`. Cost:
loses xAI's enforcement of the schema (the LLM could emit
malformed JSON, and we catch it only at parse time, with worse
error shapes for the operator). Benefit: the LLM has a real
channel for stepwise reasoning, and the empirical claim that
this improves quality is testable.

**Path 2 — Two-pass call.** First call: free-form, model emits
reasoning. Second call: structured-output mode, model emits the
JSON conditioned on its own prior reasoning (passed in as
context). Cost: doubles LLM cost on the most expensive tier
(Workhorse), and adds a second xAI round-trip per `author_recipe`
call — a non-trivial wall-clock cost given each Workhorse call is
already ~10–25s. Benefit: keeps xAI's structured-output
enforcement on the second pass, while still giving the model a
reasoning channel.

The empirical question: does either path actually move the
needle on container-selector / cross-mode / missing-required-
field failures, or does the model produce perfunctory reasoning
that doesn't change its eventual output? Worth a small A/B test
before committing.

This is independent of the Decision-tree restructure (Deferred
B) — Path 1 or 2 could plug into either v1.x or v2.0 prompts.

## Patch 3E (target-vs-nomination routing) — still open as ADR conversation

Restated from the Session 54 handoff because it remains relevant
and neither outside review addressed it: some of the lithium run's
declines aren't model failures — they're correct decisions on
misrouted targets. USGS MCS structurally doesn't carry
`refining_capacity` or `spot_price`; the recipe author honestly
declined those targets, and each decline cost one Workhorse call.
No prompt edit will recover those calls — the fix is upstream at
the L1 classifier (per-nomination target whitelisting), and it's
a schema-migration-class change that wants an ADR before code.

Two architectural directions worth discussing:

1. **Per-nomination target whitelisting.** The L1 classifier
   emits, alongside the source nomination, the set of record
   buckets this source plausibly serves. The executor skips
   targets outside the whitelist. Saves Workhorse calls; pushes
   routing to the L1 (where ADR 0007 says it belongs).
2. **Per-target descriptor on the nomination.** Symmetric to (1)
   but inverted: each nomination carries an explicit `targets:
   [observation_metric, event_type, ...]` set. The executor
   iterates only those.

Both have schema migration implications
(`DocumentSourceNomination` shape change). Both want an ADR
before code.

## The discipline this handoff is structured around (carried from Session 54)

- **Each commit is a reset target.** Patch 3, Stage 1, Stage 2,
  the normalizer fix, and Patch 4 each landed as their own
  commit. If Patch 4's live-test reveals a regression on (1)/(2)/
  (3) above, the operator can `git reset --hard <pre-Patch-4
  head>` and Patch 4 reverts cleanly without affecting the
  parallelism work.
- **Live observation only at the end of the work.** The Session
  55 instruction was "no stops for lithium observation; only at
  the end after parallelism is delivered." Patch 4 follows the
  same shape: ship and verify in one live-test, not after each
  sub-piece.
- **Reset is a first-class option for prompt edits too.** v1.18 →
  v1.17 is one file revert. The empirical question for Patch 4
  verification is whether the model's behaviour shifts; if it
  doesn't, reverting is cheap.

End of handoff.
