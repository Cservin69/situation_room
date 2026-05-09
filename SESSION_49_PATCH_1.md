# Session 49 — Patch 1

The Session 48 handoff named Piece A (live-run observation of the
post-Session-47 multi-expectation flow) as the operator's to drive.
The operator drove it on 2026-05-09 against a "global lithium supply
chain" plan; the run produced 0 records out of 7 nominations. This
patch lands the deferred Piece A as the documented observation, plus
one targeted fix for the failure class that the closed-vocabulary
discipline rule's "network-layer truth (UA, timeouts) with no LLM
path" allowance covers — the only class the operator can ship without
pre-design conversation.

The two other systemic findings (PDF excerpt budget cutoff before
deep-document chapters; per-source deadline starvation by a single
slow pre-fetch) are documented in the observation doc and explicitly
deferred. Each warrants design conversation; neither is a "constant
bump" fix.

Single bundled commit, in the Session 47 / Session 48 patch shape.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

No new dependencies. No schema change. No migration. No new IPC
commands. No new ts-rs DTOs. No new Svelte components. The change
threads two typed variants through `FetchError`, adds one
classification enum and one format helper at the executor's prefetch
boundary, refactors `prefetch_excerpt`'s return type from
`Option<...>` to `Result<..., PrefetchFailure>`, and writes a
markdown observation document.

The propose-URL prompt is **unchanged**. Its v1.0 vocabulary already
names every case the new prompt input now reaches it with.

## What's intentionally not in this patch

- **The propose-URL prompt is unchanged.** v1.0 stands. The prompt
  block "Reading prior attempts" already names `fetch failed: 404`
  and `fetch failed: 403/401` verbatim; this patch's only role is
  to deliver the prompt-vocabulary string to the LLM, not to teach
  it new vocabulary.
- **The recipe-author prompt is unchanged.** v1.15 stands. The
  recipe-author path's prior-attempts surface (recipe-author declines
  routed via `declined_this_attempt` into `summarize_attempts`)
  was already correctly stringified pre-Session-49 — that path is
  not regressed and is not widened.
- **No source-specific routing anywhere.** `PrefetchFailure` is a
  shape-only enum: no host, no scheme, no publisher names. The
  classifier projects a `FetchError` variant into the closed
  vocabulary and the format helper renders the variant. The LLM
  receives the truth and decides on policy.
- **No new closed-vocabulary entries.** The two typed `FetchError`
  variants and the five `PrefetchFailure` variants mirror the
  underlying `secure::HttpError` shapes the codebase already had —
  this patch lifts what was being collapsed, it does not invent.
- **No backfill.** The new typed variants exist only at runtime;
  pre-Session-49 prefetch failures were never persisted in a way
  that would benefit from re-classification.
- **PDF excerpt budget bump or topic-aware page selection.**
  Documented in the observation doc; deferred. The 64 KiB constant
  is the binding constraint for the lithium MCS run, but the right
  fix is one of three design choices, not a constant bump.
- **Per-source deadline / prefetch-timeout split.** Documented
  in the observation doc; deferred. Architectural — separate
  `SecureHttpClient` instance for prefetch with a tighter
  `total_timeout`.

## Files changed

### Pipeline — typed `FetchError` lift (Session 49 piece B)

- `crates/pipeline/src/http_fetcher.rs` —
  - **Module-level docstring**: new "Session 49 — typed status
    surfacing" section names this as the third typed lift after
    Track-D `RateLimited` (Session 25) and `Timeout` (Session 45).
    Names the live-run motivation (the propose-URL prompt's v1.0
    distinction between `fetch failed: 404` and `fetch failed:
    403/401` was dead because the executor stringified the status
    code before the prompt saw it).
  - **New variants**: `FetchError::Status(u16)` and
    `FetchError::TooLarge { max: usize, got: usize }`. Both follow
    the Session 45 pattern — typed because callers need to react
    differently, not because the operator surface benefits from a
    richer Display string.
  - **`From<HttpError>` updated**: 429-with-headers still routes
    to `RateLimited` first (Track-D path preserved); the new arms
    lift `HttpError::Status(_)` and `HttpError::StatusWithHeaders {
    status, .. }` (non-429) into `Status(u16)`, and
    `HttpError::ResponseTooLarge { max, got }` into `TooLarge`.
    Other shapes (DNS, TLS, redirect-rejected, URL-guard,
    `Request(_)`) continue to collapse to the generic `Http(String)`
    arm — the prompt has no host-class heuristic for those, so a
    catch-all bucket is the honest shape.
  - **`StaticFetcher`** in `testing` mod gains two new builders:
    `.status(url, code)` and `.too_large(url, max, got)`. The
    `code == 429` case is intentionally panic-asserted to point
    callers at `.rate_limited(url, retry_after)` — the typed
    `RateLimited` variant carries `Retry-After` and is the right
    fixture for 429.
  - **Tests**: 4 new tests pinning the lift —
    `status_lifts_to_typed_variant_session_49` (every common 4xx/5xx),
    `body_only_429_lifts_to_status_session_49` (defensive — the
    body-only 429 path with no header parses to `Status(429)`,
    not `RateLimited`),
    `too_large_lifts_to_typed_variant_session_49`,
    `dns_and_tls_errors_still_collapse_to_http_variant_session_49`
    (replaces the pre-Session-49
    `other_http_errors_still_collapse_to_http_variant_session_45`
    which used `Status(503)` — that case now lifts to `Status` per
    the new variant).

### Pipeline — `prefetch_excerpt` classified failure surfacing (Session 49 piece C)

- `crates/pipeline/src/fetch_executor.rs` —
  - **Runtime-fetch arm** in `fetch_recipe_bytes` (the post-author
    runtime path) gets two new sibling arms for `Status(code)` and
    `TooLarge { max, got }`. Each surfaces as `RecipeOutcome::Failed
    { stage: Fetch, message: ... }` — byte-equivalent to the
    pre-Session-49 catch-all path. The host-backoff layer does not
    react to either variant (status codes are not throttling
    signals; one oversized response isn't either).
  - **New `PrefetchFailure` enum** (`pub(crate)`): `Status(u16)`,
    `Timeout(Duration)`, `RateLimited { retry_after_seconds }`,
    `TooLarge { max, got }`, `Other(String)`. Doc-block names the
    closed-vocabulary discipline boundary it sits on.
    `PrefetchFailure::from_fetch_error` projects a `HttpFetchError`
    into the variant set; the `Http`/`NoFixture` cases collapse to
    `Other`.
  - **New `format_prefetch_failure_for_proposer`** function: maps
    each `PrefetchFailure` variant into a prompt-vocabulary string.
    `Status(404)` → `"fetch failed: 404"`. `Timeout(d)` → `"fetch
    failed: timeout after Ns"`. `RateLimited { retry_after_seconds }`
    → `"rate-limited; retry after Ns"` (matches the existing
    `format_retry_after` wording so the proposer's input and the
    runtime outcome reads use the same vocabulary). `TooLarge` →
    `"fetch failed: response too large (got at least N bytes, max
    M)"`. `Other(msg)` → `"fetch failed: <msg>"` after stripping
    the redundant `http error: ` prefix `FetchError::Http`'s
    Display contributes (avoid double-prefixing the proposer's
    "fetch failed:" bullet).
  - **`prefetch_excerpt` return type**: `Option<(String, Vec<u8>)>`
    → `Result<(String, Vec<u8>), PrefetchFailure>`. Failure paths
    (`BackoffOutcome::RateLimited`, `BackoffOutcome::Failed`)
    produce classified errors; success path now returns `Ok(...)`.
    Doc-block updated.
  - **Caller** in `author_for_nomination` — the prefetch-failed
    branch now formats the classified failure into
    `prior_attempts.reason` via the new helper, replacing the
    literal `"fetch failed (network error, bad status, or oversized
    response — see warn-level log above)"` string.
  - **Tests**: 8 new tests in the existing `tests` module —
    classification round-trips for every variant (status, timeout,
    rate-limited with/without header, too-large, other,
    no-fixture-as-other), the `http error:` prefix strip,
    rate-limited passthrough through `from_fetch_error`, and one
    integration-shape test
    (`run_fetch_for_plan_threads_status_class_into_proposer_history_session_49`)
    that exercises the whole flow: `StaticFetcher` configured to
    404 the first proposed URL, the LLM-mock decline-on-second-call,
    and an assertion that the second propose-URL prompt's
    `{{PRIOR_ATTEMPTS}}` block contains `"fetch failed: 404"` and
    does **not** contain the pre-Session-49 catch-all.

### Live-run observation document (Session 48 deferred Piece A)

- `docs/observations/2026-05-09-session-48-live-run-lithium.md`
  (new) —
  - Run metadata (plan id, provider, result), then a
    classification of every nomination's failure mode into four
    classes (A: status-code stripped; B: PDF excerpt truncated
    before chapter; C: per-source deadline starved by one slow
    fetch; D: HTML overview pages — the system worked on this one).
  - Per-class table for class A naming the five sources, the URLs
    proposed, and the actual status codes the executor saw but
    didn't surface to the prompt.
  - "What this patch fixes" / "What this patch does not fix"
    sections — class A is fixed; B, C, D are documented and
    deferred with the design choice each requires named.
  - LLM bill numbers and wall-clock time so the next session can
    measure changes against this baseline.

## Design notes worth preserving

### Why typed lifts, not string parsing

The pre-Session-49 path could have been preserved with a string-
parsing helper that recognised `"http error: status error: 403"` and
extracted the code. The typed-lift approach is preferred for the same
reason Session 25 lifted `RateLimited` and Session 45 lifted
`Timeout`: when callers need to react differently, the variant should
be typed; when callers only need a human-readable string, the
generic arm is enough. This patch's executor needs `Status(u16)` so
its prefetch-failure helper can render `"fetch failed: 404"` (matching
the prompt vocabulary verbatim) without parsing — and so any future
host-adaptation policy that wants to differentiate "404 → don't
adapt" from "5xx → adapt with caution" has the typed variant to
match on without a string scan.

### Why the format helper lives in `fetch_executor`, not on `PrefetchFailure`

`PrefetchFailure` is a closed enum at the executor's prefetch
boundary. The format helper renders it for **one specific consumer**
— the propose-URL prompt's `{{PRIOR_ATTEMPTS}}` rendering. A
future second consumer (the operator-facing UI, an audit log) would
want different rendering — the operator's UI might want the URL
plus a tone color; the audit log might want a structured row. Putting
the format on the type would force every consumer to share one shape.
Keeping it as a free function in the module that owns the consumer
is the Rust idiom for this case.

### Why `Other` carries a `String`, not a typed variant

DNS failures, TLS handshake errors, redirect rejections, and URL-
guard rejections are diagnostically distinct but **the propose-URL
prompt has no host-class heuristic that distinguishes them**. The
LLM's general knowledge handles "fetch failed: connection refused"
and "fetch failed: dns resolution failed" the same way it handles
"fetch failed: invalid certificate." A typed `Network`, `Tls`,
`Guard` split would add no actionable signal to the prompt; it would
add four match arms to the runtime executor and cost without buying
anything. Keeping the catch-all bucket is the honest choice.

### Why the `http error: ` prefix gets stripped

`FetchError::Http`'s Display contributes `"http error: <inner>"`
because the variant wraps every non-typed `HttpError` shape. The
proposer's prior-attempts entry already uses the convention "fetch
failed: ..." for every classified shape; without stripping, the
`Other` rendering would read `"fetch failed: http error: dns
resolution failed"` — the doubled "fetch failed: http error:" prefix
reads as noise. Pinned with a test
(`prefetch_failure_other_strips_redundant_http_error_prefix_session_49`)
so a future refactor of `FetchError::Http`'s Display doesn't silently
re-introduce the doubling.

### Why the body-only 429 path lifts to `Status(429)` rather than `RateLimited`

The Track-D `RateLimited` lift is reserved for the headers-aware
`HttpError::StatusWithHeaders { status: 429, headers }` shape — the
shape that carries `Retry-After` for the host-backoff layer to
honour. The body-only `HttpError::Status(429)` has no header to
parse; lifting it to `RateLimited { retry_after_seconds: None }`
would let the host-backoff layer adapt on a signal it has no
schedule for, and would let `fetch_with_backoff`'s inline-retry path
short-circuit on a value the caller can't act on. Routing it to the
new `Status(429)` variant instead is the honest answer: "the server
said 429 but gave us no hint when to retry; treat it as a generic
upstream error." The host-backoff layer doesn't react to `Status`,
which matches the principle (adapt on signals that the host is
asking us to slow down — a header-less 429 isn't a signal, it's
silence).

In production this path is unreachable: `SecureHttpClient`'s
`fetch_bytes` and `fetch_bytes_with_meta` both route through
`get_with_headers` which returns `StatusWithHeaders` on every non-
success status. The `Status(429)` test exists as a defensive pin
against a future refactor that re-introduces a body-only path.

## Test deltas

- `crates/pipeline/src/http_fetcher.rs` — replaced 1 obsolete test
  (`other_http_errors_still_collapse_to_http_variant_session_45`,
  which used `Status(503)` as the example case — the case now
  lifts to `Status(503)`); added 4 new tests pinning the typed
  lifts and the new `Other`-bucket coverage. Net: +3 tests in this
  module.
- `crates/pipeline/src/fetch_executor.rs` — added 8 new tests in
  the `tests` module: 7 unit tests for `PrefetchFailure`
  classification + format-helper round-trip + redundant-prefix
  strip, and 1 `tokio::test` that exercises the full retry-loop
  integration shape (404 on first attempt → status-class string in
  the second propose-URL prompt's prior-attempts block).

Pipeline test count: +3 + 8 = +11. API test count: unchanged. Other
crates' counts unchanged. All ignored tests (12) remain the existing
`#[ignore]` live integration tests.

## What's NOT in scope

Captured in the observation doc; restated here for the patch reader.

- **Class B — PDF excerpt truncation before deep chapters.** The
  64 KiB `PREFETCH_EXCERPT_BUDGET` is the binding constraint for the
  lithium MCS run; lithium chapter on page 110 falls off the end
  because ~50 chapters × 2–3 framed tables × ~700 bytes = ~70–100
  KiB. Three plausible designs (budget bump to 128 KiB; topic-aware
  page selection where the LLM's own `topic_tags` filter framed
  tables; chapter-heading detection that emits a per-chapter index
  before falling back to per-page framing) each warrant the design
  conversation. Doubling the budget fits inside
  `Bounds::LLM_PROMPT_BODY` (256 KiB) but trades against the
  prompt template + plan JSON + source metadata headroom — needs
  measurement against a real prompt size. Worth a Session 50 thread.
- **Class C — per-source deadline starvation by a single slow
  pre-fetch.** Architectural — a separate `SecureHttpClient`
  instance for prefetch with a tighter `total_timeout` (e.g. 60s).
  The LLM-call client keeps 300s for legitimately long completions.
  Touches the secure crate's config surface and the executor's
  `ExecutorContext` shape. Worth its own session.
- **Promotion pipeline (ADR 0004), Iterator Phase 2 (ADR 0016),
  charts on Observations / Events.** Same posture as Session 48.
- **xAI Responses API migration.** Same posture — only if a live
  `grok-4.3` run shows chat/completions silently ignoring
  `reasoning_effort`. (This run did not exercise that path; the
  three propose-URL/recipe-author tiers all consolidate to
  `grok-4.3` per the May 2026 catalog change. Token spend was
  comfortable; no signal that effort plumbing is wrong.)

## Hard rules carried over

Same as Sessions 41–48:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. This patch adds none.
- ADR 0009: every HTTP call goes through `SecureHttpClient`. This
  patch adds no HTTP calls; the new `StaticFetcher` builders are
  test-only.
- Bounds checking on every IPC string input. This patch adds no
  IPC commands.
- Tauri commands return `CommandError`. This patch adds no Tauri
  commands.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; this patch adds no DTOs.
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share. This patch's types are pipeline-
  internal; nothing crosses to ts-rs.
- Components only use CSS vars from `global.css`. This patch
  adds no components.
- Runes-using files end in `.svelte.ts`. N/A.
- L1 prompt edits come from observed classifications, not
  speculation. This patch edits no L1 prompts.
- L2 prompt edits come from observed authoring failures, not
  speculation. This patch edits no L2 prompts. The propose-URL
  prompt v1.0 stands; this patch reaches it with prompt-vocabulary
  strings the prompt was already shaped for.
- **Stockpile prompts: principle-only language.** Untouched here.
- **Do not write code to pass tests.** Every new test pins a
  contract the new variants / helpers explicitly hold.

End of patch.
