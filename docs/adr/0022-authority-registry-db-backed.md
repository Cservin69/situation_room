# ADR 0022 — Authority registry: DB-backed (Session 88)

**Status**: Proposed
**Date**: 2026-05-16
**Related**: ADR 0004 (assertion promotion model), ADR 0021
(consensus promotion stage), ADR 0017 (closed-vocabulary discipline),
ADR 0015 (sources.toml doc-narrowed)

## Context

ADR 0004 + ADR 0021 established the authoritative-source pathway:
config-driven `(source_id, metric, topic) → quorum-override` lookup
that fast-tracks Assertion rows from named sources past the
consensus-quorum gate. Session 82 shipped pathway 1 as a TOML-loaded
`AuthorityRegistry` (`crates/pipeline/src/authoritative.rs` +
`config/vocab/authoritative_sources.toml`). Session 84 wrapped it in
a hot-reload watcher (`LiveAuthorityRegistry` —
`crates/pipeline/src/authoritative_live.rs`). Session 85 added a
lossy-continue schema-warn pass for unknown fields. Session 87 lifted
`distance_1_suggestion` to public so `sources.toml` could mirror the
warn shape.

The TOML-as-runtime model has known edges that the next operator
surface (registry edits from a TUI, audit log of who changed what,
queryable history) won't sit cleanly on top of. None of those edges
are firing today; this ADR records the migration plan and lands the
storage scaffold so the surface is ready when the operator surface
demands it.

## Decision

Migrate `AuthorityRegistry` to a DB-backed table named
`authority_registry`. The TOML file becomes a bootstrap/seeding
artefact: on first boot after migration 0019 the table is empty,
boot reads the TOML and copies rows into the table tagged
`provenance = 'toml_seed'`, subsequent boots read from the table
directly. Operators can add rows via a future TUI / CLI surface with
`provenance = 'operator'`.

### Why a table, not JSON-on-disk

Three reasons, in order of operator-visible weight:

**1. Promote-stage lookup cost.** The promote pass runs a fan-out of
`(source_id, metric, topic)` lookups per Assertion under
consideration. The current in-memory implementation is an O(N) Vec
scan keyed by `source_id`-suffix substring match
(`authoritative.rs::lookup_for`). N is small today (≤30 entries) so
the scan cost is negligible; with a DB-backed registry the
composite index on `(source_id, metric)` + `(source_id, topic)`
serves the same lookups in O(log N) with no plumbing change at the
call site — the lookup just goes through a `Store` method instead.

**2. Single source of truth.** The rest of the schema (plans,
recipes, records, promote_history) lives in DuckDB. Putting the
authority registry in the same store means backup/snapshot/audit
flows that already exist for the schema include the registry
without per-config special-casing. The TOML file's role narrows to
"declarative seed for fresh installs" — same posture ADR 0015
narrowed `sources.toml` to.

**3. Operator-surface plumbing.** A future "add an authoritative
source from the dashboard" surface needs an idempotent INSERT path
keyed on `(source_id, metric, topic)`. The DB-backed registry has
that path for free; a TOML-based one would need a write-back-to-file
mechanism that's hard to make correct (TOML is order-sensitive,
preserves comments, etc.).

### Two-stage migration

**Stage 1 (Session 88, this ADR).** Land the scaffold:
- Migration `0019_authority_registry.sql` creates the table +
  indexes. Empty at first.
- `crates/storage/src/authority_registry.rs` exposes
  `Store::authority_registry_entries() / upsert / clear` for the
  next session to consume.
- Runtime registry is **unchanged**: `LiveAuthorityRegistry` still
  reads the TOML at boot + reloads on file change. The DB-backed
  storage exists but no runtime code reads from it. Operator-visible
  behaviour is byte-for-byte the same as Session 87.

**Stage 2 (next session).** Light the runtime path:
- At boot, if the `authority_registry` table is empty, copy the
  TOML-loaded entries into the table with `provenance = 'toml_seed'`.
- Switch `LiveAuthorityRegistry::reload` to read from the DB instead
  of the TOML. The file-watcher stays as the "operator-edits-TOML"
  trigger for the seed-refresh case; an operator who wants a
  permanent change goes through the (future) TUI / CLI.
- Add an integration test that proves: fresh boot copies TOML →
  table; TOML edits picked up by watcher trigger a re-seed (configurable);
  table-only entries survive a TOML deletion.

Stage 2 is intentionally a separate session so the migration is
reversible by `DROP TABLE authority_registry;` in the unlikely event
the scaffold turns out wrong. Once Stage 2 ships, the table becomes
load-bearing.

## Consequences

### What gets easier

- Future "operator-edits-registry-from-dashboard" surfaces have a
  storage path they can write to without inventing TOML-merge logic.
- Audit ("who added `epa:fred` as authoritative for `unemployment_rate`,
  and when") gains a natural home via the `created_at` / `updated_at`
  columns on the table.
- Backup/snapshot of operator workspace state captures the registry
  for free.

### What gets harder

- Bootstrapping a fresh install now has two surfaces: the TOML
  (seed-on-empty) and the DB (runtime). The seed-on-empty rule keeps
  this from being operator-visible on the common path but adds a
  conditional to the boot sequence. We accept this for the
  "single-source-of-truth at runtime" property.
- The TOML file's role narrows to "seed for fresh installs". An
  operator who edits the TOML expecting their change to land at the
  next promote pass will be surprised — Stage 2 needs to either keep
  the file-watcher-triggers-re-seed behaviour OR surface a clear
  message that runtime config is now DB-only. Stage 2 ADR amendment
  will pick the posture.

### Closed-vocab discipline

The `provenance` column is a closed-vocab string (`'toml_seed'` /
`'operator'`) — consistent with ADR 0017 / `project_sr_no_source_routing`.
Adding a third variant requires a code change so the closed set
stays auditable.

## Non-goals

- **No registry edits from any UI surface in Session 88.** The
  scaffold is read-only on the current runtime path; Stage 2 lights
  the runtime read first, the operator-write surface follows that.
- **No retroactive seeding on existing installs.** The migration
  creates an empty table; Stage 2's boot-time seed-on-empty does the
  populate. Operators upgrading mid-flight see no change because the
  runtime path still reads the TOML.

## Status path

- Proposed (Session 88): scaffold landed, runtime unchanged.
- Accepted (Stage 2 session): runtime reads from DB. Required
  evidence: integration test passes, fresh-install boot copies TOML
  → table, an operator-modified TOML edit is reflected in the next
  promote pass.
