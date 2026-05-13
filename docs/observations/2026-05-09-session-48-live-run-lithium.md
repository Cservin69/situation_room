# 2026-05-09 — live run, post-Session-48, "global lithium supply chain"

The Session 48 handoff named this work as Piece A — observe a live
run of the post-Session-47 multi-expectation flow, classify what we
see, write down what's load-bearing for the next session. Piece A was
deferred from the Session 48 patch because it required cargo on the
operator's machine; this doc is the deferred Piece A landing in
Session 49.

## Run metadata

- **Plan**: `019e0c59-a077-74c0-8394-a6baa76aa964` — global lithium
  supply chain. 7 nominations from L1, 11 record-typed expectations
  (4 observation_metric, 3 event_type, 2 entity_kind, 2
  relation_kind), 730-day window, scope AU/CL/CN/AR.
- **Provider**: xAI, `grok-4.3` for all three tiers (per the
  Session 42 consolidation).
- **Result**: 0 records, 0 of 7 nominations succeeded, every
  expectation row reports UNCOVERED.

## What the live run revealed, by failure class

### A. Pre-fetch HTTP error, status code stripped before reaching the proposer

Five of seven nominations declined because the propose-URL retry
loop's first or second URL failed at pre-fetch and the proposer's
prior-attempts history received the literal string `fetch failed
(network error, bad status, or oversized response — see warn-level
log above)` regardless of the actual class. The propose-URL prompt
v1.0 (`config/prompts/propose_source_url.md`, "Reading prior
attempts") explicitly distinguishes:

- `fetch failed: 404` — wrong path on this host. Try a different
  path, not a different parameter on the same broken path.
- `fetch failed: 403/401` — host is blocking us. A different path
  on the same host is unlikely to fare better. Decline unless you
  know an open mirror.

With the executor erasing the status code before the prompt sees it,
both heuristics are dead — the proposer can't tell "this host blocked
us, try elsewhere" from "this path is wrong, try another path on the
same host". The five sources affected this run:

| Position | Nomination | First URL → status | Second URL → status |
|---:|---|---|---|
| 2 | SEC EDGAR (Albemarle 10-K/10-Q) | `cgi-bin/browse-edgar?company=Albemarle` → 403 | `cgi-bin/browse-edgar?CIK=0000915912` → 403 |
| 3 | World Bank Pink Sheet | `thedocs.worldbank.org/...` → 404 | `pubdocs.worldbank.org/...` → 404 |
| 6 | Fastmarkets battery-raw-materials | `/lithium/` → 404 | `/lithium-hydroxide/` → 404 |
| 7 | Reuters commodities | `feeds.reuters.com/reuters/commodities` → network error | `www.reuters.com/markets/commodities/` → 401 |

(Position 5, `industry.gov.au` REQ, is its own failure class — see C
below.) After the second attempt produced no new information the
proposer declined on the third propose-URL call, exhausting the
nomination.

### B. PDF excerpt budget eaten before the lithium chapter

The USGS Mineral Commodity Summaries (position 1) returned a 200-page
PDF; the lithium chapter starts on page 110. After Session 44 dropped
narrative on no-table pages and bumped `PREFETCH_EXCERPT_BUDGET` to
64 KiB, the reasoning was that a 110-page PDF with one small table
per page comes in around 55 KiB of framed output and fits the budget
end-to-end. The MCS shape is denser than that target: ~50 chapters
before lithium, two or three small tables each, ~700 bytes per framed
table = ~70–100 KiB of framed output before page 110. The 64 KiB
budget cuts off in the early-to-mid alphabet (well before "Lithium").

The LLM correctly declined for production and reserves with the
verbatim reason "the provided excerpt is truncated before the lithium
chapter on page 110, so no … table structure is visible for
pdf_table authoring." It correctly declined for refining_capacity
and spot_price on the orthogonal grounds that USGS MCS doesn't carry
those data. No bug in the LLM's behaviour; the bug is in the
prefetch's coverage of long PDFs.

This is the failure mode that motivated Session 44's narrative drop
in the first place. The drop bought ~30 KiB of headroom; long
densely-tabulated regulatory PDFs eat through that and we're back at
the same shape.

### C. Per-source deadline starved by a single slow pre-fetch

Position 5, the Australian Office of the Chief Economist Resources &
Energy Quarterly (industry.gov.au), produced this sequence:

```
10:55:27.192239  pre-fetching endpoint hint  source_id=www.industry.gov.au  url=https://www.industry.gov.au/p***2023
11:00:27.197794  pre-fetch failed (timed out after 300s)
11:00:27.199494  per-source deadline (240s) exceeded after 1 attempt(s)
```

The default `SecureHttpConfig::total_timeout` is 300s. The per-source
authoring deadline is 240s
(`PER_SOURCE_DEADLINE_SECS`). When the LLM provider's response is the
binding constraint, 240s is generous. When the *pre-fetch* is the
binding constraint, a single attempt can consume more than the entire
per-source budget, and the next two attempts of `MAX_AUTHORING_ATTEMPTS_PER_SOURCE`
have no time to run. The proposer never gets a chance to suggest an
alternate URL on this host — an interaction the per-source deadline
constant's rustdoc didn't anticipate ("the deadline only bites when
the LLM gateway slows down dramatically").

Recipe for a fix: separate `SecureHttpClient` instance for the
prefetch path with a tighter `total_timeout` (e.g. 60s) so a slow host
fails fast and leaves room for the next attempt. The LLM-call client
keeps its 300s ceiling. The fix is architectural, not a constant
bump; it warrants its own session.

### D. HTML overview/marketing landing pages

Position 4, the IEA Critical Minerals / Global EV Outlook nomination,
returned a server-rendered landing page with no extractable data on
both attempted URLs (`/reports/global-ev-outlook-2024` and a
critical-minerals-outlook variant). Authoring-time validation correctly
rejected one CSS selector that matched a body containing 112002 bytes
(an iframe-driven SPA shell), and the LLM declined honestly on the
remaining targets. **No bug.** The system worked: the prefetch's HTML
digest gave the LLM the structure (titles, headings, repeating
classes) and the LLM correctly identified that the structured data
lives in PDFs / interactive downloads behind those overview pages.
Authoring-time validation's role is exactly to cut off "selector
matches a container element" recipes before they hit storage.

That said: the proposer's URL choices for IEA were informed only by
"a description that mentions IEA" plus "no prior attempts." Both URLs
it picked had the same overview-page shape. With prior-attempts
history that read `recipe author declined: HTML landing page with no
extractable structure`, the proposer could have routed to a PDF
download URL or skipped IEA entirely. **The decline reason was
preserved** in this case (recipe-author declines route through
`declined_this_attempt` into prior_attempts via `summarize_attempts`
— that path was correct pre-Session-49 and stays correct post-).
This isn't a Session 49 fix; it's evidence that the existing surfacing
of recipe-author declines is sufficient for the SPA case.

## What this patch fixes

**Class A only.** Network-layer truth (HTTP status code, timeout
shape, oversized-response numbers) now travels into the propose-URL
retry loop's prior-attempts history, where the prompt v1.0's
existing vocabulary picks it up. Pure plumbing of an already-typed
signal that was being stringified before it reached the LLM.

The fix is two typed lifts on `FetchError` (`Status(u16)` and
`TooLarge { max, got }` — the third typed lift after Track-D's
`RateLimited` and Session-45's `Timeout`), one new
`PrefetchFailure` enum at the executor boundary, and one
`format_prefetch_failure_for_proposer` helper that maps each variant
into the prompt's `fetch failed: <code>` / `fetch failed: timeout
after Ns` / `fetch failed: response too large (...)` / generic
`fetch failed: <message>` shapes. Tests pin the wire format.

The propose-URL prompt is **unchanged** — its v1.0 vocabulary already
names the cases the prompt input now reaches it with.

## What this patch does not fix

- **Class B** (PDF excerpt truncation). Three plausible designs —
  budget bump to 128 KiB, topic-aware page selection, chapter-
  detection heuristic — each warrants design conversation. The
  64 KiB constant is the binding constraint; doubling it fits inside
  `Bounds::LLM_PROMPT_BODY` (256 KiB) but trades against prompt
  template + plan JSON + source metadata headroom. Worth a Session 50
  thread.
- **Class C** (per-source deadline starvation). Architectural —
  separate `SecureHttpClient` instance for prefetch with a tighter
  timeout. Worth its own session; touches the secure crate's
  config surface.
- **Class D** (SPA / overview HTML). Not a bug — the system worked
  as designed. The propose-URL prompt could be richer about
  recognising overview-page descriptions before proposing, but that
  is a prompt edit grounded in observed declines, not a runtime fix.

## Numbers worth keeping

- 7 nominations × 3 max attempts = 21 propose-URL calls available
  per fetch-run. This run made 14 (one short on industry.gov.au due
  to deadline starvation, one less per nomination that the proposer
  declined early on).
- LLM bill, this run: 14 propose-URL calls (Cheap), 12 recipe-author
  calls (Workhorse). Of the 12 recipe-author calls, 8 declined on
  insufficient excerpt and 4 declined on data-not-present. None
  succeeded.
- Wall-clock time, post-classify to fetch-run-complete: ~13.5 minutes.
  The industry.gov.au 300s timeout accounted for ~37% of that
  single-handedly.

## Cross-references

- Session 47 patch — multi-recipe-per-nomination architectural
  piece (`SESSION_47_PATCH_1.md`).
- Session 48 patch — operator-introspection surfaces (host-backoff
  status panel, sources-memory panel) over the post-Session-47
  pipeline (`SESSION_48_PATCH_1.md`).
- Session 49 patch 1 — Class A fix
  (`SESSION_49_PATCH_1.md`): typed `FetchError::Status` /
  `TooLarge` lifts, `PrefetchFailure` classification at the
  prefetch boundary, prompt-vocabulary strings into the propose-URL
  retry loop's prior-attempts history.
- Session 50 patch 1 — Class B + C fixes plus runtime-UI scaffold
  (`SESSION_50_PATCH_1.md`):
  - **Class B (PDF excerpt truncated before deep chapters)** —
    chosen design: topic-aware page selection. The 64 KiB
    `PREFETCH_EXCERPT_BUDGET` stays; the lever is *which* pages
    fill it. `PrefetchRelevance` is built once per nomination
    from `plan.topic_tags + nomination.description +
    plan.geographic_scope` and scores each page's framed text;
    selection keeps `PREFETCH_PDF_HEAD_PAGES` (= 3) for
    orientation plus the highest-scoring pages until the budget
    fills, with explicit `[... N pages skipped (low topic
    relevance) ...]` markers between gaps. Closed-vocabulary
    discipline preserved (no host names, no learned classifiers,
    no document-class heuristics). The other two designs from
    this doc — budget bump to 128 KiB, chapter-heading detection
    — are documented as not-shipped because the constant bump is
    a sticking-plaster (the next dense PDF blows through 128 the
    same way) and the heading-detection design requires either a
    new crate dependency or a hand-rolled `/Outlines` parser
    fragile against PDFs without outlines.
  - **Class C (per-source deadline starvation)** — chosen design:
    separate `SecureHttpClient` instance for prefetch with
    `total_timeout: 60s`. The LLM-call client keeps the default
    300s ceiling. Both clients share the per-host backoff state
    so observed throttling on a host carries over between paths
    exactly as the pre-Session-50 single-client flow did. 60s is
    the value chosen because most legitimate prefetches land in
    5–25s; even four consecutive timeouts (4 × 60s = 240s) fit
    inside the per-source authoring deadline.
  - **Runtime-UI scaffold (operator pull, separate from the
    observation classes)** — `FetchReport.svelte` now pre-renders
    the plan's `expectations.document_sources` as a "running now"
    list the moment `plans.fetching` flips true. Each row shows
    "queued" because the existing `run_fetch_for_plan` IPC
    command returns one bundled report at the end; per-nomination
    stage transitions ("queued" → "fetching" → "authoring") would
    require a Tauri event channel and a parallel state surface
    (Session 51 thread). The cheap path is enough to remove the
    "20+ sessions of staring at a void" symptom: the operator
    sees what the executor is iterating instead of nothing.
- **Class D (HTML SPA / overview pages)** — no fix needed; the
  system worked as designed. Documented above as evidence that
  recipe-author decline routing is sufficient for the SPA case.
