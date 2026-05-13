# Session 51 — Patch 1

The Session 50 patch landed three pieces (topic-aware PDF page
selection, a separate prefetch `SecureHttpClient`, and a
running-now scaffold). The 2026-05-09 live run on the lithium
plan produced two pieces of evidence Session 51 was eager for:

1. **Session 50 piece B/C is working as designed.** The host
   `www.industry.gov.au` (Australian Resources and Energy
   Quarterly) timed out once on its 60s prefetch ceiling and
   transitioned to `RECOVERING` in the host-backoff strip
   (`HOST BACKOFF1 host · this session — fails: 1 wait: —`). The
   shared `HostBackoff` state caught the signal exactly the way
   the pre-Session-50 single-client flow would have, and the
   tighter prefetch ceiling kept the failure inside the
   `PER_SOURCE_DEADLINE_SECS` (240s) budget instead of starving
   sibling nominations on the same plan. No code change required.
2. **The propose-URL retry loop declined a nomination because the
   only authoritative path it knew was unresponsive.** The decline
   reason on `nom:…c2c2ee539af0` reads *"The sole known
   authoritative path for the Australian Resources and Energy
   Quarterly (the index page) already timed out; no other verified
   non-SPA data endpoint, direct PDF, or RSS feed on industry.gov.au
   is known that would reliably serve the mine-level lithium
   production/export/forecast tables without fabricating a path."*
   That is exactly the case the operator's Session 51 override
   addresses: the proposer was trapped inside a single host-class
   ranking and had no language permitting a pivot to a news /
   trade-press article quoting the same figure.

This patch lands two pieces:

- **Piece A (UI):** the six bucket panels in `PlanReview` are
  height-capped with internal vertical scroll, so a tall bucket
  (Document with seven nominations, Observation once records
  populate, etc.) can no longer stretch its grid row and visually
  overflow into the panels below (`RecipeOutcomesHeatmap`,
  `SourcesMemoryPanel`, host-backoff strip).
- **Piece B (prompt):** the propose-URL prompt is bumped from v1.0
  to v1.1. The new sections reframe `priority_tier` as a hint
  about provenance class rather than a strict ranking, and add
  explicit guidance that responsive news / trade-press surfaces
  beat unresponsive authoritative flagship documents. The
  prior-attempts heuristic block expands to cover the timeout,
  too-large, and rate-limited shapes (which Session 49 already
  emits to the prompt's `PRIOR_ATTEMPTS` slot but the v1.0 prompt
  did not name).

The two pieces share a session because they surface in the same
operator workflow — running a fetch on a real plan and reviewing
what came back. Piece A makes the review panel scannable; piece B
makes the next run's nomination success rate higher on plans
whose authoritative endpoints are slow or blocking.

The override in piece B is operator-confirmed and stand-by:
"favouring news outlets sources against authorities is an
override which I standby and confirm" (handoff message,
2026-05-09). The patch keeps the discipline boundary intact: no
host names, no scheme matching, no domain allow/deny lists. The
prompt teaches the principle (fetchability + concision beat
pedigree); the LLM applies it.

## Apply

Files were edited in place. To verify (operator runs cargo on
Mac per the cargo-on-Mac workflow; sandbox can't reach
crates.io):

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

Both changes are content-only relative to the type system:
piece A is CSS inside an existing Svelte component (no Svelte
script changes, no prop changes), and piece B is a prompt file
that's `include_str!`'d at compile time (`apps/desktop/src-tauri/
src/main.rs:55`). The Session-49 prefetch-failure wire format the
prompt's prior-attempts block names (`fetch failed: 404`,
`fetch failed: timeout after …s`, etc.) is unchanged — Session 51
adds prompt-side guidance for shapes the executor was already
emitting verbatim. The `format_prefetch_failure_for_proposer`
tests at `crates/pipeline/src/fetch_executor.rs:8012+` still
pin the same exemplars; v1.1 is additive over v1.0.

No new dependencies. No schema change. No migration. No new IPC
commands. No new ts-rs DTOs. No new Svelte components. No new
Rust types.

## Files changed

### Frontend — bucket body height cap (Piece A)

- `apps/desktop/src/components/panels/Bucket.svelte` —
  - **Doc-block addition** at the top of the file names the
    Session 51 scope ("Body height cap") and the rationale: a
    tall bucket would stretch its CSS-grid row and visually
    overflow into the panels below. Names the closed-vocabulary
    discipline boundary the cap sits on (uniform across all six
    bucket types; no document-specific routing).
  - **`.body` rule additions** — `max-height: clamp(180px, 36vh,
    420px)` (responsive cap that scales with viewport so wide
    monitors get more density without losing the cap on small
    windows), `overflow-y: auto`, `scrollbar-gutter: stable`
    (reserves the gutter so a scrolling bucket's content doesn't
    horizontally shift relative to non-scrolling siblings),
    `scrollbar-width: thin` + `scrollbar-color` (Firefox), and
    `::-webkit-scrollbar`{,-thumb,-track} rules (Chromium /
    WebKit). All scrollbar styling uses existing design tokens
    (`var(--border-subtle)`); no hex literals.
  - The bucket header (title + count) sits outside `.body`, so
    it stays in view as the body scrolls. The empty-state copy
    ("no expectations for this type — by design") and the
    records section both live inside `.body` and scroll
    together, which keeps the per-bucket affordance honest:
    everything below the header is the bucket's content, scroll
    if you need to.

The cap is uniform across all six bucket types (Observation,
Event, Entity, Relation, Document, Assertion). Closed-vocabulary
discipline — no source-specific routing — and avoids a future
follow-up where another bucket type starts overflowing once its
records section populates densely.

### Prompt — propose-URL v1.1 (Piece B)

- `config/prompts/propose_source_url.md` —
  - **Header**: `# Propose Source URL Prompt — v1.0` →
    `# Propose Source URL Prompt — v1.1`.
  - **"What makes a good URL"** gains a binding-constraint
    paragraph: "fetchability" (workstation can retrieve bytes
    within per-source deadline) and "authorability" (bytes
    contain extractable structure) are the two constraints; a
    URL failing either is a wasted attempt regardless of
    publisher prestige.
  - **"Concrete heuristics by source type"** is reordered:
    News and trade press is now first (it was fourth). The
    Statistical agencies / Regulators bullets pick up explicit
    language about prefetch budgets and per-source deadlines —
    "long flagship reports often exceed prefetch budgets and
    fetch deadlines; prefer a focused publication on the same
    host…".
  - **New section "How to weight `priority_tier`"** (~30 lines)
    — the operator-confirmed override. Reframes the four
    priority tiers as a hint about provenance class, not a
    strict ranking. Practical consequences enumerated:
    - When a news / trade-press article plausibly covers the
      metric, propose that — even if L1 named
      `authoritative_primary`.
    - When L1's hint and a responsive news surface point at the
      same metric, prefer the responsive surface. Reserve
      authoritative endpoints for descriptions that require
      primary structure (full reserves tables, multi-year
      price series, legal text).
    - When no news/trade-press path is known, falling back to
      the authoritative endpoint is reasonable on the first
      attempt; treat the prior-attempts log as the strongest
      signal on the second and third.
    - Closing principle: "the workstation's per-source
      deadline is the binding constraint, not the publisher's
      prestige."
  - **"Reading prior attempts"** expands. The v1.0 list named
    only `404`, `403/401`, and three recipe-author decline
    shapes. v1.1 adds:
    - `fetch failed: timeout after …s` / `fetch failed: 5xx` —
      same-host pivot to a responsive news surface.
    - `fetch failed: response too large (got at least N bytes,
      max M)` — pivot to a focused surface (press release,
      single-chapter PDF, news article quoting the figure).
    - `rate-limited; …` — pivot off the host.
  - The `403/401` bullet now explicitly directs the proposer
    to "pivot off the host: propose a news or trade-press
    article that covers the same metric and cites this host
    as its primary source" — replacing the v1.0 "consider
    declining" softer phrasing.

The wire format the prompt's prior-attempts block names
(`fetch failed: <code>`, `fetch failed: timeout after Ns`,
`fetch failed: response too large (got at least N bytes, max M)`,
`rate-limited; retry after Ns`) is exactly what
`format_prefetch_failure_for_proposer` already emits at
`crates/pipeline/src/fetch_executor.rs:1900+`. No code change
needed; the v1.0 wire shape is preserved across the version bump.

The prompt is loaded via `include_str!` at compile time
(`apps/desktop/src-tauri/src/main.rs:55`), so the version bump
ships with the next workspace build.

## Design notes worth preserving

### Why the bucket height cap is on `.body`, not `.bucket`

The bucket header (title + count) is the primary scan affordance
when an operator surveys the six panels — it tells them at a
glance how many expectations / records each type has. If the cap
sat on the bucket itself, scrolling a tall bucket would scroll
its header out of view and the operator would lose the count
context while reading rows. Capping `.body` keeps the header
fixed and makes the rows the only thing that scrolls — same
information density, no loss of context.

### Why a responsive-viewport clamp instead of a fixed pixel cap

The six-bucket grid is responsive (3 cols → 2 cols → 1 col on
narrow windows; see `PlanReview.svelte:728+`). A fixed cap
sized for the 3-col layout would feel cramped on a 1-col
narrow window where the bucket has the full width to itself,
and conversely a cap sized for 1-col would be too tall on a
3-col wide-monitor layout where six buckets need to fit
without dominating the screen. `clamp(180px, 36vh, 420px)`
gives the cap a viewport-relative middle ground with floors
and ceilings that match the design's existing density
expectations.

### Why all six buckets, not just Document

The 2026-05-09 screenshot showed Document overflowing because
the live lithium plan happens to have seven nominations with
long descriptions. But the same overflow will appear on
Observation once a plan with many metrics populates its records
section, on Entity for plans with many companies + mines, etc.
A Document-only fix would land us back in this same patch in
two sessions. The uniform cap on `.body` is principle-only
(closed-vocabulary discipline: no per-bucket-type routing),
costs zero behavioural difference for buckets that fit under
the cap (no scrollbar appears), and inoculates against the
follow-up.

### Why the override is in the prompt, not in scoring or routing code

The closed-vocabulary discipline rule's "never bake source-
specific routing in code, prompt, or fixtures; teach principles,
validate outputs" reads to forbid both code-side host
allow/deny lists *and* prompts that name specific publishers.
The Session 51 override does neither: the new prompt language
talks about *classes* of source (news / trade-press,
authoritative flagships, statistical-agency endpoints) and
*observable properties* of fetches (fetchability, prefetch
budget, per-source deadline). No host name appears in the
prompt; no host name appears in fetch_executor; the only
host strings are in the operator-facing UI / logs (already
network-layer truth, already exempt). The LLM applies the
principle against its general-knowledge model of which
publishers cover which metrics — exactly the substitutability
the discipline rule is meant to preserve.

### Why fetchability beats pedigree in this specific override

The empirical case the operator surfaced: across the lithium
plan's 11 expectation rows, the auth-primary endpoints
declined or failed to author on USGS MCS PDF (no refining
capacity / no spot price), SEC EDGAR (`403`), IEA
(no extractable structure / type-coercion failure), and
gov.au (timeout). Two records succeeded — both from
`pubs.usgs.gov` on the production / reserves expectations.
The remaining nine expectations went uncovered.

Counterfactually, a news / trade-press surface (a Reuters or
Bloomberg or trade-press piece) covering the same metric
would have produced *some* record with a clear citation back
to the primary source, and the recipe-author's
`authored_from` provenance string would carry the article's
URL while the article's body cites the agency. The operator
gets a record; the workstation gets to keep iterating; the
agency's primary structure remains available for nominations
whose descriptions specifically require it.

The principle the override teaches: a returned record from a
news article beats a declined nomination from an unreachable
agency every time, *and* primary-source pedigree is
reconstructable from the article's citations during recipe
authoring.

### Why the prompt version bumps to v1.1 and not v2.0

The wire format the prompt's prior-attempts block names is
unchanged (Session 49's `format_prefetch_failure_for_proposer`
output). The output schema (`{ url, rationale }` or empty-url
decline) is unchanged. Existing v1.0 instructions remain
verbatim — "What makes a good URL", "What NOT to propose",
"Discipline" all stand. v1.1 *adds* sections and *expands*
heuristics. Patch shape: minor version bump.

The fetch_executor doc-comments naming "propose-URL prompt
v1.0" stay as historical references — they pin the wire format
the v1.0 prompt established, and v1.1 preserves it. Future
readers tracing the breadcrumb to `config/prompts/
propose_source_url.md` land on the latest version with all
v1.0 vocabulary still present.

### Why no test pinning the new prompt language

The patch's only Rust-side touchpoint (the prior-attempts wire
format) is already pinned by the Session 49 tests at
`crates/pipeline/src/fetch_executor.rs:8012+`. The new prompt
language is in scope for the LLM, not the executor — its
effect is observed in *which URLs* the proposer commits to on
plans where authoritative endpoints decline, not in any
shape-level change the test surface can pin. The right
validation is the next live run on a plan whose auth-primary
endpoints decline; the recipe-history strip and per-nomination
outcome rows surface what the proposer chose. Pinning the
prompt's prose against a fixture would only verify the file
hasn't been edited — duplicates `git diff`.

## Test deltas

- `apps/desktop/src/components/panels/Bucket.svelte` — no test
  deltas. The change is CSS-only inside an existing component;
  the existing svelte-check / type-check run will catch any
  malformed selector or rule. Visual verification by re-opening
  the lithium plan in the workstation: the Document panel now
  shows a slim scrollbar on the body when its seven nominations
  push past the cap; the panels below (`RecipeOutcomesHeatmap`,
  `ExpectationCoverage`, `SourcesMemoryPanel`,
  `HostBackoffStatus`) align cleanly to the row above instead
  of having Document spill into them.
- `config/prompts/propose_source_url.md` — no test deltas. The
  wire format the prompt's prior-attempts block names is
  unchanged from v1.0; the Session 49 wire-shape tests
  (`prefetch_failure_classifies_status_codes_session_49` et al)
  still pin the executor's emit. The prompt-level guidance is
  validated by live runs.
- `crates/pipeline/src/fetch_executor.rs` — no test deltas. The
  executor's output is unchanged.

Pipeline test count: unchanged. API test count: unchanged. Other
crates' counts unchanged. All ignored tests (12) remain the
existing `#[ignore]` live integration tests.

## Live-run evidence (Session 50 verification)

The 2026-05-09 lithium plan run validates Session 50 piece B/C
landed correctly:

- `HOST BACKOFF1 host · this session — www.industry.gov.au
  RECOVERING — fails: 1 wait: —` confirms the prefetch client's
  60s ceiling caught the slow Australian endpoint, the shared
  `HostBackoff` state recorded the failure, and the host
  transitioned to `RECOVERING` rather than starving the
  per-source deadline budget for the rest of the plan's
  nominations.
- The propose-URL retry loop declined `nom:…c2c2ee539af0` after
  one timeout attempt with a rationale that names the failure
  shape (timeout) and the lack of alternative paths the proposer
  knew. That decline is exactly the input shape the Session 51
  override is teaching the proposer to handle differently — the
  next run on a similar nomination should pivot to a news /
  trade-press surface that covers Australian lithium production
  rather than declining on the agency's unreachability.
- Two records authored successfully (`pubs.usgs.gov` on the
  production and reserves expectations); one apply-stage failure
  on `www.iea.org` with a content-coercion error
  ("invalid type: string `Global EV Outlook 2024`, expected f64")
  that is its own design thread (recipe-author authored a string
  in a numeric content slot — the type-coercion happens at
  recipe-apply, not recipe-author, so the failure is delayed).
  Out of scope for Session 51; flag for a future session if
  recurrence is observed.

## What's intentionally not in this patch

- **Per-bucket-type-specific layout tweaks.** The uniform `.body`
  cap is the principle-only fix. If a future operator decision
  surfaces a need for, say, a denser Document panel that shows
  more rows before scrolling than the other buckets do, the
  lever is one rule on `.bucket[data-type="document"]` — but
  pre-emptively differentiating would be source-specific routing
  in CSS, the same shape the discipline rule forbids elsewhere.
  Document; defer.
- **Live IEA `f64` content-coercion bug.** The 2026-05-09 run
  surfaced an apply-stage failure on a recipe that authored a
  string into a numeric content slot. The fix is in the recipe-
  apply / recipe-author path (validation at authoring time, not
  apply time), and the right shape is its own session. The
  Session 51 prompt override is independent of this bug —
  pivoting to a news source for the same metric avoids the IEA
  page entirely; the bug stays for the cases where IEA *is* the
  right surface.
- **xAI Responses API migration.** Same posture as Sessions
  47–50.
- **Promotion pipeline (ADR 0004), Iterator Phase 2 (ADR 0016),
  charts on Observations / Events.** Same posture.
- **Per-nomination live state (Session 50 thread).** The
  cheap-path scaffold from Session 50 stands; the Tauri event
  channel for live per-nomination state is still its own
  session.
- **Recipe-author prompt edit to mirror the override.** The
  recipe-author prompt v1.15 stands. The propose-URL prompt
  picks the URL; the recipe-author prompt writes coordinates
  against whatever bytes come back. The override is properly
  located at the URL-discovery stage; the recipe-author has no
  decision to make about source class (it sees bytes, it
  authors or declines).
- **Vocabulary-driven page selection bumped above 64 KiB
  ceiling.** Session 50 piece A's relevance-aware selection
  is the right shape; if the next live run shows the budget is
  still binding under topic-aware selection, the lift is
  parametric (raise `prefetch_excerpt_budget`), not structural.

## Hard rules carried over

Same as Sessions 41–50:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. This patch adds none.
- ADR 0009: every HTTP call goes through `SecureHttpClient`.
  This patch adds none — the Session 50 prefetch-client split
  stands.
- Bounds checking on every IPC string input. This patch adds
  no IPC commands.
- Tauri commands return `CommandError`. This patch adds no
  Tauri commands.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; this patch adds no DTOs.
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share. This patch's types are
  pipeline-internal; nothing crosses to ts-rs.
- Components only use CSS vars from `global.css`. The new
  `.body` cap and scrollbar styling use only existing design
  tokens (`var(--border-subtle)`); no hex literals.
- Runes-using files end in `.svelte.ts`. The Bucket change is
  CSS-inside-`<style>` only.
- L1 prompt edits come from observed classifications, not
  speculation. This patch edits no L1 prompts.
- L2 prompt edits come from observed authoring failures, not
  speculation. **This patch edits the propose-URL L2 prompt
  (v1.0 → v1.1) on the basis of the 2026-05-09 live-run
  evidence: nine of 11 expectations went uncovered because the
  proposer was trapped inside an authoritative-only ranking on
  a plan whose auth-primary endpoints were unreachable. The
  override is operator-confirmed.** The recipe-author prompt
  v1.15 stands.
- **Stockpile prompts: principle-only language.** The new
  propose-URL v1.1 sections name source classes
  (news/trade-press, authoritative flagships, statistical
  agencies) and observable properties (fetchability, prefetch
  budget, per-source deadline). No host name, no scheme
  matcher, no domain string in the prompt body.
- **Do not write code to pass tests.** No new tests. The
  override's effect is validated by live runs, not by fixtures.
- **Closed-vocabulary discipline.** The `.body` height cap
  applies uniformly to all six bucket types (no per-type
  routing); the prompt v1.1 sections name source classes
  rather than specific hosts.

End of patch.
