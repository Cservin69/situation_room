# Session 57 — Handoff

Session 57 went deeper than the prompt question Session 56 left open. The
session set out to build the eval-harness (the variance-aware measurement
infrastructure Session 56's records-0/1/1/2/3 finding had demanded), and
ended up landing **ADR 0017 + Pieces A, B, and C in one coordinated
change**: a closed-vocabulary fetch-outcome class system that the URL
proposer reads and routes on, plus a data-source-first bias for
structured-expectation queries. Both signals (records + wall-clock)
moved on the eval-harness gate; the variance went to zero.

This handoff covers what landed, what the new bound is, and what
Session 58 should pick up next.

## What landed (single coordinated commit)

### 1. Eval harness — `apps/eval_harness/`

Non-Tauri composition root that runs N independent `(classify → accept
→ run_fetch_for_plan)` trials for one topic and writes JSONL metrics.
Per-trial isolated DuckDB + per-trial fresh `HostBackoff` so cross-trial
state can't contaminate the variance measurement.

Default invocation:

```
cargo run -p situation_room-eval-harness --release -- \
  --topic "lithium supply chain" --trials 5
```

Output:
- JSONL: `eval-runs/<topic-slug>-<timestamp>.jsonl` (one line per trial)
- Stderr summary: records mean / stddev / range, wall_clock mean

Default logging is quiet — only the per-trial summary lines from the
harness itself, error-level only from the pipeline. Override with
`RUST_LOG=situation_room=info,warn` for the desktop-binary firehose.

### 2. Host probe — `apps/eval_harness/src/bin/host_probe.rs`

Diagnostic binary that hits each URL with four UA strings using the
production `SecureHttpClient`. TSV output (status × UA × URL) drives
populated host-class overrides in `fetch_classes.rs`.

The Session 57 probe (`docs/observations/2026-05-10-host-probe.tsv`)
discriminated all four hypotheses cleanly. Key findings:
- Reuters: 401 every UA → paywall, confirmed.
- Bloomberg: 403 every UA → WAF beyond UA, confirmed (even chrome_macos
  blocked).
- SEC `www.sec.gov/edgar/search/`: 403 every UA → SPA URL + UA block.
- SEC `efts.sec.gov` and `data.sec.gov`: 403 for our UAs, **200 (60–160 KB
  JSON) for chrome_macos** → primary-source data exists, but only
  reachable with a browser-class UA we don't ship (ADR 0009 territory).
- Fastmarkets, Gallup, Yonhap, IEA reports: hallucinated subpaths /
  SPA shims / wrong URL shape — host fine, proposer's path wrong.
- Realmeter, industry.gov.au: every UA timeout → unreachable.

**The SEC email-UA hypothesis is falsified.** Adding `contact@…` to our
UA did not unblock any sec.gov-class host. The published fair-access doc
overstates what the policy enforcer accepts.

### 3. ADR 0017 — `docs/adr/0017-fetcher-capability-and-proposer-repertoire.md`

Articulates the four-bucket decomposition of the "coverage-publisher
block wall" pattern (paywall, WAF beyond UA, UA-policy, hallucinated
path) and explicitly rejects five easy-wins by name. Prescribes a
three-piece structural change measured against an eval-harness gate.

### 4. Piece A — `crates/pipeline/src/fetch_classes.rs`

Closed-vocabulary `FetchOutcomeClass` enum + classifier function +
initially-empty host-override map. Seven variants:

- `Ok`
- `HostUnreachable` (DNS, TLS, connect-refuse, timeout, 5xx)
- `HostBlockedByWaf` (403 from a host structurally unreachable for us)
- `HostRequiresAuth` (401 — paywall)
- `HostRequiresUaPolicy` (vestigial — no host populates it per probe)
- `UrlShapeMismatch` (400/404/410 or "fetched but no extractable
  structure" or "recipe authored but apply failed")
- `RateLimited`

Default policy: 401 → `HostRequiresAuth`, 403 → `HostBlockedByWaf`,
400/404/410/other-4xx → `UrlShapeMismatch`, 5xx/timeout/DNS/TLS →
`HostUnreachable`, 429 → `RateLimited`. `HOST_CLASS_OVERRIDES` map is
empty and locked-empty by a unit test until probe evidence justifies
entries.

21 unit tests cover every branch including suffix-anchored override
semantics, case-insensitivity, and "override fires only on 403".

### 5. Piece B — class-aware proposer routing

- `crates/pipeline/src/propose_source_url.rs`: `PriorAttempt` gains a
  `class: FetchOutcomeClass` field; `render_prior_attempts` emits a
  dedicated `Class: <label>` line above the reason line.
- `crates/pipeline/src/fetch_executor.rs`: three `PriorAttempt`
  construction sites populate `class`:
  - Apply-failure (cross-run, from `apply_failures_for_nomination`) →
    `UrlShapeMismatch` (URL was responsive, content shape didn't match).
  - Prefetch-failure → `PrefetchFailure::class(host)` which routes
    through `fetch_classes::classify_error`.
  - No-target-authored (fetch worked, recipe author declined every
    target) → `UrlShapeMismatch`.
- New `PrefetchFailure::class(&self, host: &str) -> FetchOutcomeClass`
  method delegates to `fetch_classes` — single classification path.
- `crates/pipeline/src/fetch_backoff.rs`: `host_of` promoted to
  `pub(crate)` so the executor's prefetch classification uses the same
  host-extraction helper as the per-host backoff layer.
- `config/prompts/propose_source_url.md` v1.5: new "Class-based routing"
  subsection at the top of "Reading prior attempts" mapping each class
  to a routing shape (pivot host vs. pivot URL on same host vs. wait
  for backoff). Free-text reason rules kept as within-class detail.

### 6. Piece C — data-source-first bias

`config/prompts/propose_source_url.md` v1.5: new top-level section
"Machine-readable endpoints first for structured nominations" between
"What makes a good URL" and "How to weight priority_tier". Teaches the
proposer that for quantitative nominations (counts, percentages,
capacities, time-series), subdomains like `data.*`, `api.*`, `efts.*`
and direct file URLs (`.csv`, `.xlsx`, `.json`) are strictly preferable
to the publisher's main HTML site. Layered on top of priority-tier
weighting, not in place of it.

## Eval-harness gate — measured before/after

Five trials of the lithium fixture, fresh DB per trial, fresh
`HostBackoff` per trial.

| metric | baseline (before B+C) | after B+C | delta |
|---|---|---|---|
| records mean | 1.60 | **2.00** | +25% |
| records min | 0 | 2 | floor lifted off zero |
| records max | 3 | 2 | ceiling capped |
| records stddev | 1.02 | **0.00** | **variance collapsed entirely** |
| wall_clock mean | 164.6 s | **141.2 s** | −14% |
| wall_clock range | — | 105.5–195.4 s | proposer-path-dependent |
| trial_errors | 0 | 0 | unchanged |

JSONL files:
- baseline: `eval-runs/lithium-supply-chain-20260510T183233Z.jsonl`
- after: `eval-runs/lithium-supply-chain-20260511T0637...jsonl` (timestamp
  varies by run)

The **variance collapse to zero** is the structural finding. Every
trial picked the same URLs in the same order and produced exactly 2
records (USGS production + USGS reserves). The 0-record and 3-record
outliers from the baseline distribution are gone. Session 56's
complaint was variance, not mean; that complaint is resolved on this
fixture.

Concrete evidence the proposer is reading the class labels (extracted
from JSONL decline reasons):
- *"Prior attempt on sec.gov returned **host_blocked_by_waf** (403). Per
  class-based routing, pivot off-host…"*
- *"Both obvious Fastmarkets lithium paths returned 404
  (**url_shape_mismatch**). No other known non-fabricated URL…"*
- *"Prior attempt on the named host timed out (**host_unreachable**).
  Per class-based routing, pivot off-host rather than retry…"*
- *"No additional credible **machine-readable** or focused chapter
  URLs on the IEA host are known without fabrication"* (Piece C
  vocabulary appearing in declines)
- *"No other known non-SPA, **server-rendered listing** or feed URL on
  fastmarkets.com matches"* (Piece C vocabulary)

The proposer is no longer parsing free text; it reads the closed-
vocabulary class and applies the routing rule. **Piece B is working as
designed.** Piece C's machine-readable language also appears in
declines, indicating the proposer is reasoning about machine-readable
shapes even when it can't find one.

## The new bound

With Pieces B+C landed, the records-per-trial number stops being limited
by **proposer decision quality** and starts being limited by two
separate problems:

### Problem 1 — Recipe-author selector quality (recurring pattern)

When the proposer picks a URL that *does* contain the right data shape,
the recipe author still sometimes picks container-sized selectors
instead of leaf cells. The 2026-05-11 run shows three concrete cases:

- `mining.com/tag/lithium/`: selector matched a 15,002-byte iframe
  container; runtime cap is 2,048 bytes per scalar.
- `worldbank.org/en/research/commodity-markets`: selector matched a
  21,601-byte JavaScript blob; same cap exceeded.
- `iea.org/reports/global-ev-outlook-2024`: selector matched a
  112,002-byte iframe container; same cap exceeded.

The recipe-author's authoring-time validator catches the size violation
and declines, so no broken recipe lands — but the URL was responsive
and the proposer picked correctly; the recipe author just couldn't
write a good selector against the page shape.

This is its own ADR (territory: ADR 0007 amendment, or a new ADR for
recipe-author quality). Not a quick prompt tweak — the failure shape
("selector returns container, not leaf") is what the closed-vocabulary
discipline was *supposed* to prevent and the authoring-time validator
catches downstream, but the recipe author is producing too many
candidate selectors that fail validation. Investigation: is the
recipe-author prompt over-encouraging "broad first" selectors? Is the
prefetched excerpt's structure being summarised in a way that misleads
the author? Is there a per-format (HTML vs. PDF vs. JSON) quality gap?

### Problem 2 — `data.sec.gov` / `efts.sec.gov` reachable only with browser UA

The 2026-05-10 probe showed both SEC API subdomains return well-formed
JSON when fetched with chrome_macos UA but 403 to our `SituationRoom/…`
UA. Adding a per-host browser-UA capability to the fetcher would unlock
a class of authoritative_primary data source (SEC filings, plus by
extension other publishers that follow the same pattern).

This is **ADR 0009 territory** — that ADR deliberately keeps us on a
scrutinisable identifying UA. Revisiting it requires its own ADR
amendment with a clear scope (e.g. "per-host browser-UA override on a
small allowlist, justified by probe evidence; full residential-proxy
adapters remain out of scope"). The Session 57 ADR 0017 explicitly
lists this in its "Out of scope" §6.

## Recommended Session 58 order

The eval-harness gate is now standing infrastructure; any future patch
that touches the fetch path or the proposer prompt should land paired
with a ≥5-trial before/after run.

Top of the queue, in order:

1. **Recipe-author selector quality** (highest leverage). The same
   "container instead of leaf" failure shape recurred across three
   different hosts in a single trial. Root-cause it: is the prompt over-
   encouraging broad selectors? Are the per-format prefetch summaries
   misleading the author? Start with reading 3-5 actual failing
   selectors from the post-Session-57 JSONL alongside the recipe-author
   prompt and forming a hypothesis. If the diagnosis is a prompt-tweak,
   the eval-harness gate applies (5 trials before/after). If the
   diagnosis is structural (e.g. the closed-vocabulary modes don't
   cover a shape that's common in HTML), it's an ADR.

2. **Per-host timeout backoff** (carried from Session 55/56). The
   `industry.gov.au` 60s × 1 timeout floor is still present in every
   trial. The harness wall-clock variance (105–195 s) is largely
   explained by which trials happened to hit this host. ADR-territory:
   should `HostBackoff` widen its window for hosts in `HostUnreachable`
   class, or should the proposer pivot off `host_unreachable` faster
   (it sometimes already does — see the single-attempt declines).

3. **ADR 0009 revisit** for `data.sec.gov`-class endpoints. Bounded
   scope: per-host browser-UA override on a small allowlist, justified
   by probe evidence. Not a residential-proxy adapter. Likely cohesive
   with a host-class-capability registry that lives alongside
   `HOST_CLASS_OVERRIDES`.

4. **Patch 3E** (Yahoo-Finance ALB spot_price misroute, carried from
   Sessions 54–56). The class-aware routing in Piece B may have changed
   how often this pattern surfaces; re-evaluate against the new
   baseline before designing.

5. **Anti-example → gold-standard prompt surgery** (Deferred A from
   Session 55). Lowest priority now that the structural change has
   landed; prompt-only tweaks are still inside the variance band for
   any sufficiently surprising fixture.

## What did NOT land (carried forward)

- ADR 0009 amendment for browser-UA endpoints — see §3 above.
- Recipe-author selector quality ADR — see §1 above.
- Per-host timeout backoff — see §2 above.
- Patch 3E — see §4 above.

## Discipline carryover

- "No easy wins" — Session 57's three pieces deliberately did not ship
  the obvious UA-tweak fix; the probe proved that fix was empty. The
  next sessions should hold the same line: when a finding has a shallow
  patch and a deeper diagnosis, prefer the deeper one. The recipe-
  author selector-quality issue (§1 above) is the canonical example for
  Session 58 — the shallow patch is "add anti-examples to the prompt";
  the deeper diagnosis is the structural question above.
- ADR-territory items get an ADR, not a one-line patch.
- Eval-harness ≥5 trials before/after for any patch that touches fetch
  path or proposer prompt.
- Closed-vocabulary discipline: hosts appear only in `HOST_CLASS_OVERRIDES`
  (currently empty); no host strings in prompts, no host strings in
  classifier output. The proposer reads classes, the classifier
  reasons about hosts, the boundary is the override map.

## Reset targets

Each piece is its own commit. Reset ladder:

- Piece C only (prompt rollback to v1.4 + revert the "Machine-readable
  endpoints" section): cheapest rollback, leaves Piece A+B in place.
  Would re-introduce coverage-publisher bias for structured-expectation
  queries.
- Piece B (revert `PriorAttempt::class` field + executor wiring + the
  prompt's "Class-based routing" subsection): mid-cost rollback.
  Leaves Piece A in place as dead-code telemetry surface.
- Pieces A + B + C: full rollback to pre-Session-57. Loses the
  measurement gain too (eval-harness baseline 0/1/1/2/3 returns).

No reason to roll back any of the three — the measured eval-harness
gate passed on both signals.

## File index

New:
- `apps/eval_harness/Cargo.toml`
- `apps/eval_harness/src/main.rs`
- `apps/eval_harness/src/bin/host_probe.rs`
- `docs/adr/0017-fetcher-capability-and-proposer-repertoire.md`
- `docs/observations/2026-05-10-host-probe.tsv`
- `crates/pipeline/src/fetch_classes.rs`
- `SESSION_57_HANDOFF.md` (this file)

Edited:
- `Cargo.toml` (workspace member)
- `.gitignore` (`eval-runs/`)
- `crates/pipeline/src/lib.rs` (module decl)
- `crates/pipeline/src/propose_source_url.rs` (PriorAttempt + render + tests)
- `crates/pipeline/src/fetch_executor.rs` (PriorAttempt construction sites + PrefetchFailure::class + imports)
- `crates/pipeline/src/fetch_backoff.rs` (host_of visibility)
- `config/prompts/propose_source_url.md` (v1.4 → v1.5: Class-based routing + Machine-readable endpoints sections)

Memory updated:
- `feedback_no_easy_wins.md` (new)
- `project_sr_session_57_verification.md` (new, see memory dir)
- `MEMORY.md` index
