# Session 53 — Handoff

The Session 51 patch landed propose-URL prompt v1.0 → v1.1 (news-
first override on the basis of the 2026-05-09 first-run evidence).
The Session 52 patch landed UI polish: per-nomination outcome
glyph + expandable cross-run chronology in the Document bucket.

The 2026-05-09 18:12–18:24 lithium re-run is the v1.1 verification.
Run id `019e0df0-ba52-7eb1-a6c1-f67bfb366b24`, 7 nominations
attempted, 2 recipes authored, **0 records produced** (both
authored recipes failed at apply-stage). v1.1 is reaching the
proposer — its language shows up verbatim in three decline
rationales — but is not changing behaviour on the cases the
override was designed for. Two new failure shapes surfaced. This
session is the design write-up for the v1.2 follow-up plus a
parallel recipe-author validation thread that the live-run
forced into scope.

## v1.1 verification — what the override did and didn't do

### Where v1.1 fired (operator-visible in decline rationales)

- **Australia / industry.gov.au (P1, auth-primary):** proposer
  declined with "no other known responsive non-SPA endpoints or
  trade-press articles with extractable mine-level lithium
  production/export/forecast tables from the RE Quarterly are
  available *without guessing*."
- **SEC EDGAR (P1, auth-primary):** proposer declined with "News/
  trade-press articles quoting the filings would not satisfy the
  nomination's requirement for primary authoritative
  disclosures."
- **Reuters (general_news):** proposer tried `feeds.reuters.com`
  (request failure) → `reuters.com/search?q=lithium` (401), then
  declined with "no other known server-rendered, fetchable
  Reuters endpoint matches the nomination's events focus."

In all three the proposer *considered* the v1.1 pivot and
*declined to take it*. The prompt language is reaching the
sample; the decision is reasoning its way past it.

### Where v1.1 didn't fire

- **IEA (P1):** two attempts on iea.org overview pages
  (`global-ev-outlook-2024`, `global-critical-minerals-outlook-2024`).
  No host pivot. v1.1 said "fetchability beats pedigree"; the
  overviews fetched but didn't author. The proposer treated
  "authored = fetchable" rather than "authored useful bytes =
  fetchable."
- **Fastmarkets (industry_trade_press):** declined on first
  attempt with paywall reasoning. No alternate source class
  considered.
- **USGS MCS (P1):** proposer stuck on the auth-primary PDF for
  all 4 expectations. 1 of 4 authored (reserves), 3 declined
  for fitness reasons.

### Two failure modes the override didn't anticipate

1. **The proposer's "don't guess" discipline defeats
   "fetchability beats pedigree."** Australia is the smoking gun
   — the proposer named the news pivot, then refused it on the
   grounds that picking a specific Bloomberg/Reuters URL would
   be "guessing." That collides with the "Discipline" section's
   `Synthetic guesses with no grounding ... Better to decline
   than to guess from rumor` rule, which v1.0 wrote and v1.1
   left untouched. v1.1 added permission to pivot but didn't
   amend the discipline rule that licenses decline-rather-than-
   pivot.

2. **News surfaces have paywalls too.** Reuters returned 401 on
   `reuters.com/search?q=lithium`. v1.1's 401 / 403 guidance
   ("pivot off the host: propose a news or trade-press article
   that covers the same metric and cites this host as its
   primary source") assumes the 401 came from an auth-primary
   host. It has no language for the secondary case: news host
   itself blocks → pivot to a *different* news host.

### Apply-stage regressions (parallel thread)

Both authored recipes failed at apply-stage:

- `pubs.usgs.gov` (`reserves`, `019e0df2-9af1`): `content
  assembly failed: observation content: invalid type: string
  "Argentina", expected f64`. Recipe-author selected a country-
  name column into the numeric `value` field.
- `www.worldbank.org` (`spot_price`, `019e0df5-083d`): `content
  assembly failed: observation content: missing field
  'value'`. Recipe-author authored a selector that returned
  nothing or the wrong shape.

Same class of bug: **the recipe-author authors selectors that
type-check syntactically but don't shape-match the bytes**. The
2026-05-09 first run hit the same shape on
`www.iea.org` (`"Global EV Outlook 2024"` string into f64).
Three observations across two runs is enough — this is
recurrent, not a one-off.

## Pieces for Session 53

Six pieces, listed roughly in design-priority order. A and B are
the load-bearing additions the live run forced into scope; C and
D extend the same self-healing-loop logic; E is operator-flagged
UI polish from the Session 52 conversation; F is a new feedback
loop the post-consolidation reasoning_effort plumbing makes cheap
to add. Next session can ship all six as one commit (their wire
shapes don't conflict) or split A+B+C+D as the prompt-and-author
bundle and E+F as a separate UI-and-orchestration bundle.

### Piece A — propose-URL prompt v1.1 → v1.2

Three additive amendments that close the gaps the live run
surfaced. v1.0/v1.1 vocabulary stays verbatim. Patch shape: minor
version bump, additive.

**1. The "reasonable shot" principle (closes the don't-guess
gap).** Add a new paragraph under "How to weight `priority_tier`":

> "Don't guess" forbids fabricating *parameters* on
> auth-primary endpoints (`?from=2024&to=2025` when you don't
> know the API accepts those params) and *paths* on hosts whose
> routing you don't understand. It does **not** forbid
> proposing a publisher's known coverage URL when prior
> attempts on the auth-primary host have exhausted. A topic-
> tag listing on a major coverage publisher (a commodities
> desk's lithium tag, a financial portal's commodity-prices
> page) is a *reasonable shot* — the publisher covers this
> metric class in their normal editorial flow, and the worst
> case is a 404 that costs one attempt. A wrong URL on a known
> coverage host costs the same as a decline, but the right URL
> returns a record. When prior attempts have exhausted the
> auth-primary host, taking a reasonable shot at a coverage
> publisher's tag/topic/listing URL is preferred over decline.

This phrasing is principle-only (no host names) and cleanly
distinguishes the two kinds of "guess" the proposer is
conflating: parameter fabrication on opaque endpoints (correctly
forbidden) vs reasonable inference about which publishers cover
which metric classes (the override wants).

**2. Two-step host pivot (closes the news-paywalls-too gap).**
Amend the `403/401` and `timeout`/`5xx` bullets in "Reading
prior attempts":

> When the prior-attempts log shows the *news/trade-press host
> itself* returning 401/403/timeout (e.g., a paywalled search
> endpoint, a SPA-only news front-page, a CDN block), the same
> "pivot off the host" rule applies recursively: propose an
> article on a different news/trade-press publisher covering
> the same metric. Do not retry the same blocked publisher.
> Two-step pivots are still pivots — the binding constraint
> remains fetchability + concision.

**3. "Authored bytes ≠ useful bytes" (closes the IEA / overview-
landing-page gap).** Amend the `recipe author declined: no
extractable structure` bullet:

> When prior attempts on the same host produced an
> `extractable structure` decline on an overview / landing /
> hub page, the host's flagship document was not selected
> precisely. The pivot is not necessarily off-host; first try
> a focused publication on the same host (a single-chapter
> PDF, a press release, a fact sheet, a data-explorer export
> URL) targeted at the *specific metric* the description
> names. If a focused on-host surface is not known, then
> pivot off the host using the news/trade-press rules above.

These three amendments are all extensions of v1.1's framework —
they teach the proposer how to apply the override in the
specific shapes the live run revealed it isn't applying it. No
host names, no scheme matchers; principle-only.

The xAI Workhorse-tier model (grok-4.3, Medium reasoning_effort
per the post-consolidation memory) is the proposer's tier today.
The v1.2 amendments add ~60 lines to the prompt; the prompt is
already ~290 lines so a 20% lift is fine within the cheap-tier
context budget.

### Piece B — recipe-author author-time shape validation

The two apply-stage failures share a class: **the recipe-author
authors selectors that type-check the wire schema but don't
shape-match the bytes**. The fix is recipe-author-side, not
apply-side: the author should validate, before returning a
recipe, that running its selector against the prefetched bytes
produces values matching the target field's expected type.

Concretely, for an `ObservationMetric` target where the binding
spec calls for an f64 `value`:

1. Recipe-author returns a draft recipe (selector spec).
2. **New step**: a thin in-process validator runs the draft's
   `extract` block against the prefetched bytes and inspects
   the resulting JSON for shape/type compatibility.
3. If the validator finds the selector returns strings where
   f64 was expected (`"Argentina"`, `"Global EV Outlook 2024"`,
   `"74,700"`), or returns no `value` field at all, the recipe-
   author surfaces the validation failure as a per-target
   decline (`AuthoringError::Declined`) instead of returning a
   broken recipe.
4. The retry loop sees the decline at L2-author time and
   either pivots to the next URL or surfaces a per-expectation
   `RecipeOutcome::Declined` — same wire shape v1.1 already
   surfaces.

This moves the failure from apply-stage to author-stage, which
matters because:

- **Apply-stage failures consume a fetch attempt** and waste the
  workstation's per-source deadline budget on a recipe that was
  doomed before it started.
- **Author-time failures feed the prior-attempts log** with a
  rich rationale ("selector returned string for numeric target")
  the next propose-URL iteration can read and pivot on.
- **The validator is a strict subset of the apply-stage logic**
  (it runs the same `extract` against the same bytes; the
  difference is *when*). Reusing the existing apply-stage
  pipeline as a validator is mechanical refactoring, not new
  code.

The validation is **shape-only**, not content-validation: it
catches "string in numeric slot" and "missing required field,"
not "the value is wrong." Content correctness is the recipe-
apply layer's job; shape correctness is the recipe-author's
contract.

A specific case the validator must handle: the USGS MCS
"comma-formatted numbers" decline (`74,700`). The current
recipe-author *already* declines this on its own f64-coercion
read, but inconsistently — same prompt, same bytes, different
decisions across runs. An explicit shape validator would
either:

- Run a pre-author normalizer (strip commas, parse f64) and
  succeed where today's recipe-author refuses, OR
- Surface the comma-format as a known shape-mismatch the
  proposer's prior-attempts log can react to.

Either path is better than the current stochastic decline.

### Piece C — apply-stage failures fed into prior-attempts

Today's `RecipeOutcome::Failed { stage: Apply, message }` rows
surface to the operator (FetchReport, Bucket chronology) but
**don't flow back into the propose-URL prior-attempts log**.
Run N+1's proposer for the same nomination sees only fetch-stage
and author-stage entries; an apply-stage shape failure on run N
is invisible to the next iteration's pivot logic.

Until Piece B's author-time validator catches every shape bug
(it won't — selector behaviour against unseen bytes is
unbounded), apply-stage failures will keep happening. The
proposer needs to see them as prior-attempts so it can pivot.

The fix is wire-format-only on the proposer-side input
composition. Where today's `format_prefetch_failure_for_proposer`
emits:

```
fetch failed: 403
fetch failed: timeout after 60s
recipe author declined: no extractable structure
```

…run N+1 also gets:

```
recipe authored but apply failed: <stage> · <message head>
```

The `<message head>` is the apply-stage error truncated to ~120
chars (full string in the chronology hover already). The
proposer's pivot heuristic for this shape is the same as
`recipe author declined: no extractable structure`: try a
different URL on the same host or pivot off-host.

The v1.2 prompt's "Reading prior attempts" section names the
new shape with one bullet:

> `recipe authored but apply failed: <stage> · ...` — the
> recipe-author committed a selector that ran successfully
> against the prefetched bytes but produced values whose
> shape failed at apply (string in a numeric slot, missing
> required field, etc.). The URL is fetchable but the
> chosen path's data shape doesn't match the closed
> extraction modes for this target. Pivot to a different
> path on the same host (or off-host using the news rules)
> rather than retrying the same path.

Wire-shape impact: extends the existing prior-attempts string
concatenation in
`crates/pipeline/src/propose_source_url.rs::build_prompt`. No
new DTO, no schema change, no migration. The
`fetch_run_outcomes` table already carries apply-stage failure
messages (Session 46); this piece reads them at proposer-input
composition time.

### Piece D — numeric-format normalizer in recipe-apply

The USGS MCS decline shape ("comma-formatted numbers (e.g.
74,700) and estimate prefixes that prevent clean numeric
extraction to f64 via pdf_table") is the recipe-author's
interpretation of a real apply-stage limitation: today's f64
coercion is strict and rejects `"74,700"`, `"e74,700"`
(estimate marker), `"$1,234.56"` (currency), `" 12.5 "`
(whitespace).

A bounded pre-coercion normalizer in
`crates/pipeline/src/recipe_apply.rs` (or wherever the
observation `value: f64` field is parsed) strips, in order:

1. ASCII whitespace.
2. Common estimate / approximation prefixes (`e ` /
   `e<digit>` / `~` / `≈` / `est.` followed by space). The
   `e` matcher must distinguish from scientific-notation
   exponents (`1.5e9`); only strip when followed by a
   space-then-digit, or when the candidate already contains
   no scientific-notation pattern.
3. Common currency prefixes / suffixes (`$`, `€`, `£`, `¥`,
   `USD`, `EUR`).
4. ASCII thousand separators (commas) when the comma sits
   between digit triplets and the candidate has no decimal
   period preceded by another comma (rejecting EU-locale
   `1.234,56` shapes — those need explicit locale-aware
   parsing which is out of scope for this piece).

After the strip, parse f64. If parseable, the apply-stage
succeeds and emits one info-level log line naming the
normalisation that fired (so the operator sees when it's
intervening). If unparseable, the apply-stage fails with the
original (un-normalised) string in the error message — the
operator sees the real shape, not a misleading post-strip
fragment.

The normalizer is **bounded**: it doesn't try locale detection
(EU `1.234` vs US `1.234`), doesn't attempt currency conversion,
doesn't infer units from prefixes. Edge cases stay in the
"apply fails honestly" path. The shape it does catch — US-locale
decimals + thousand separators + estimate prefixes + currency
markers — is the dominant case the live runs surfaced.

Tests pin the normalisations on a small fixture table:
`"74,700"` → `74700.0`, `"e1,200"` → `1200.0`, `"$1,234.56"`
→ `1234.56`, `"1.5e9"` → `1500000000.0` (preserves scientific
notation), `"1.234,56"` → parse failure (ambiguous — left
alone), `"abc"` → parse failure (genuinely non-numeric).

Wire impact: none. The change is internal to the apply path; the
output shape (`f64` value or apply-stage error) is unchanged.

### Piece E — UI polish: host-backoff strip + sticky plan header

Two deferred items from the Session 52 conversation, both
small enough that bundling them with a feedback-loop session
makes sense.

**E.1 — host-backoff strip.** The current strip renders
`HOST BACKOFF1 host · this session — fails: 2 wait: —`. The
`BACKOFF1` wedge is a typographic accident (missing space
between the label and the count); the `wait: —` row repeats
information the visual state already encodes (a blocked host
shows a `wait: <N>s` countdown; a recovering host with no
active backoff has nothing meaningful in `wait`). Polish:

- Fix spacing (`HOST BACKOFF · 1 host · this session`).
- Drop the `wait: —` token when there's nothing to wait on;
  keep it when an active backoff is counting down.
- Lift the recovering / blocked / blocked-cooldown semantic
  state into a small status dot (dim / amber / red) using the
  existing `--signal-warning` / `--signal-negative` tokens.
  Same dot vocabulary the FetchReport row borders use, so the
  glance reads consistently across surfaces.

CSS-only inside `HostBackoffStatus.svelte`; no prop changes.

**E.2 — sticky plan header.** Below the bucket grid the panels
stack tall (FetchReport + RecipeOutcomesHeatmap +
ExpectationCoverage + RecipesPanel + SourcesMemoryPanel). The
plan's topic, accept-reject buttons, and run-fetch button
scroll out of view, so the operator loses orientation while
reading the bottom of the page. Add `position: sticky; top: 0`
on `<header class="head">` in `PlanReview.svelte`, with a
small `background: var(--bg-panel)` and a subtle bottom border
so the sticky header reads as a separate strip rather than
overlapping content.

CSS-only inside `PlanReview.svelte`; no prop changes, no
behavior changes.

### Piece F — reasoning_effort escalation for stuck nominations

The post-Session-50 memory notes that xAI consolidated to
grok-4.3 on May 15 2026; cost-tier differentiation now flows
through `reasoning_effort` (Low/Medium/High) instead of
distinct model strings. The propose-URL call uses cheap-tier
(Low effort) by default. After a nomination has been declined
≥3 times across runs (or ≥2 declines plus the most-recent run
also declining), the next run's propose-URL call **for that
specific nomination** escalates to workhorse (Medium effort).

The escalation is per-nomination, not per-plan or per-host: a
stuck nomination gets more thinking; the rest of the plan
stays cheap. The signal source is `plans.outcomesHistory` (or
the same query the heatmap reads) — the workstation counts
declines for this nomination's source_id prefix and decides
the effort string at propose-call time.

Composition: `xai::ProviderConfig::cheap_effort` becomes
overridable per-call. The fetch_executor's `propose_url`
call site computes the effort from the nomination's prior
decline count and passes it as a per-call argument. The
provider's `complete` method (or its existing per-tier
selection wrapper) honours the override.

This is the feedback loop the system has been missing: today
every nomination gets the same model effort across runs
regardless of how stuck it is. After F, the workstation
invests more reasoning where it's needed and stays cheap
where it isn't. Doesn't fix the underlying prompt issues
(A does that); does compound with A by giving the harder
prompt a bigger thinking budget on exactly the cases that
need it.

The escalation ceiling stops at workhorse (Medium) for now.
Frontier (High) is reserved for deliberate operator-driven
re-runs (a future "re-fetch with more thinking" affordance);
adding it to the automatic ladder risks budget surprises.

Wire impact: a new internal config knob on the LLM provider
(per-call effort override). No new DTO. The escalation
decision is observable in the existing run logs (one line
per call naming the effort string). A small UI affordance —
showing the effort tier the propose-URL call used in the
chronology entry — is a future polish, not part of this
piece.

## Why these pieces share a session

All six surface in the same operator workflow — running a fetch
on a real plan and reading what came back — and the four
backend pieces (A, B, C, D) feed the same prior-attempts loop
from different angles:

- **Piece A (v1.2 prompt)** widens what the proposer can *do*
  with the prior-attempts signal (read shape-failure shapes,
  propose two-step host pivots, take reasonable shots at
  coverage publishers).
- **Piece B (author-time validator)** widens what the prior-
  attempts log *contains* upstream (move shape failures from
  apply-stage to author-stage so the next iteration's proposer
  sees them earlier and on more cases).
- **Piece C (apply-failure feedback)** widens what the prior-
  attempts log *contains* downstream (until B catches every
  shape bug, the apply failures it misses still need to be
  visible to run N+1's proposer).
- **Piece D (numeric-format normalizer)** removes a class of
  apply-stage failures entirely by accepting the formatting
  the recipe-author reasonably authored — closing the
  ironic case where v1.2 teaches the proposer to pivot on a
  failure shape (B/C surface) that D removes from the failure
  set.

Together: the proposer emits more diverse URL shapes, the
recipe-author validates earlier, the apply path accepts more
of the shapes recipe-authors reasonably produce, and the
prior-attempts log contains richer signal for the next
iteration to pivot on.

**Pieces E and F are independent:**

- **Piece E (UI polish)** is a 60-line CSS-only commit on two
  components. It rides along because it's small enough to not
  warrant its own session and the operator already flagged
  both items in the Session 52 conversation.
- **Piece F (reasoning_effort escalation)** is the feedback
  loop the May-15 xAI consolidation made cheap. It's
  orthogonal to A/B/C/D in design but compounds with them at
  runtime — the v1.2 prompt at Medium effort produces better
  pivots than v1.2 at Low effort, on exactly the stuck
  nominations where the run-history signals "this one needs
  more thinking."

This matches the Session 47/49/51 cadence — one design thread
per session, multiple pieces under one observation. The 2026-
05-09 18:12 verification run is the observation; Pieces A–F
are the response.

## What's intentionally not in this patch

- **L1 prompt edits.** The L1 emits four priority tiers
  including `general_news` and `industry_trade_press`; the
  lithium plan's L1 already nominated Reuters and Fastmarkets.
  The bottleneck is not L1 emission, it's L2 ranking. Hold
  the L1 edit.
- **A specific publisher allow-list / preference table.** The
  closed-vocabulary discipline rule forbids it. The "reasonable
  shot" principle is host-class language; the LLM applies it
  against its general-knowledge model of which publishers cover
  which metric classes. No domain strings in the prompt.
- **Locale-aware numeric parsing in Piece D.** Piece D handles
  the US-locale shapes the live runs surfaced (commas as
  thousand separators, periods as decimals). EU-locale shapes
  (`1.234,56`) need explicit per-source or per-plan locale
  detection — left in the "apply fails honestly" path until
  a live run demands the lift. Adding heuristic detection
  would silently mis-parse legitimate values; explicit is
  better than guessing.
- **Frontier-tier (High effort) escalation in Piece F.** The
  automatic ladder stops at workhorse (Medium). Frontier is
  reserved for deliberate operator-driven re-runs because
  each frontier call is materially more expensive and
  automatic escalation to it risks budget surprises on a
  plan with many stuck nominations.
- **An effort-tier UI badge on the chronology entries.**
  Showing which effort the propose-URL call used per attempt
  is a future polish for Piece F; the data is in run logs and
  could be surfaced once the loop's effect is observed. Not
  scope for the first F commit.
- **Per-attempt wire surface for intra-run URL chronology.**
  Today's surface piggybacks on the decline `message` string;
  the Session 52 chronology component renders it inline. A
  first-class wire surface for intra-run attempts would require
  storage + DTO + IPC; out of scope unless the live-run
  signals it.
- **Promotion pipeline (ADR 0004), Iterator Phase 2 (ADR 0016),
  charts on Observations / Events.** Same posture as Sessions
  47–52.
- **xAI Responses API migration.** Same posture.
- **Per-bucket-type outcome glyph extension to Observation /
  Event / Entity / Relation / Assertion.** The Session 52 patch
  notes this needs the Session 23 expectation-binding shape;
  defer.
- **Live per-nomination state during a fetch (the deferred
  Tauri event channel from Session 50).** Same posture. Piece
  F's effort-tier choice is recorded in the post-run history;
  the live-channel surface for in-flight effort visibility is
  the same architectural follow-up.
- **Sources-memory feedback into the proposer.** The classifier
  reads `{{SOURCES_MEMORY}}` (Session 48); the proposer doesn't
  yet. Threading sources-memory into the propose-URL prompt
  would let the proposer see "this plan's prior runs succeeded
  on these hosts" — a known-good-host signal. Out of scope for
  Session 53 because A/B/C/D already widen the prior-attempts
  signal substantially; layer sources-memory feedback in only
  if the v1.2 + author-validator combination still leaves the
  proposer reaching for unknown hosts.

## Live-run evidence summary

Files of interest the operator should keep with this handoff
when reviewing it next session:

- The 2026-05-09 18:11 desktop log (the timestamped log
  pasted in the conversation that produced run
  `019e0df0-ba52-7eb1-a6c1-f67bfb366b24`).
- The three review-pane screenshots taken during/after the
  run (recipe history strip, last-run detail view, expectation
  coverage matrix).

Run-level summary (from the pasted log):

| Nomination | L1 tier | Outcome | Notes |
|---|---|---|---|
| USGS MCS lithium chapter | P1 | 1 recipe authored, 3 expectation declines | Authored recipe failed at apply (`string "Argentina"` → f64) |
| SEC EDGAR filings | P1 | Nomination declined after 2× 403 | Proposer refused news pivot citing description language |
| World Bank Pink Sheet | P1 | 1 recipe authored, 3 expectation declines | Authored recipe failed at apply (`missing field 'value'`) |
| IEA Critical Minerals / EV Outlook | P1 | Nomination declined after 2 attempts | Both attempts on iea.org overview pages |
| Australia REQ | P1 | Nomination declined after 2× 60s timeouts | Proposer named news pivot, refused on "no guess" grounds |
| Fastmarkets BMI | TP | Nomination declined on first attempt | Paywall reasoning, no source-class pivot |
| Reuters commodities | GN | Nomination declined after RSS-fail + 401-search | Two-step pivot off Reuters not attempted |

Coverage matrix (post-run, from Expectation Coverage panel):
**2 of 11 expectations covered** (`reserves` via pubs.usgs.gov,
`spot_price` via www.worldbank.org). **0 records on the
workstation** because both covering recipes failed at apply.

Host-backoff strip: `www.industry.gov.au RECOVERING — fails: 2
wait: —`. Session 50 piece B/C continues to work as designed
across sessions (was `fails: 1` after the first run; now `fails:
2` after the second run on the same host).

## Hard rules carried over

Same as Sessions 41–52:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. Pieces A–D add none. Piece
  B adds an author-time validator step (not a new mode); Piece
  D adds a numeric-format normalizer in apply (not a new mode).
- ADR 0009: every HTTP call goes through `SecureHttpClient`.
  Piece B's validator runs against already-prefetched bytes;
  Piece D operates on already-extracted strings; neither adds
  an HTTP path.
- Bounds checking on every IPC string input. No new IPC
  commands across A–F. Piece F's per-call effort override is
  internal to the LLM-provider trait.
- Tauri commands return `CommandError`. No new Tauri commands.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; this session adds none — A/B/C/D reuse existing DTOs;
  E is CSS-only on existing components; F's effort-tier choice
  surfaces only in run logs (no DTO yet).
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. Piece E uses
  only existing tokens (`--bg-panel`, `--border-subtle`,
  `--signal-warning`, `--signal-negative`, `--fg-tertiary`); no
  hex literals.
- Runes-using files end in `.svelte.ts`. Piece E adds no new
  runes; CSS-only edits on `HostBackoffStatus.svelte` and
  `PlanReview.svelte`.
- L1 prompt edits come from observed classifications, not
  speculation. **None this session — L1 emission is correct;
  the bottleneck is L2 ranking.**
- L2 prompt edits come from observed authoring failures, not
  speculation. **Piece A edits the propose-URL L2 prompt v1.1
  → v1.2 on the basis of the 2026-05-09 18:12 verification
  run: three concrete failure shapes (don't-guess defeats
  pivot, news-host paywalls, overview-page authoring
  succeeds-but-doesn't-extract) the override didn't anticipate.
  Piece A also adds the new `recipe authored but apply failed:
  ...` prior-attempts shape Piece C emits.**
- L2 prompt edits come from observed authoring failures —
  Piece B edits the recipe-author at the *code* layer (a new
  validator step), not the prompt. The recipe-author prompt
  v1.15 stands.
- **Stockpile prompts: principle-only language.** The v1.2
  amendments name source classes and observable properties.
  No host name, no scheme matcher, no domain string in either
  the propose-URL prompt or the apply-stage normalizer's
  rules.
- **Do not write code to pass tests.** Piece B's validator
  tests pin its observable contract (string-in-numeric →
  decline, missing-required-field → decline); Piece D's
  normalizer tests pin a small fixture table of normalisations
  against known shapes (commas, currency markers, estimate
  prefixes). Both test surfaces are shaped by the design, not
  the other way around.
- **Closed-vocabulary discipline.** A's amendments name source
  classes and observable properties. B's validator applies
  uniformly across nominations. C's prior-attempts shape is
  emitted uniformly across all apply-stage failure stages.
  D's normalizer applies uniformly to any `f64` apply target,
  not per-source. E's UI changes are uniform across hosts. F's
  escalation logic counts declines per nomination uniformly,
  not per-host or per-publisher-class.

End of handoff.
