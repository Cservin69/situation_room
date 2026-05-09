# Session 45 — Patch 1

Piece D from the Session 45 handoff: network-layer issues carried
from Sessions 40 / 43 / 44. Single tarball / one commit pair per the
handoff's explicit rule ("Touches `SecureHttpClient`'s UA default,
the candidate-source data file, and the per-host backoff state in
the fetch client — three distinct surfaces. Bundle them together;
they all live at the network layer.")

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

No new dependencies. No schema changes. No migration. The change
adds one new typed error variant (`FetchError::Timeout`), reshapes
the default `User-Agent`, and lands a per-host backoff decorator at
the pipeline layer.

## Files changed

- `crates/secure/src/http.rs` —
  - Default `user_agent` reshaped from
    `situation_room/<version> (+https://github.com/situation_room)`
    to `SituationRoom/<version> (+<repo-url>)`, with `<version>` from
    `CARGO_PKG_VERSION` and `<repo-url>` from `CARGO_PKG_REPOSITORY`
    (workspace `repository` field). Both segments are baked at build
    time.
  - Field-level rustdoc on `SecureHttpConfig::user_agent` rewritten:
    the SEC-EDGAR-specific sentence is gone; the new prose explains
    why a non-empty default closes the SEC `data.sec.gov` 403 case
    *generically* and names "SEC is one caller among many" so a
    future reader doesn't reintroduce per-source naming.
  - Two new unit tests (`default_user_agent_is_build_time_identifier_session_45`,
    `user_agent_override_threads_through_config_session_45`) pin the
    default UA shape and assert that per-fetcher overrides through
    the existing `pub user_agent: String` field still build a
    working client.

- `crates/pipeline/src/http_fetcher.rs` —
  - New `FetchError::Timeout(Duration)` variant. `From<HttpError>`
    rewritten from an if-let chain to a match that lifts both
    `StatusWithHeaders { 429, … }` (existing) and `Timeout(d)` (new)
    into typed variants; everything else still flattens to
    `Http(String)`. The lift is what lets the per-host backoff layer
    distinguish "host is timing out — adapt" from "404 — don't
    adapt" without string-matching the error message.
  - Module rustdoc gains a `## Session 45 — typed timeout surfacing`
    section explaining the lift and naming the
    `crates/pipeline/src/fetch_backoff.rs` consumer.
  - `StaticFetcher` (test fixture) gains a `timeouts` map and a
    `.timeout(url, duration)` builder so the per-host backoff tests
    can synthesise typed timeouts without hitting real network.
  - Two new unit tests (`timeout_lifts_to_typed_variant_session_45`,
    `other_http_errors_still_collapse_to_http_variant_session_45`)
    pin the lift behaviour and the catch-all fall-through.

- `crates/pipeline/src/fetch_backoff.rs` —
  - New `HostBackoff` struct (per-host adaptive state, keyed at
    runtime on the URL host string) and `BackoffFetcher<'a>`
    decorator that wraps any `&dyn HttpFetcher` to apply per-host
    pre-flight waits and record observed signals (429,
    `Retry-After`, timeouts, successes).
  - `exponential_backoff(failures)` schedule: `1s, 2s, 4s, 8s, 16s,
    32s, 60s, 60s, …` — capped at `HOST_BACKOFF_MAX_SECS = 60` so a
    sustained-failing host stabilises at one request per minute
    rather than diverging. `HOST_BACKOFF_BASE_SECS` and
    `HOST_BACKOFF_MAX_SECS` are public constants for test
    assertions and patch-note math when the cap moves.
  - `host_of(url)` helper parses URLs to host strings, lowercases
    for case-insensitive dedup, and returns `""` on URL parse
    failures or schemes without a host (e.g. `data:`). Empty-host
    routes through the decorator without state changes.
  - 14 new unit tests covering: pre-flight wait semantics,
    429-with-Retry-After honoring, 429-without-header exponential
    schedule, timeout-equals-429-no-header policy, per-host
    isolation (state is per-host, not global), success-resets-
    counter recovery story, max-cap convergence, host case-
    folding, decorator pass-through behaviour for the happy path,
    decorator state-recording for 429 / Timeout / success, decorator
    no-block behaviour on cold-start, and `Content-Type` threading
    through the decorator (Session 32 regression-pin).

- `crates/pipeline/src/fetch_executor.rs` —
  - One new arm in the match on `BackoffOutcome::Failed(...)` inside
    `fetch_recipe_bytes`:
    `BackoffOutcome::Failed(HttpFetchError::Timeout(d)) => Err(RecipeOutcome::Failed { … message: format!("timed out after {d:?}") })`.
    By the time this arm fires, the per-host backoff layer has
    already pushed the host's `next_allowed_at` out; the executor
    surfaces the typed reason in the recipe-failure message.
  - The other call site (`prefetch_endpoint_hint` at line 1209)
    matches `BackoffOutcome::Failed(e) => …` with a wildcard, so
    it's unaffected.

- `crates/api/src/commands.rs` —
  - `AppState` gains `pub host_backoff: Arc<HostBackoff>`. Constructed
    inside `AppState::new` (no signature change) — the policy is
    uniform across deployments, so the composition root has no knob
    to thread.
  - `run_fetch_for_plan` constructs a scope-bound `BackoffFetcher`
    around `state.http.as_ref()` and `state.host_backoff.clone()`
    before building `ExecutorContext`. The wrapper has scope-bound
    lifetime; the underlying state lives in `AppState` so observed
    signals carry across consecutive `run_fetch_for_plan` calls
    until the binary restarts.

- `.env.example` —
  - `SEC_EDGAR_USER_AGENT=…` placeholder removed. It was never read
    by any crate (zero call sites pre-Session-45) and its presence
    lied about the architecture by suggesting per-source overrides
    were the move. Replaced with a multi-line comment explaining the
    new build-time UA and how to override at the boot path.

## Architecture

### Three sub-pieces, one network layer

The handoff named three architectural openings: SEC user-agent,
Reuters feeds, `industry.gov.au` timeouts. Each had a principle-
clean answer drafted during Session 44 (the operator audited them
with `are we again hardcoding sources?` and they held). Session 45
ships them together because they all live at the network layer and
share the same audit constraint: no `if host == X` branch, no
per-source code path, no `[per_host."<hostname>"]` config table.

### Sub-piece D-1 — SEC user-agent

**The 403.** `data.sec.gov` rejects requests with an empty
`User-Agent` (or one matching a known bot/library signature) with
HTTP 403. Pre-Session-45 the executor would surface that as a fetch
failure with no diagnostic; the operator had no signal that the
cause was UA shape rather than a malformed URL or a real outage.

**Where the fix went.** `SecureHttpConfig::default` ships a build-
time identifier of the shape `SituationRoom/<version> (+<repo-url>)`
for *every request*. The contact piece is the workspace's
`repository` URL (per the SEC's own guideline: an email or a project
URL is acceptable). The `+` prefix is the long-standing convention
(Googlebot et al.) for "the URL that follows is the contact for this
client."

The default works for SEC because it works for *every* host that
needs a non-empty UA — the fix is at the client, not at the source.
Per-fetcher override remains the existing `pub user_agent: String`
field; LLM providers and any future per-source overrides can set it
directly without touching the secure crate.

**Why no `SEC_EDGAR_USER_AGENT` env var.** That was the
pre-Session-45 placeholder in `.env.example` — a SEC-shaped knob
that no crate consumed. Two things wrong with reviving it: (a) it
encodes a per-source mental model the architecture has explicitly
rejected, (b) it would suggest other per-source UAs are the next
move when in practice every host gets the same default. The
placeholder is gone, replaced with prose that explains the new
default and the override path.

### Sub-piece D-2 — Reuters feeds

**The original observation.** Session 40 found that
`feeds.reuters.com` either 404s or hangs depending on path; the
URLs that used to work appear defunct.

**Why the patch is a no-op at the registry layer.** Post-ADR-0015
(Session 37), there is no static candidate-source registry to drop
Reuters from. The classifier consumes `Store::sources_memory` —
a derived view over `recipes ⨝ recipe_fetch_attempts` that only
surfaces URLs that have *succeeded at least once*. Reuters URLs
that fail at fetch time never enter the memory; the architecture
already excludes them. The pre-ADR-0015 surface
(`config/sources.toml`) is now narrowed to two demo fixtures
(`csv_demo`, `json_demo`) and contains no Reuters entries.

**Where Reuters still appears.** Three places, each non-load-bearing:
- `crates/storage/src/{assertions,documents,events}.rs` test
  fixtures — synthetic test data that exercises the storage layer's
  serialization shapes. Not real candidate-source data.
- `config/prompts/research_classifier.md` — three prose mentions
  of "Reuters" as an *example* of a general-news source the LLM
  might consider. The handoff explicitly forbids editing the L1
  classifier prompt as part of Session 45 ("Edit the L1 classifier
  prompt. Not in scope.") — and the prose is descriptive, not
  routing, so dropping it would be cosmetic.

The principle the handoff named ("don't write `if host ==
\"reuters.com\" do X.`") is preserved by the architecture itself:
sources that fail are excluded from memory; the LLM is free to
nominate them anyway and the per-host backoff layer (D-3 below)
will adapt if Reuters URLs continue to fail.

### Sub-piece D-3 — Per-host backoff

**The pre-Session-45 gap.** `fetch_backoff.rs`'s policy was
*per-request*: a 429 with `Retry-After ≤ 60s` triggered an inline
sleep-and-retry, anything else surfaced. It worked for the single-
request decision but left the system blind across requests:
- A host that 429'd the prefetch was hit again immediately by the
  runtime fetch, with no learning between the two.
- A host with pathological timeouts (the `industry.gov.au`
  observation — 300s timeouts, possibly a geo-block) tied up the
  executor for the full configured timeout on every recipe.

**The new layer.** `BackoffFetcher` is a decorator over any
`HttpFetcher` that records observed signals into a per-host state
map (`HostBackoff`). On every request the decorator does two things
beyond the underlying fetch:

1. **Pre-flight.** Read `next_allowed_at` for the URL's host. If
   the future, sleep until then before issuing the request.
2. **Post-flight.** Inspect the result. On success, reset the
   host's failure counter. On 429 with `Retry-After`, push
   `next_allowed_at` to `now + retry_after`. On 429 without
   `Retry-After` or a `Timeout`, push to
   `now + exponential_backoff(failures)` per the uniform schedule.

The schedule is `1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s, …` capped at
`HOST_BACKOFF_MAX_SECS = 60`. A host that sustains failures
stabilises at one request per minute rather than diverging.

**The host string is a runtime key.** The map is keyed on the
parsed URL host (lowercased for case-insensitive dedup). No host
appears in code, prompt, or config — adding `industry.gov.au` to
the map happens the first time a request hits it. Removing it
happens the first time a request to it succeeds (failure counter
resets to 0; the entry remains but is functionally inert until the
next failure).

**No `[per_host."<hostname>"]` table.** Backoff parameters are
uniform constants (`HOST_BACKOFF_BASE_SECS`, `HOST_BACKOFF_MAX_SECS`).
Per-host *behaviour* is derived entirely from per-host *signals* —
the tuning knob is the schedule shape, applied identically to every
host. A host that needs different parameters is the operator
signalling that the architecture's reaction to observed signals is
wrong, not that we need a special case for that host.

**Lifetime.** `HostBackoff` lives in `AppState` so observed signals
carry across `run_fetch_for_plan` calls within a session — a 429
during prefetch slows down the runtime fetch on the same host, and
sustained failures on one plan inform the next plan's fetches.
Persistence across binary restarts is not implemented; today's
failure modes are short-lived (rate-limit windows, transient
timeouts) so a fresh start on each restart is the right default.

**Why a decorator, not state on `SecureHttpClient`.** The handoff's
"three distinct surfaces" sentence reads naturally with the
backoff state at the *fetch client* layer (the `HttpFetcher` trait
in pipeline) rather than the secure-crate HTTP client. Two reasons
for the placement to be at pipeline:
1. The LLM provider (the only other significant `SecureHttpClient`
   user) hits exactly one host (xai or anthropic). Per-host
   adaptation there has no meaning.
2. Backoff signals (429, Timeout, success) are pipeline-domain
   concerns; `SecureHttpClient` already exposes the typed errors
   and the pipeline layer's `BackoffFetcher` is where the policy
   reaction lives.

The decorator pattern means the production wrapper is constructed
at one boot site (`run_fetch_for_plan` in `crates/api/src/commands.rs`)
and tests can swap it for a bare `StaticFetcher` when they want to
assert on unadapted signals.

## No source-specific routing anywhere

The audit the operator applied at end-of-Session-44
(`are we again hardcoding sources?`) holds for every change in this
patch:

- **D-1 UA default.** Generic project identifier from
  `CARGO_PKG_VERSION` and `CARGO_PKG_REPOSITORY`. No SEC-specific
  string. Per-fetcher override is the existing struct field;
  callers are caller-shaped, not source-shaped.
- **D-2 Reuters.** No code change. The architecture already
  excludes failing sources from memory; no `if host` branch was
  added or removed.
- **D-3 backoff.** Map keyed on the host string at runtime;
  parameters uniform; no `match host` on URL or response. The
  closed `FetchError` shapes (`RateLimited`, `Timeout`, `Http`,
  `NoFixture`) drive the policy; no host-specific arm.

## Tests

New tests across three files:

**`crates/secure/src/http.rs` (2 tests)** —
- `default_user_agent_is_build_time_identifier_session_45` — pins
  the shape of the default UA (`SituationRoom/` prefix,
  parenthesised `+contact` token, non-empty version). Asserts on
  shape rather than literal string so a workspace `version` bump
  doesn't need a parallel edit.
- `user_agent_override_threads_through_config_session_45` — pins
  that direct field assignment on `SecureHttpConfig::user_agent`
  produces a working `SecureHttpClient::new`. Acts as the contract
  for any future "let's hide the field behind a builder" PR.

**`crates/pipeline/src/http_fetcher.rs` (2 tests)** —
- `timeout_lifts_to_typed_variant_session_45` — `From<HttpError>`
  preserves `Timeout(Duration)` rather than collapsing to
  `Http(String)`.
- `other_http_errors_still_collapse_to_http_variant_session_45` —
  the catch-all behaviour for non-Timeout, non-429 shapes is
  unchanged.

**`crates/pipeline/src/fetch_backoff.rs` (16 tests)** —

Pure-policy `HostBackoff` tests:
- `host_backoff_starts_with_no_wait_for_unknown_host_session_45`
- `host_backoff_429_with_retry_after_pushes_next_allowed_session_45`
- `host_backoff_429_without_retry_after_uses_exponential_schedule_session_45`
- `host_backoff_timeout_uses_same_schedule_as_429_no_header_session_45`
- `host_backoff_success_resets_failure_counter_session_45`
- `host_backoff_state_is_keyed_per_host_not_global_session_45`
- `host_backoff_caps_at_max_secs_session_45`
- `host_backoff_lowercases_host_string_for_dedup_session_45`
- `host_of_returns_empty_for_unparseable_url_session_45`

Schedule primitive tests:
- `exponential_backoff_zero_failures_is_zero_session_45`
- `exponential_backoff_schedule_doubles_session_45` — pins the
  exact schedule values (1, 2, 4, 8, 16, 32, 60, 60, …).

`BackoffFetcher` decorator integration tests:
- `backoff_fetcher_passes_through_successful_fetch_session_45` —
  also stands in for the success-path recording check (no failure
  recorded → counter stays at 0; pre_flight_wait stays ZERO).
- `backoff_fetcher_records_429_into_host_state_session_45`
- `backoff_fetcher_records_timeout_into_host_state_session_45`
- `backoff_fetcher_does_not_block_unknown_host_session_45`
- `backoff_fetcher_threads_content_type_through_decorator_session_45`
  (Session 32 regression-pin — `Content-Type` survives the
  decorator).

A decorator-level "round-trip resets a prior failure" test was
considered and dropped: it would need either (a) a 1s wall-clock
sleep through the decorator's pre-flight, or (b) tokio's
`test-util` feature for `start_paused` (not enabled on the
workspace `tokio` dep). The two halves are pinned separately at
zero cost — see the inline note next to the dropped test in the
fetch_backoff tests module.

Existing tests are unchanged in behaviour. The pre-Session-45
`fetch_with_backoff` tests continue to pass — the function
signature is unchanged; the decorator lives underneath, not
alongside.

## What to expect

**Cold start (first-time fetches).** No behaviour change.
`HostBackoff` starts empty; every host's first request goes through
the decorator with `Duration::ZERO` pre-flight wait.

**SEC EDGAR fetches.** Pre-Session-45 those 403'd silently
(empty UA). Post-Session-45 they succeed (the default UA names the
project and links to the repo).

**Hosts that 429.** First 429 sets `next_allowed_at` to either the
honored `Retry-After` value or `now + 1s`. Subsequent requests to
the same host wait until then before issuing. Recovery is
automatic on the first successful response.

**Hosts that time out.** Same shape as 429-without-header. The
typed `Timeout(Duration)` variant lets the layer distinguish
timeouts from generic failures; the recipe-failure message names
the timeout explicitly ("timed out after 300s") rather than the
generic "fetch failed" string.

**Long-running operators.** Per-host state survives across
`run_fetch_for_plan` calls within a session. If a host gets
throttled during prefetch, the runtime fetch on the same host
inherits the cooldown — no second 429.

## Out of scope (still / carried)

- An explicit `[TOC]`/outline block in PDF prefetch — see
  `SESSION_44_PATCH_1.md` "Architecture / Why no explicit outline
  parsing" for why the implicit version shipped first.
- xAI Responses API migration — only architecturally necessary if a
  live `grok-4.3` run shows chat/completions silently ignoring the
  `reasoning_effort` parameter Session 43 patch 1 plumbed.
- Persistence of `HostBackoff` state across binary restarts —
  today's failure modes are short-lived enough that fresh-on-restart
  is the right default. If a future session shows hosts with
  multi-day rate-limit windows, persisting the state to a small
  derived table is the next move; the pure-policy `HostBackoff`
  type already serializes cleanly.
- Editing the L1 classifier prompt to drop the Reuters mentions —
  hard rule per the Session 45 handoff. The prose is descriptive,
  not routing; the architecture excludes failing sources from
  candidate memory regardless of what the prompt names.
- A CLI-shipped `BackoffFetcher` wrapper. The CLI binary
  (`apps/situation_room`) is the classifier-only path — it does not
  call `run_fetch_for_plan` and does not need the per-host layer.
  When and if the CLI grows fetch capability, the wrapper construction
  pattern from `crates/api/src/commands.rs` is the template.
