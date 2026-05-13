# Session 50 — Patch 1

The Session 49 patch landed two of the four classes from the
2026-05-09 lithium MCS observation: A (status-code stripped before
proposer) shipped via typed `FetchError::Status`/`TooLarge` lifts and
a `PrefetchFailure` classification at the prefetch boundary; D was
documented as "no bug, system worked." Classes B (PDF excerpt
truncated before deep-document chapters) and C (per-source deadline
starved by a single slow pre-fetch) were named as Session 50 design
threads.

This patch lands B and C as a single bundled commit, plus a third
piece the operator pulled in: a runtime-UI scaffold so the fetch
review panel shows the queue of nominations during a run instead of
a void until the synchronous `run_fetch_for_plan` IPC returns.

The three pieces share a session because they all surface in the
same operator workflow — running a fetch on a real plan and
watching what happens. B and C are pipeline-side; the UI piece is
frontend-only and small. The bundling matches the Session 47 / 48 /
49 patch shape (one commit per session, all under one observation).

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

No new dependencies. No schema change. No migration. No new IPC
commands. No new ts-rs DTOs. No new Svelte components.

The pipeline change threads a new `Option<&PrefetchRelevance>`
parameter through `prefetch_excerpt` and `render_pdf_text_with_tables`
(via a new `..._with_relevance` variant; the no-arg wrapper
preserves the pre-Session-50 byte-identical output for existing
callers and tests). The `ExecutorContext` gains an
`Option<&dyn HttpFetcher>` field for the prefetch-client split. The
production composition root (`apps/desktop/src-tauri/src/main.rs`)
constructs a second `SecureHttpClient` with a 60s `total_timeout`
and threads it through `AppState`. The frontend pre-renders the
nominations list when `plans.fetching === true` and a plan is
selected.

The propose-URL prompt is **unchanged**. The recipe-author prompt is
**unchanged**. No L1 / L2 prompt edits in this session.

## What's intentionally not in this patch

- **Tauri event channel for per-nomination progress.** The Right
  shape for live fetch-state surfacing — what would let rows in
  the running-now scaffold flip from "queued" to "fetching" /
  "authoring" / terminal outcome live — needs a new IPC surface,
  a new DTO that mirrors the executor's stage taxonomy without
  becoming a parallel state surface that drifts from
  `RecipeOutcome`, and channel-lifecycle decisions (backpressure,
  disconnect, replay-on-reconnect). Session 51 thread. The
  Session 50 cheap path (pre-rendered "queued" rows from the plan's
  `document_sources`) is enough to remove the "20+ sessions of
  staring at a void" symptom.
- **Topic-aware page selection for HTML / JSON.** The PDF branch
  was the binding constraint per the live-run obs (long densely-
  tabulated regulatory docs). HTML's structural digest already
  aggressively bounds itself; JSON's outline is small. If a future
  observation shows HTML or JSON eating budget, the lever is the
  same shape as the PDF one and the helper code generalises
  cleanly.
- **Vocabulary drawn from plan expectations.** The relevance
  vocabulary draws from `plan.topic_tags` + `nomination.description`
  + `plan.geographic_scope` only. The plan's expectations
  (metric_id, event_type, entity_kind, relation_kind) often carry
  domain-internal slugs (`refining_capacity`,
  `export_control_enacted`) that don't match the natural-language
  surface of a published PDF. Including them would either bias
  scoring with token noise or require translating slugs to surface
  form — both speculative. Document; defer.
- **Backfill of past prefetch failures with topic-aware retry.**
  Pre-Session-50 prefetch outputs aren't persisted in a way that
  would benefit from re-rendering against the new vocabulary. The
  next live run produces fresh excerpts under the new path.
- **Tunable prefetch timeout via config.** The 60s ceiling is
  hard-coded in the binary's composition root with the rationale
  inline. If a live run shows 60s is too tight (a 200 MiB+ PDF
  trickle, say), the lift is one line and the value should be
  driven by observation, not pre-emptively configurable.

## Files changed

### Pipeline — topic-aware PDF page selection (Session 50 piece A, Class B)

- `crates/pipeline/src/fetch_executor.rs` —
  - **Module-level**: imports `BTreeSet` for the relevance vocab
    set's deterministic ordering.
  - **`ExecutorContext`** gains `prefetch_http: Option<&'a dyn
    HttpFetcher>`. Optional because the pre-Session-50 callers and
    tests don't exercise prefetch timing; fallback to `ctx.http`
    via `ctx.prefetch_http.unwrap_or(ctx.http)` preserves their
    behaviour byte-for-byte. Doc-block names the closed-vocabulary
    discipline boundary the split sits on.
  - **`prefetch_excerpt`** signature widens to
    `(ctx, url, source_id, relevance: Option<&PrefetchRelevance>)`.
    Routes through `ctx.prefetch_http.unwrap_or(ctx.http)` so the
    production binary's tighter prefetch client serves
    propose-URL retry-loop pre-fetches without affecting the LLM
    provider client. Session 50 piece B threads the second client
    through the executor; the routing is here so the fix is one
    function, not one per call site.
  - **`render_pdf_text_with_tables`** becomes a thin wrapper
    around the new `render_pdf_text_with_tables_with_relevance`
    that passes `None`. Existing callers and tests get
    byte-identical output through this entry point. The new
    caller in `prefetch_excerpt` uses the relevance variant
    directly.
  - **`render_pdf_pages_with_tables`** (new, pure over `&[String]`):
    frame all pages, score against the vocabulary, select pages
    under the budget, emit in document order with skip markers
    between gaps. Pure for testability — tests build synthetic
    page strings inline without going through `pdf_extract`.
  - **`frame_one_pdf_page`** (extracted from the old inline loop):
    same per-page coordinate-space framing the runtime extractor
    indexes into. Byte-equivalent to the pre-Session-50 inline
    block.
  - **`select_pdf_pages_by_relevance`** (new): scores each framed
    page; if every page scores zero returns all indices (caller
    falls back to document-order emission); otherwise includes
    `PREFETCH_PDF_HEAD_PAGES` (= 3) for orientation and adds
    remaining pages by descending score until the budget would
    be exceeded.
  - **`score_text_against_vocab`** (new): lowercase substring
    match counter. Pure function for testability. Counts every
    occurrence of every vocab token; multiple matches per page
    sum.
  - **`emit_selected_pdf_pages`** (new): joins selected pages in
    document order with `\n\n`, emits a `[... N pages skipped
    (low topic relevance) ...]` marker for each gap. The marker
    is closed-vocab clean — no source-specific text, no host
    name.
  - **`PrefetchRelevance`** (new struct, `pub(crate)`): carries
    the lowercase distinct-token vocabulary built from
    `plan.topic_tags + nomination.description +
    plan.geographic_scope`. Built once per nomination outside the
    propose-URL retry loop.
  - **`tokenize_for_prefetch_relevance`** (new): tokenize on
    non-alphanumeric word boundaries, retain length >= 4, drop
    pure-numeric tokens, lowercase.
  - **`PREFETCH_PDF_HEAD_PAGES`** (new const, value 3):
    always-included head-of-document page count for orientation.
    Documented rationale in its rustdoc.
  - **`PREFETCH_RELEVANCE_MIN_TOKEN_LEN`** (new const, value 4):
    minimum token length retained in the vocabulary. Drops "of",
    "the", "and", and most ISO codes (`HU`, `CL`, `CN`).
  - **`PREFETCH_RELEVANCE_STOPWORDS`** (new const list): tiny
    closed list of high-frequency low-information English tokens
    that pass the length filter but dilute scoring. Includes
    `data`, `report`, `annual`, `table`, `page`, etc.
  - **Caller** in `author_for_nomination`: builds
    `PrefetchRelevance::from_plan_and_nomination(plan, nomination)`
    once, outside the attempt loop (the vocabulary is stable
    across attempts). Passes `Some(&owned)` when non-empty,
    `None` otherwise.
  - **Tests**: 11 new tests pinning the vocabulary projection,
    the scoring function, the selection algorithm under budget,
    the head-page inclusion, the all-zero fallback, the
    skip-marker emission, and the `prefetch_http` routing.

### Pipeline — separate prefetch SecureHttpClient (Session 50 piece B, Class C)

- `crates/pipeline/src/fetch_executor.rs` —
  - **`ExecutorContext`** gains `prefetch_http` field (covered
    above; the routing change in `prefetch_excerpt` reads from
    it). Existing test sites get `prefetch_http: None,` added by
    a one-shot script-driven edit.
- `crates/api/src/commands.rs` —
  - **`AppState`** gains `prefetch_http: Arc<SecureHttpClient>`.
    Doc-block names the rationale: tighter `total_timeout`
    prevents a single slow prefetch host from starving the
    `PER_SOURCE_DEADLINE_SECS` (240s) budget.
  - **`AppState::new`** signature widens to take the second
    client.
  - **`run_fetch_for_plan` command** wraps the prefetch client
    in its own `BackoffFetcher` (sharing the `host_backoff`
    state with the LLM-call client's wrapper) and passes
    `Some(&backoff_prefetch)` into the `ExecutorContext`. The
    backoff state stays symmetric across both clients —
    observed signals on a host carry over between paths exactly
    as the pre-Session-50 single-client flow already did.
- `apps/desktop/src-tauri/src/main.rs` —
  - **Prefetch client construction**: a `SecureHttpConfig` with
    `total_timeout: Duration::from_secs(60)` (struct-update from
    `default()`) feeds a second `SecureHttpClient::new`. The
    LLM-call client keeps the default 300s ceiling.
  - **`AppState::new` call** widens to pass the second client.
    Doc-block names "60s" with the rationale: most legitimate
    prefetches land in 5–25s; even four consecutive timeouts
    (4 × 60s = 240s) fit inside the per-source deadline.

### Frontend — running-now scaffold (Session 50 piece C)

- `apps/desktop/src/components/FetchReport.svelte` —
  - **New running-now block**, gated on `plans.fetching &&
    plans.selected`. Renders the plan's
    `expectations.document_sources` as a list with one row per
    nomination, each marked "queued". Priority-tier badge
    coloured per existing source-priority palette. Long
    descriptions truncate-with-tooltip so rows stay scannable.
  - **`tierShortLabel`** helper (new): 4-char abbreviation
    (`P1` / `P2` / `TP` / `GN`) for the inline tier badge.
  - **CSS additions**: `.running-now`, `.running-list`,
    `.running-row`, `.row-marker`, `.priority-tier`,
    `.description`, `.running-explainer` rules. All use existing
    `global.css` design tokens (`var(--bg-panel-alt)`,
    `var(--signal-info)`, etc.); no hex literals; no animated
    spinners (static text label + dashed border-left signals
    in-flight).
  - **Doc-block addition** at the top of the file names the
    Session 50 scope (cheap path) and the Session 51 thread
    (Tauri event channel for live per-nomination state).
  - The previous run's `fetchReport` continues to render below
    the running-now block while a new run is in flight. That
    preserves at-a-glance "did this plan ever produce records"
    context without swapping the whole panel mid-run.

## Design notes worth preserving

### Why topic-aware over budget bump (Class B)

The observation doc enumerated three options for Class B: budget
bump to 128 KiB, topic-aware page selection, chapter-heading
detection. The constant bump was the simplest but it's a
sticking-plaster — the next dense PDF (FERC, IEA full reports)
blows through 128 KiB the same way 64 KiB blew through. Chapter-
heading detection requires either a new crate dependency or a
hand-rolled `/Outlines` parser on top of `lopdf`, fragile against
PDFs without outlines. Topic-aware selection threads a signal the
plan already carries (`topic_tags`, `geographic_scope`,
`nomination.description`) and produces principle-only ranking — no
host names, no document classes, no learned classifiers.

### Why scoring uses framed text, not raw page text

The framed text (`[PDF page N, table M] (R rows × C cols)\n  row
0: "Country" "Production"\n  row 1: "Chile" "Lithium"`) is what
the LLM will see and author against. Pages with no detected
tables yield framed text of just `[PDF page N] (no table
detected)` — they score zero against any vocabulary, and they
contribute nothing the LLM can author `pdf_table` coordinates
against. Scoring on framed text aligns the relevance signal with
authorability: a page with lots of vocab in narrative but no
detected table can't be authored, and shouldn't crowd out a page
with a detected table whose cells contain the vocab.

### Why head-of-document pages are always included

The first three pages typically carry document title / abstract /
table of contents — orientation context the LLM uses to verify
"yes, this is the right document." Without them, a long PDF
where vocab tokens hit only late chapters could surface those
chapters with no contextual frame, and the LLM has no way to
sanity-check it's reading USGS MCS vs. EUR-Lex vs. a random
mirror. The head-page floor costs ~3 KiB of budget on most
documents — small compared to the 64 KiB ceiling.

### Why the prefetch client is a separate `SecureHttpClient` and not a per-call timeout override

`SecureHttpClient` has no per-call timeout override surface. Adding
one (`with_timeout`, `request_timeout` arg) would touch the secure
crate's stable boundary and change the contract every other caller
relies on. A separate client is the smaller change: same
`SecureHttpClient` type, different config, threaded through
`ExecutorContext` as a separate field. The `BackoffFetcher` wrapper
generalises cleanly (it takes `&dyn HttpFetcher`), so wrapping the
second client is one line at the composition root.

### Why both clients share `HostBackoff` state

Observed throttling signals on a host (429 / `Retry-After` /
timeout patterns) are about the host's behaviour, not which client
hit it. Separating the backoff state per client would let the
prefetch client and the runtime fetch client adapt independently
to the same host's signals — re-learning what the other already
knew. Sharing the state means a 429 observed during prefetch
informs the runtime fetch's next call to that host, exactly as the
pre-Session-50 single-client flow already did.

### Why the running-now scaffold pre-populates from the plan, not from an in-flight executor handle

The cheap path renders what the plan *says* the executor will
process, not what the executor *is* processing. The two are
identical at run start (the executor iterates
`plan.expectations.document_sources` in order); they would diverge
if the executor short-circuits on a deadline or skips a nomination
on a structural failure. Pre-populating from the plan is honest
about what the operator can verify without a live state stream:
the queue is what the plan declares; live state is the Session 51
thread.

### Why a single static "working…" label, not an animated spinner

The dashed border-left already signals in-flight state in the
existing run-history strip's pending-row treatment. A second
animation in the same panel would be motion noise; a static
"working…" label is enough alongside the dashed border. Reduced-
motion-conscious operators get the same signal without an
animated element they'd need to suppress.

## Test deltas

- `crates/pipeline/src/fetch_executor.rs` — added 11 new tests:
  - 2 for `PrefetchRelevance` vocab projection (substantive tokens
    present, stopwords filtered, short tokens filtered, pure-numeric
    filtered; empty case for stopwords-only nomination).
  - 2 for `score_text_against_vocab` (substring count;
    empty-vocab-returns-zero).
  - 4 for `render_pdf_pages_with_tables` (no-relevance fallback,
    all-zero-score fallback, head-and-high-score selection, budget
    respected, skip-marker emitted on gaps, contiguous-no-marker).
  - 2 for `prefetch_excerpt`'s `prefetch_http` routing (Some(_)
    path, None fallback path).
  - 1 for `select_pdf_pages_by_relevance` direct invocation
    (head + high-score selection in document order).
  - 1 for `emit_selected_pdf_pages` (skip marker between selected
    pages with gaps).
- `crates/api/src/commands.rs` — no test deltas; the
  `prefetch_http` field addition is a struct-shape change covered
  by the type system.
- `apps/desktop/` — no test deltas; the scaffold renders from
  existing store state and the existing svelte-check / type-check
  run will catch any wire-shape mismatches.

Pipeline test count: +11. API test count: unchanged. Other crates'
counts unchanged. All ignored tests (12) remain the existing
`#[ignore]` live integration tests.

## What's NOT in scope

Captured above; restated for the patch reader.

- **xAI Responses API migration.** Same posture as Sessions 47–49 —
  only if a live `grok-4.3` run shows chat/completions silently
  ignoring `reasoning_effort`. This patch's pipeline changes do not
  touch the LLM provider crate.
- **Promotion pipeline (ADR 0004), Iterator Phase 2 (ADR 0016),
  charts on Observations / Events.** Same posture as Sessions
  48–49.
- **Per-nomination live state (Session 51 thread).** The cheap-path
  scaffold renders queue contents from the plan; the right-path
  event channel is its own session.

## Hard rules carried over

Same as Sessions 41–49:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. This patch adds none.
- ADR 0009: every HTTP call goes through `SecureHttpClient`. This
  patch adds two: the existing default-config client (LLM + runtime
  fetch) and the new 60s-timeout client (prefetch). Both are
  `SecureHttpClient` instances; nothing else.
- Bounds checking on every IPC string input. This patch adds no
  IPC commands.
- Tauri commands return `CommandError`. This patch adds no Tauri
  commands.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; this patch adds no DTOs.
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share. This patch's types are pipeline-
  internal; nothing crosses to ts-rs.
- Components only use CSS vars from `global.css`. The new running-
  now CSS block uses only existing design tokens.
- Runes-using files end in `.svelte.ts`. The store change is to
  `plans.svelte.ts` (no change in this patch — frontend reads
  existing state).
- L1 prompt edits come from observed classifications, not
  speculation. This patch edits no L1 prompts.
- L2 prompt edits come from observed authoring failures, not
  speculation. This patch edits no L2 prompts. The propose-URL
  prompt v1.0 stands; the recipe-author prompt v1.15 stands.
- **Stockpile prompts: principle-only language.** Untouched here.
- **Do not write code to pass tests.** Every new test pins a
  contract the new helpers / constants explicitly hold; the
  topic-aware selection is principle-only and the scoring is
  generic.
- **Closed-vocabulary discipline.** No source-specific routing
  anywhere. The relevance vocabulary is built from plan-supplied
  signals; nothing in `PrefetchRelevance` or the scoring helpers
  mentions a host, scheme, publisher, or document class. The
  prefetch-client split is purely network-layer (timeout shape).

End of patch.
