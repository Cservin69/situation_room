# ADR 0017 — Fetcher capability and proposer repertoire: the coverage-publisher block wall

**Status**: Accepted (probe data 2026-05-10; Piece A landed Session 57; Pieces B + C land in the same session)
**Date**: 2026-05-10
**Related**: ADR 0007 (research function: two-level LLM architecture
and the closed-extraction-vocabulary discipline), ADR 0009
(security posture: SecureHttpClient, no-bypass rule), ADR 0011
(plan lifecycle and fetch executor), ADR 0012 (re-author on
failure), ADR 0014 (stub-authored recipe provenance), ADR 0015
(LLM-emitted source nominations), ADR 0016 (extraction iterator)

---

## Context

Session 56 ran 5 lithium trials and observed records 0/1/1/2/3 —
per-trial variance dominated the prompt-version effect we were
trying to measure. Session 57's first lithium re-run produced 2
records; its concurrent south-korea run produced 19 (one nomination
ate the page) but five of the seven nominations declined. Across
both runs a single pattern accounts for nearly every decline that
isn't a per-target content judgement: **the URL the proposer picked
was either unfetchable or, when fetched, did not contain
extractable data of the shape the plan asked for.**

Examples from the lithium run on 2026-05-10 15:57 (logged in
Session 57):

| Host                  | Status                | Plan nomination          |
|-----------------------|-----------------------|--------------------------|
| reuters.com           | 401 (twice)           | "Reuters commodities desk reporting" |
| bloomberg.com         | 403                   | (proposer pivot from SEC EDGAR fail)  |
| sec.gov/edgar/search  | 403                   | "SEC EDGAR filings of listed lithium producers" |
| fastmarkets.com (×2)  | 404 (×2)              | "Fastmarkets battery-raw-materials price assessments" |
| iea.org/***2024       | 404                   | "IEA Critical Minerals Outlook" |
| iea.org/***2023       | 404                   | (proposer pivot)        |
| realmeter.co.kr       | timeout 60s           | (south-korea run)        |
| industry.gov.au/p…    | timeout 60s           | "Australian Office of the Chief Economist Resources and Energy Quarterly" |

These look superficially like one phenomenon ("major coverage
publishers don't work for us") but a closer read shows **at least
four distinct underlying causes**, each of which would respond to a
different fix. Treating them as one phenomenon is the easy-win
trap: a single UA tweak silently papers over three of the four
without ever addressing them, while we tell ourselves we've fixed
the coverage problem.

### The four-way decomposition

#### 1. Paywall (auth-required content)

Reuters returns **401**, not 403. 401 specifically means
"authentication required, you have not provided valid credentials."
Reuters Connect is the paid API; reuters.com tag pages and topic
listings are paywall-metered and return 401 to anonymous fetchers
*by design*. No User-Agent change is going to fix this — the host
isn't deciding based on UA, it's deciding based on session/cookie
auth state, and we have no auth state.

The right response is *not* to keep proposing reuters.com URLs and
fail at fetch time. It is either to stop proposing them, or to
propose them only as the trigger for a documented "this needs
auth" decline that doesn't burn a fetch attempt.

#### 2. Bot/WAF block on non-residential IP

Bloomberg and other Cloudflare-fronted hosts return **403** — a
"forbidden, no further negotiation" code. Bloomberg's WAF is
known to fingerprint several signals beyond UA: HTTP/2 settings
frame ordering, TLS ClientHello shape (JA3), IP reputation class
(residential vs. datacenter), and rate of arrival. A browser-
shaped UA may unblock some of these hosts; for Bloomberg
specifically, it almost certainly will not. Our `SituationRoom/…`
UA is identifying us as a bot, but our datacenter IP and TLS
fingerprint already gave us away — UA is the most visible signal,
not the only one.

The right response is *not* to fight a fingerprinting WAF on its
home turf. It is to recognise that this class of host is
unreachable without a residential proxy or a documented
partnership, and to bias the proposer away from it accordingly.

#### 3. UA-policy enforcement

SEC EDGAR returns **403** when the UA does not include a
contact email per the [published fair-access
policy](https://www.sec.gov/search-filings/edgar-search-assistance/accessing-edgar-data).
Our `SituationRoom/0.1.0 (+https://github.com/…)` identifies us
but does not include the email-shaped suffix the policy enforcer
checks for. This is the one bucket where a UA tweak directly fixes
the problem — but only for SEC; the same change would not help
Reuters or Bloomberg.

Critically, the proposer is *also* contributing to SEC failure by
proposing the SPA URL `https://www.sec.gov/edgar/search/#/forms=10-K,10-Q&entityName=Albemarle`.
The `#/...` fragment is client-side; the server sees only
`/edgar/search/` and has no way to scope to "Albemarle 10-K". Even
with the right UA, this URL shape is wrong for the question. The
machine-fetchable endpoint is `https://efts.sec.gov/LATEST/search-index?q=…&forms=…&entityName=…`
or `https://data.sec.gov/submissions/CIK<10-digit>.json`. The
proposer does not know about either.

#### 4. Hallucinated URL

Fastmarkets `/lithium/` and `/prices/` both 404. IEA Critical
Minerals Outlook 2024 path returns 404. Gallup Korea report-list
endpoint returns 404. Yonhap politics paths return 400/404. The
proposer is generating plausible-shaped paths that do not exist on
the host. The "don't guess" rule eventually fires and declines
the nomination, but only after burning attempts.

This is not a fetcher problem. It is a proposer-knowledge-gap
problem. UA is irrelevant; the URL never had a chance.

### What the four buckets share

All four buckets reach the proposer because the proposer's mental
model of "where to find authoritative information about X" is
biased toward **coverage publishers** — Reuters, Bloomberg,
Fastmarkets, Gallup. These are the venues a human researcher
might cite, but they share three properties that make them the
worst possible fit for our fetcher:

1. They monetise access (paywall or paid API).
2. They are aggressively WAF-defended.
3. Their machine endpoints are different from their human-facing
   URLs (and the proposer only knows the human-facing ones).

Meanwhile, **primary-source / data-publisher hosts** that DO work
end-to-end — USGS, KOSIS, NEC, Korea Times, World Bank docs (XLSX
endpoints), data.sec.gov — are reached only by accident or as
late-pivot fallbacks. The proposer's first guess for a lithium
plan was Fastmarkets (404) before USGS (success on a different
nomination); for an SK-elections plan, Realmeter (timeout) before
the working koreatimes.co.kr listing.

The structural problem is not "we cannot fetch Reuters." It is
"the proposer's repertoire is biased toward hosts our fetcher is
structurally unable to reach, when there exist other classes of
host that work and that the proposer underweights."

## Diagnostic state

The diagnostic probe ran on 2026-05-10. The full TSV lives at
`docs/observations/2026-05-10-host-probe.tsv` (40 rows: 10 URLs ×
4 UAs each). Cross-tabulating UA × status discriminates the four
hypothesised buckets cleanly. Headline findings:

| Host                                              | default_sr | with_email | googlebot | chrome_macos | Confirmed bucket                                  |
|---------------------------------------------------|------------|------------|-----------|--------------|---------------------------------------------------|
| `www.reuters.com/markets/commodities/`            | 401        | 401        | 401       | 401          | **1 — paywall**                                    |
| `www.bloomberg.com/topics/lithium`                | 403        | 403        | 403       | 403          | **2 — WAF beyond UA** (Cloudflare; even browser UA blocked) |
| `www.sec.gov/edgar/search/`                       | 403        | 403        | 403       | 403          | **2 — WAF / SPA URL** (every UA blocked)           |
| `efts.sec.gov/LATEST/search-index?q=…&forms=…`    | 403        | 403        | 403       | **200 (60 KB JSON)** | **2.5 — needs browser UA** (new finding)   |
| `data.sec.gov/submissions/CIK*.json`              | 403        | 403        | 403       | **200 (160 KB JSON)** | **2.5 — needs browser UA** (new finding)  |
| `www.fastmarkets.com/`                            | 200        | 200        | 200       | 200          | **4 — host fine, paths hallucinated** (proposer guesses dead subpaths) |
| `www.gallup.co.kr/gallupReport/gallupReportList.do` | 200 (1.2 KB) | 200 | 200 | 200          | **4 — SPA shim** (200 but body is JS loader, not data) |
| `en.yna.co.kr/politics`                           | 400        | 400        | 400       | 400          | **4 — URL shape** (host's router rejects path)     |
| `www.realmeter.co.kr/`                            | timeout    | timeout    | timeout   | timeout      | **HostUnreachable** (slow/geofenced)               |
| `www.industry.gov.au/publications/…`              | timeout    | timeout    | timeout   | timeout      | **HostUnreachable** (slow/geofenced)               |

### Hypothesis updates from the data

**Hypothesis 1 — Paywall (Reuters): CONFIRMED.** Every UA, including
chrome_macos, returns 401. Reuters is genuinely auth-walled at the
network layer. No UA tweak helps.

**Hypothesis 2 — WAF beyond UA (Bloomberg): CONFIRMED.** Every UA,
including chrome_macos, returns 403. Bloomberg's WAF fingerprints
beyond UA — IP class, TLS ClientHello shape, or HTTP/2 settings.
The "switch to a browser UA" easy win would not unblock this host.

**Hypothesis 3 — UA-policy enforcement (SEC needs email): FALSIFIED.**
The published SEC fair-access doc says a contact email in the UA
is required. Our `situation_room_with_email` row tests that exact
remedy. **It does not work.** SEC's actual filter is stricter than
the docs claim — every URL on every sec.gov-class host returns 403
to default-, email-, and googlebot-shaped UAs, and only chrome_macos
passes (and only on the *API* subdomains `efts.sec.gov` and
`data.sec.gov`; the SPA URL `www.sec.gov/edgar/search/` is 403
even with chrome_macos). This is the most important finding from
the probe: **adding an email to our UA, the canonical "easy SEC fix",
would have moved no records.** The `HostRequiresUaPolicy` enum
variant survives in `fetch_classes.rs` as a shape ready for hosts
that *do* respond to a doc-prescribed UA fix, but `HOST_CLASS_OVERRIDES`
records no such hosts as of the 2026-05-10 probe.

**Hypothesis 4 — Hallucinated URLs: CONFIRMED, with refinement.**
Fastmarkets `/` returns 200 to every UA. The 404s in production
runs are because the proposer keeps picking *subpaths* (`/lithium/`,
`/commodities/battery-raw-materials/`, `/price-reports/lithium`)
that do not exist. Same shape for Gallup Korea: the report-list URL
returns 200 but the body is a 1.2 KB JS loader, not the report data.
This is a proposer-knowledge gap, not a fetcher gap, and is the
target of Piece C below.

**New finding — SEC has primary-source JSON APIs that work.** The
probe surfaced two SEC endpoints not previously on the proposer's
radar: `efts.sec.gov/LATEST/search-index` (full-text filing search)
and `data.sec.gov/submissions/CIK*.json` (per-company filing index).
Both return well-formed JSON when fetched with chrome_macos. They
are 403 with our `SituationRoom/...` UA, so they are de facto
unfetchable for our production binary today *(unless we revisit
ADR 0009's identification posture, which is out of scope for this
ADR — see "Implementation order" §6 below)*. The actionable point
is not the UA decision; it is that **the proposer's mental model
of "where SEC filings live" maps to `www.sec.gov/edgar/search/`
(SPA URL, unfetchable) when the right shape is `data.sec.gov` or
`efts.sec.gov` (machine-readable, structured).** Generalising
this: for many publishers there is a class-level distinction
between the human-facing HTML site (often SPA, often WAF-defended)
and a machine-readable API subdomain (often `data.*`, `api.*`,
`efts.*`, or a documented `/api/` path). The proposer should know
to prefer the latter for structured-expectation queries. This is
the new content of Piece C.

## Decision (proposed)

The structural fix is a **three-piece coordinated change**, no
single piece of which is sufficient on its own. None of these
pieces is a UA-tweak fire-and-forget; the easy-win path is
explicitly rejected as the only response.

### Piece A — Surface a host-capability vocabulary

Today the fetcher has a single capability shape: "do a GET, with
SecureHttpClient defaults, and report status." The recipe author
and the URL proposer have no language for the difference between
"reuters.com 401 means authentication required, do not retry" and
"industry.gov.au timeout means the host is slow, retry with
patience." Both surface as "fetch failed" and the next attempt
proposes a similar URL.

We introduce a closed enum of fetch outcomes that the proposer can
reason about:

```rust
enum FetchOutcomeClass {
    Ok,
    HostUnreachable,        // DNS, TLS, connect-refuse, timeout
    HostBlockedByWaf,       // 403 from a host known to fingerprint
                            //  beyond UA — confirmed by probe
    HostRequiresAuth,       // 401 — paywall, no point retrying
                            //  the same host without credentials
    HostRequiresUaPolicy,   // 403 from a host where adding the
                            //  email-shaped UA fixes it (SEC class)
    UrlShapeMismatch,       // 404 — endpoint doesn't exist; the
                            //  problem is the path, not the host
    RateLimited,            // already exists; preserve
}
```

The classification is the fetcher's job, not the proposer's. The
fetcher applies a small lookup that maps `(host, status)` pairs to
the class. The lookup is **closed-vocabulary** — no host strings in
the proposer prompt, no host strings in classifier output. The
host→class map lives in one place (`crates/pipeline/src/fetch_classes.rs`)
and is the single bake-in of host-specific knowledge in the
codebase.

[probe-confirms] The exact host→class entries depend on probe
results. We commit only to entries the probe verifies.

### Piece B — Teach the proposer to read the outcome class

The propose-URL prompt today gets a list of `prior_attempts` with
URLs and free-text decline reasons. We extend it to receive the
`FetchOutcomeClass` for each prior attempt and add closed-
vocabulary instructions:

- `HostRequiresAuth` → do not propose another URL on the same
  host; pivot to a different host class.
- `HostBlockedByWaf` → same; this host class is structurally
  unreachable from this fetcher.
- `UrlShapeMismatch` → propose a different *shape* on the same
  host (different path layout) before pivoting hosts.
- `HostUnreachable` → propose a different host (timeouts here are
  ours to handle, not the proposer's).
- `RateLimited` → existing backoff handles this; proposer waits.

The proposer gets no host strings or domain strings — it sees
classes and decides classes. This preserves ADR 0015's
closed-vocabulary discipline and prevents the "but Reuters is the
right place to look" failure mode.

### Piece C — Bias the proposer toward primary-source hosts

The proposer prompt today has a "news-first, reasonable-shot"
heuristic (Session 53's v1.2). The heuristic was right when the
fetcher could reliably reach news, which Session 56–57 have shown
it cannot. We invert it:

- **Primary sources first** when the plan's expectation is a
  number, table, time-series, or other structured datum. USGS
  publications, government statistical agencies, World Bank /
  IMF / IEA *data products* (XLSX, CSV, JSON), regulator filings
  via machine endpoints (data.sec.gov style).
- **Coverage publishers second**, for events / announcements
  where structured data is unlikely to live anyway. Bias toward
  hosts the per-host capability map says we can reach.

This is not bake-in of specific hosts. It is a class-level rule:
"data-publisher classes before coverage-publisher classes when the
expectation is structured." The classifier already produces typed
expectations (`observation_metric`, `event_type`, etc.); the rule
keys off those types, not off any host string.

### What we explicitly do NOT do

1. **Add a browser UA and ship.** This appears to fix Bloomberg in
   isolation but does nothing for Reuters (paywall), nothing for
   Fastmarkets (hallucination), and partially regresses our
   identification posture (Session 45's build-time identifier was
   chosen to make us scrutinisable).

2. **Add SEC-specific code.** Adding the email to our UA is
   correct but it is one entry in the host→class table (Piece A),
   not a special case. Per ADR 0007's closed-vocabulary
   discipline, no SEC-specific routing in code.

3. **Add a residential-proxy adapter for Bloomberg.** This would
   trade a fetcher problem for an operational and legal one. The
   right answer is "we cannot reach Bloomberg" → bias proposer
   away.

4. **Patch the proposer prompt with another anti-example.**
   Session 56 already established that prompt-only tweaks live
   inside the variance band. Without Piece A's outcome
   classification, the proposer cannot distinguish "this host
   class is unreachable" from "this URL shape is wrong" and any
   prompt-only nudge will land on the wrong side of that line for
   half of cases.

## Alternatives considered

### Alt 1 — UA-only fix

Add `contact@…` to the UA string. **Rejected** as the sole
response: helps SEC but only SEC. The probe will quantify; no host
other than SEC is hypothesised to recover.

### Alt 2 — Browser UA

Switch the production UA to a Chrome string. **Rejected**:
violates ADR 0009's identification posture (we want to be
scrutinisable, not pretending to be a human in San Francisco), and
even on its own merits would not fix Bloomberg-class WAF blocks.
The probe will confirm the latter.

### Alt 3 — Per-host adapters with auth

Build a Reuters Connect adapter, an SEC EDGAR JSON-API adapter, an
LSEG adapter, etc. **Deferred**, not rejected: the per-host
adapter shape is the right escape valve when a host is genuinely
load-bearing for a research session. But building the registry of
adapters before we know which hosts the operator's actual research
sessions need is premature. Piece A's outcome classification is
the precondition; once a research-session class repeatedly hits
`HostRequiresAuth` for the same authoritative host, an adapter is
the next step. None of the lithium or south-korea sessions
demonstrably need one yet.

### Alt 4 — Pre-flight host classification (separate LLM step)

Add a step before propose-URL that asks the LLM "which host class
is appropriate for this expectation type?" and only then ask
propose-URL to pick a URL. **Rejected**: doubles the LLM-step
count for a per-fetch operation that should be cheap; ADR 0007's
"the runtime is LLM-free" rule rules out putting it in the runtime
path. The right home for class selection is inside the
propose-URL prompt itself (Piece C), with the proposer reading the
expectation type and applying the bias in one step.

### Alt 5 — Cache the proposer's bad guesses across sessions

Persist a "we tried this URL and it 404'd" memo and feed it back
on next-session attempts. **Deferred**: addresses bucket 4 but not
buckets 1–3 (auth state and WAF state can change), and risks
freezing the proposer's view of a host that has since become
fetchable. Worth revisiting once Pieces A–C are in.

## Consequences

### Positive

- The proposer's URL-pick stops being an opaque mass of
  401/403/404/timeout/success that varies trial-to-trial. Each
  outcome carries a class; the proposer's next move is informed
  by the class.
- Records-per-trial variance should decrease for plans whose
  expectations are structured (numbers, tables) because the
  proposer-bias toward data publishers will more often pick
  hosts that work.
- The host→class map in `fetch_classes.rs` is the single
  bake-in of host-specific knowledge, with a documented contract
  that anything outside the map gets `Ok` / `HostUnreachable` by
  default. This makes ADR-0007's closed-vocabulary discipline
  enforceable at code-review level, not just at prompt-review
  level.

### Negative

- One new code surface (`fetch_classes.rs`) that has to be
  maintained as observed host behaviours change. Mitigation: the
  map is small, every entry is justified by probe evidence, and
  changes go through code review with a probe re-run.
- The proposer prompt gets longer. Mitigation: the additions are
  closed-vocabulary instructions, not anti-examples — they
  generalise instead of accreting.
- Class-level bias toward primary sources may cause the proposer
  to under-propose news for genuinely event-type expectations
  where news is the right venue. Mitigation: the rule keys off
  expectation type, and `event_type` keeps news as a first-class
  option; only `observation_metric` shifts the bias.

### Risks the probe data flagged (resolved)

1. **Bloomberg WAF**: chrome_macos returned 403, same as the other
   three UAs. **Confirmed: WAF beyond UA.** No fix at the UA layer.
2. **SEC `data.sec.gov`**: every UA we'd ship (default, with-email,
   googlebot) returned 403. Only chrome_macos returned 200. **The
   "drop SPA URL, use JSON API" action would only work paired with
   a UA change** — and ADR 0009 deliberately keeps us on a
   scrutinisable identifying UA. So the actionable finding is
   teaching the proposer about machine-API endpoints (Piece C),
   not changing our UA. Bucket 3 stays empty in the host map.
3. **Reuters**: every UA returned 401. **Confirmed: paywall**.

The three-piece structural decision is unchanged by the data.
Piece A's class vocabulary is the right shape; the host map stays
empty; Pieces B and C are where the work is.

## Implementation order

1. ✅ Run `host-probe` against the URL list above; commit results
   under `docs/observations/2026-05-10-host-probe.tsv` and update
   this ADR with confirmed/rejected hypotheses. **Done 2026-05-10
   (Session 57).** Findings folded into the "Diagnostic state"
   section above.
2. ✅ Implement Piece A — `FetchOutcomeClass` enum +
   `fetch_classes.rs`, host map starts empty (and stays empty per
   probe findings), classifier function exercised by 21 unit
   tests. **Done 2026-05-10 (Session 57).**
3. **Implement Piece B** — extend propose-URL prompt to read prior
   attempt classes and route on them. Pair with eval-harness ≥5
   trials before/after to confirm the change moves records out of
   the variance band. **Baseline established 2026-05-10:** records
   `mean 1.60, stddev 1.02, range 0–3, wall_clock 164.6s` over 5
   lithium trials (`eval-runs/lithium-supply-chain-20260510T183233Z.jsonl`).
4. **Implement Piece C** — invert the news-first bias to data-first
   for structured expectations, with the new probe-driven
   refinement: teach the proposer that machine-API subdomains
   (`data.*`, `api.*`, `efts.*`) and direct file URLs (CSV, XLSX,
   JSON) are preferred over the publisher's main HTML site for
   structured-expectation queries. Same eval-harness gate as
   Piece B.
5. Decide on Alts 3 & 5 only if Pieces A–C don't move the
   distribution enough.
6. **Out of scope:** revisiting ADR 0009 to add a browser-UA
   fetcher path. The probe data showed `data.sec.gov` and
   `efts.sec.gov` work with chrome_macos but not with our
   identifying UA. Adding a per-host UA-override path would
   unlock these endpoints AND change our identification posture.
   ADR 0009 §"The rule" is deliberate; revisiting it requires
   its own ADR amendment, not an inline change here.

Pieces A–C are each a separate commit with a named reset target.
Pieces B and C ship together because Piece B's class vocabulary
in the proposer prompt is the precondition for Piece C's class-
level data-source bias to be addressable. None of the pieces is
shippable without the eval-harness gate.
