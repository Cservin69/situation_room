# ADR 0024 — `DerivedFrom` carries `record_type` (Session 94)

**Status**: Accepted (Sn-94 — code + migration land in one push; backfill
is a one-shot migration, not a runtime fallback)
**Date**: 2026-05-17
**Related**: ADR 0003 (closed-vocab record types), ADR 0004 (promotion
model), ADR 0021 (consensus promotion stage), `migrations/0001_init.sql`
(the schema this ADR amends)

## Context

`record_derived_from` (migration 0001) declares both `child_type` and
`parent_type` as `TEXT NOT NULL`. The writer
(`crates/storage/src/envelope_io.rs::insert_subjects_and_derivation`)
stamps `child_type` from the per-record-table caller, but stamps
`parent_type` as the literal string `"unknown"` — because the in-memory
`DerivedFrom` struct (`crates/core/src/schema/envelope.rs`) only carried
`record_id + role`, not the parent's record type.

The bug is silent: the column has been a write-only `"unknown"` constant
since v1. The read path
(`reconstruct_envelope`) doesn't even `SELECT parent_type`, so the
defect doesn't surface in round-trip tests. It only surfaces when an
analytics SQL tries to filter on it — which is exactly what happened in
`session89-analyze.sql:302`:

```sql
LEFT JOIN record_derived_from rdf
       ON rdf.parent_id = a.id
      AND rdf.parent_type = 'assertion'
      AND rdf.role IN ('consensus_support', 'promotion')
```

Every JOIN row evaluated to `parent_type = 'unknown' AND 'unknown' =
'assertion' → false`. The "unpromoted-assertion pile" stanza in Sn-89's
analyze SQL returned every assertion as unpromoted. The Sn-89 handoff
flagged the SQL as "buggy" but the underlying storage defect was never
patched.

## Decision

`DerivedFrom` gains a `record_type: RecordType` field. The writer stamps
the real value; the reader round-trips it; a one-shot migration
backfills the existing `"unknown"` rows by joining against each
per-record table.

### Schema additions to `DerivedFrom`

```rust
pub struct DerivedFrom {
    pub record_id: uuid::Uuid,
    pub record_type: RecordType,   // NEW
    pub role: DerivationRole,
}
```

`#[serde(default = "default_legacy_record_type")]` on `record_type` so
that legacy on-wire JSON (and any test fixture that omits the field)
deserializes cleanly. The default returns `RecordType::Assertion` — the
dominant historical parent kind for `derived_from` (every promotion +
consensus-support edge in the codebase points at an Assertion). Records
written under the new code path always supply the field explicitly; the
default only fires for legacy reads.

### Storage layer

- `insert_subjects_and_derivation`: stamps `d.record_type.as_str()`
  instead of the literal `"unknown"`.
- `reconstruct_envelope`: extends the SELECT list to include
  `parent_type`; parses via the `RecordType` serde representation.
  Rows whose `parent_type` is still `"unknown"` (a database not yet
  migrated past 0019, or a hand-edit) fall back to
  `RecordType::Assertion` rather than failing the whole query — the
  same lenient-on-read posture `decode_assertion_row` uses for Sn-78
  poison rows.

### Migration 0020 — backfill + Sn-78 cleanup

Bundled with the Sn-78 poison-row deletion (operator-approved at
kickoff; same migration touches different tables). The backfill pass
rewrites `parent_type = 'unknown'` rows in place by checking each of
the six per-record tables for `parent_id` membership. The order matches
the canonical enumeration (Observation → Event → Entity → Relation →
Document → Assertion); first-match wins. Rows whose `parent_id` resolves
to no table are left as `'unknown'` and warn-logged by the read path's
fallback.

The migration is irreversible (DELETE on assertion rows; UPDATE on
record_derived_from). DuckDB rolls the whole script back on any
statement error.

## Alternatives considered

**A — backfill at write time, keep struct as-is.** Storage's
`insert_subjects_and_derivation` could do a per-derived-from lookup
across the six tables to fill `parent_type`. Rejected: extra DB
round-trips at every insert; can't know parent_type without a query the
caller doesn't need today; the in-memory struct still lacks the
information, so any code that wants to reason about parent types must
re-query.

**B — `Option<RecordType>` field.** Rejected: pushes the burden of
"what does unknown mean" to every reader. The closed-vocab discipline
(`project_sr_no_source_routing`) is "the type is one of six known
things; if you can't say which, you have a bug." A None on the wire
makes the bug invisible; defaulting to Assertion + warn-logging the
fallback makes the bug visible without breaking the read path.

**C — add a `RecordType::Unknown` variant.** Rejected: violates ADR 0003
(closed vocabulary at six). The legacy-row case is rare, transitional,
and resolved by the migration; a permanent seventh variant for it would
become load-bearing.

## What this does NOT do

- Does not change the wire shape exposed to the frontend. `ProvenanceDto`
  types `derived_from` as `unknown` (see
  `apps/desktop/src/lib/api/types/ProvenanceDto.ts:34`), so adding a
  field is transparent to TypeScript callers.
- Does not add a runtime resolver for "given a record_id, what's its
  type?" The new column is the answer; pipelines that previously had no
  way to ask now ask the column.
- Does not retroactively reconstruct `derived_from` edges that were
  never written. Records with empty `derived_from` stay empty; this is
  about the integrity of edges that already exist.
- Does not amend ADR 0003 or the closed-vocabulary discipline. The set
  of record types stays at six.

## Verify gate

`session94-verify.sh` Stage 1 runs the new unit tests:

- `parent_type` round-trips through the storage layer.
- Legacy JSON without `record_type` deserializes (serde default fires).
- Migration 0020 backfills a synthetic `parent_type = 'unknown'` row to
  the correct value via the per-table lookup.
- Migration 0020 deletes a synthetic Sn-78 poison row from `assertions`.

Stage 4 (optional, operator's DB) runs the migration live and spot-
checks the row counts: `SELECT COUNT(*) FROM record_derived_from WHERE
parent_type = 'unknown'` should drop from the pre-migration value to 0
or the count of "parent_id has no per-table match" rows. The latter
case is informative — those edges point at deleted parents and signal a
different cleanup pass (Bug 4 from the Sn-94 kickoff candidate list).
