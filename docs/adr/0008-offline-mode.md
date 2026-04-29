# ADR 0008 — Offline mode and cache architecture

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0005 (DuckDB), ADR 0007 (research function),
ADR 0006 (design language)

## Context

Stockpile is a desktop app that pulls from internet sources. Two
realities follow from that:

1. The user is sometimes offline, or on a bad connection, or
   using the app while a source is down. The product should not
   become useless in those cases.
2. Even when online, network latency varies. Panels that wait on
   HTTP round-trips to render feel slow and break the
   information-dense scan behavior that ADR 0006 is designed for.

The architectural question is: **what's the relationship between
the network layer and the UI?**

There are two common answers:

- **Fetch-on-render.** Each panel queries the network when it
  renders. Simple, but slow, and breaks when the network is
  unavailable.
- **Cache-first.** The network layer writes into a local cache;
  the UI reads only from the cache. Panels are fast; offline
  works; freshness is a separate concern surfaced explicitly.

Stockpile picks the second.

## Decision

**DuckDB is the system of record for the UI. The UI reads only
from DuckDB. The network layer writes through into DuckDB.**

Specifically:

- Every source fetch writes its result into DuckDB (as records
  per ADR 0003, via the pipeline). The UI never sees the raw
  network response.
- Every panel query is a DuckDB query. It does not involve the
  network.
- "Freshness" is surfaced as a per-panel "last updated" timestamp,
  not as a loading spinner.
- The scheduler (in `crates/sources/`) drives background refreshes
  according to per-source schedules in `config/sources/`. Scheduler
  failures don't propagate into UI errors; they appear as staleness
  in the "last updated" labels.

### Feed panels

Feed-style panels (news, events, alerts) use a ring buffer pattern:

- A bounded number of recent items are kept for fast render
  (default 200 per panel, configurable).
- Older items scroll out of the ring but remain in DuckDB —
  they're still queryable via the full archive view.
- Items the user explicitly opens, pins, or comments on are
  promoted to the "archive" set and are **never** auto-evicted.

This means the user's working memory (the last 200 headlines) is
always fast to render, and their explicit engagement (the few
stories they read carefully) is preserved indefinitely.

### Offline mode

An explicit offline toggle does three things:

1. Pauses the scheduler — no background fetches run.
2. Surfaces a visible "offline" indicator in the app chrome.
3. Each panel's "last updated" label is emphasized so the user
   knows how stale they're looking at.

Turning offline mode back off resumes the scheduler at its
normal cadence.

### Archive vs cache

The pipeline distinguishes two kinds of stored content:

- **Cache**: automatically fetched content, eligible for
  eviction based on size and age policies. Most ingested
  documents start here.
- **Archive**: content explicitly engaged with by the user.
  Never evicted. Includes anything opened, pinned, annotated, or
  promoted to an Observation/Event/Relation.

Records (`Observation`, `Event`, etc.) are always archival —
they're facts or claims, not cacheable ephemera.
`Document` records start as cache unless the user reads them.

## Rationale

**Why cache-first.** Panels render instantly. The user can scan
a dense screen without any panel being the slowest to load. If
one source is slow or down, its panel shows the last known value
plus a staleness indicator; the rest of the screen is unaffected.
This is the UX that matches ADR 0006's information-density goals
— a screen that loads in stages defeats the purpose of density.

**Why DuckDB is the system of record, not just a cache.** The
alternative is "records are canonical, DuckDB is a cache of
records." But the records *are* the data — there's nothing more
canonical upstream. The sources are just how records arrive;
once a record is in DuckDB with its provenance, DuckDB is the
truth. A cache would imply "also go check the real version";
there is no real version.

**Why the scheduler runs in the background, not on-demand.**
Users shouldn't have to hit refresh. A commodities analyst who
checks Stockpile every morning wants yesterday's overnight
updates already present, not "loading..." The scheduler ticks
at configured intervals (per-source, per-severity); the UI
always has what the scheduler last brought in.

**Why the ring buffer for feed panels.** Unbounded feeds would
degrade render performance as history accumulates. Bounded-by-
default-but-user-can-escape is the right balance: cheap common
case, no data loss on engagement.

**Why explicit archive is never evicted.** The user made an
explicit choice when they opened / pinned / annotated something.
Evicting that would be a data-loss event from the user's
perspective. Auto-evicting unopened items is acceptable because
the user never engaged with them; they're replaceable by re-
fetching. The distinction is: did the human pay attention to it?
If yes, keep forever.

**Why offline is an explicit mode, not automatic.** Automatic
detection of "are we online" is surprisingly hard (NAT, captive
portals, partial connectivity). An explicit toggle is
unambiguous: the user opts in to "don't try to fetch," and the
system respects that literally. Auto-detection can come later
as a UX layer over the explicit mechanism.

**Why staleness is surfaced per-panel.** Users care about
freshness differently for different data. A price panel from two
hours ago is useless; a corporate-filings panel from two hours
ago is fine. Per-panel "last updated" lets the user judge
freshness against their own threshold without the system having
to decide for them.

## Alternatives considered

**Fetch-on-render.** Rejected: slow, offline-hostile, breaks
information-density.

**Cache without persistence.** Rejected: every app restart would
wipe working state. The cache persists across sessions.

**Per-source refresh on user demand only.** Rejected: inverts
the workflow. The user wants fresh data waiting when they open
the app, not a series of "refresh this panel" button clicks.

**Infinite feed history.** Rejected: render performance.

**Automatic offline detection, no explicit toggle.** Rejected:
detection is fragile. An explicit toggle is honest about what
the system is doing.

**LRU cache policy.** Considered, and used for the `cache`
tier. Rejected for the `archive` tier: LRU would evict
old-but-engaged items in favor of new-but-unread ones, which is
the wrong priority for a research tool where the user's
engagement is the signal that matters.

## Consequences

**Positive**

- Panels are fast. The UI never waits on network.
- Offline works. The user can review existing research without
  connectivity.
- Network failures are graceful — they manifest as staleness,
  not errors.
- User engagement is preserved; nothing the user has touched
  disappears.

**Negative**

- DuckDB write-throughput is a real constraint. Heavy ingestion
  days could back up if the scheduler isn't rate-limited.
  Mitigated by per-source schedules and bounded parallelism in
  the ingestion pipeline.
- The cache-vs-archive distinction adds complexity to the
  storage model (an extra column or table classifying content).
  Worth it; otherwise we either evict user-engaged content or
  grow unboundedly.
- The user has to understand the "last updated" convention. Not
  a big ask, but it's a concept they must learn.

**Neutral**

- Per-panel staleness labels become part of the design language
  (ADR 0006). They're small, monospaced, non-color chrome.
- The scheduler's exact cadence is configuration, not
  architecture. Tunable per source without ADR revision.

## Code references

- `crates/storage/` — the DuckDB-backed storage. Cache vs archive
  distinction lives here.
- `crates/sources/` — the scheduler and source registry.
- `crates/pipeline/` — the write-through pipeline: fetch → parse
  → normalize → store.
- `config/sources/*.toml` — per-source refresh schedules.
- `apps/desktop/src/` — the UI that reads only from DuckDB via
  the Tauri command surface.

## Review notes

Reviewed 2026-04-20. Codifies the Phase 1 caching and offline
decisions. The cache-vs-archive distinction and the ring-buffer
behavior for feed panels were the human reviewer's specifications
during earlier design; this ADR records them with rationale.

The specific numbers (200-item ring buffer, per-source refresh
cadences) are defaults and configuration, not architecture.
Tuning them does not require revisiting this ADR; adding a new
content tier (e.g., "cold archive" with a different retention
policy) would.
