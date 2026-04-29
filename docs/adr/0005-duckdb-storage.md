# ADR 0005 — DuckDB as the storage engine

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0003 (six record types), ADR 0008 (offline mode),
ADR 0007 (research function)

## Context

Stockpile needs an embedded database. The workload is analytical:
most queries are time-series aggregates, subject-filtered scans,
joins between records and their derived-from chains, and
windowed comparisons. The data footprint is modest for a single-
user workstation — hundreds of thousands of records, perhaps
tens of millions over time — but the query patterns look like
OLAP, not OLTP.

Three credible options:

1. **SQLite.** Ubiquitous, battle-tested, embedded, small.
   Row-oriented; analytical queries (GROUP BY, aggregates over
   large scans) are slow compared to columnar stores.
2. **Postgres.** Full-featured, rich type system, but requires
   running a server process. Unfriendly for the zero-setup
   desktop-app story.
3. **DuckDB.** Columnar analytical engine, embedded (like
   SQLite), single-file storage, SQL-compatible, has spatial
   and time-series extensions, native Parquet interop.

The decision was not close.

## Decision

**DuckDB from day one. Not SQLite. Not Postgres.**

Stockpile ships a DuckDB database file per workspace. All
records, all recipes, all derived artifacts live in it. The
database file is the system of record; the file is portable
(copy it, you have the whole state). Migrations live in
`migrations/` as SQL files; the storage crate applies them at
startup.

## Rationale

**Columnar storage matches the query shape.** Panels run
aggregates over large scans of records filtered by subjects and
time. "Show me the median of this metric across all sessions
tagged `Li` in the last 90 days" is a columnar query. Running it
on a row store means scanning every row's every column and
discarding most of them. DuckDB scans only the columns the query
actually uses.

**Single-file storage is a product feature.** A user can
back up their workstation by copying one file. They can share a
research state with a colleague by sending that file. They can
version-control it if they really want to (not recommended, but
possible). None of this works with a server-based database.

**Native Parquet interop matters for the future.** Stockpile
will likely want to ingest public data dumps (USGS's historical
tables, census data, etc.). Many of these ship as Parquet today
and will increasingly do so. DuckDB reads Parquet natively
without a conversion step. A user can also export a table as
Parquet with a single query, which is valuable for
reproducibility and for feeding data into other tools.

**The spatial extension is actually good.** Geometry queries
("entities within this polygon", "events near this point") are
first-class in DuckDB with the spatial extension loaded. SQLite
has SpatiaLite, but it's a heavier integration with more
platform-specific setup.

**Time-series queries benefit from UUIDv7's time-ordered ids.**
Per ADR 0003, every record has a UUIDv7 primary key, so `ORDER
BY id` gives chronological order for free. DuckDB's columnar
layout plus this ordering makes range scans over time very
fast without a separate index.

**Embedded, not server.** The user's experience must be: install
the binary, run it, type a topic. No "also install Postgres and
configure a connection." SQLite and DuckDB both deliver this;
Postgres does not.

## Alternatives considered

**SQLite.** Rejected on query performance. The analytical
workload Stockpile will run — cross-session aggregates, large
filtered scans, panel-driven joins — is the workload SQLite is
worst at. SQLite is the right answer for OLTP-shaped workloads
with many small transactions. Stockpile's shape is the opposite.

**Postgres (local instance).** Rejected on setup friction. Even
with `brew install postgres` being a single command, "make sure
the server is running, create a user, create a database, manage
connection strings" is a lot of non-product work for someone who
just wants to type a topic.

**Postgres (hosted).** Rejected: Stockpile is a desktop app for
a researcher's own machine. Hosting means an account, a network
dependency, and a monthly bill for a product that's supposed to
be OSS and self-contained.

**SurrealDB, LanceDB, other newer options.** Rejected on
maturity. DuckDB is 2019-era, has production users, has a
bounded and understood failure mode. Newer options may be
excellent but the risk profile is higher than necessary.

**Flat files (Parquet + DataFusion).** Considered briefly.
Rejected because mutation is painful — Stockpile isn't write-
once; records get updated, dedup-keyed, promoted. A real
database handles this better than files plus a query engine.

## The duckdb crate situation

As of Phase 2c, the `duckdb` Rust crate is deliberately **not**
in the workspace dependencies. It was removed in Phase 1b when
we deferred the real storage work and didn't want to carry an
unused dep. It gets re-added in Phase 2e along with the first
real migrations and round-trip tests.

When re-added:
- `duckdb = { version = "1.1", features = ["bundled"] }` in the
  workspace dependencies.
- `StorageError::DuckDb(#[from] duckdb::Error)` variant restored
  in `crates/storage/src/error.rs`.
- First migration (`0001_init.sql`) creates one table per record
  type plus junction tables for subjects and derived-from chains.

## Consequences

**Positive**

- Analytical queries are fast by default; panels don't wait.
- The workspace is one file; backup and transfer are trivial.
- Parquet interop means future public-data ingestion is easy.
- Spatial extension handles geometry queries without a separate
  system.
- UUIDv7 + columnar = cheap time-range scans.

**Negative**

- DuckDB's concurrency model is single-writer. Not a concern for
  a single-user desktop app, but means a multi-user future would
  need architectural changes (or a sync layer).
- DuckDB is younger than SQLite; fewer tools know how to read its
  files directly. Mitigated by Parquet export.
- The `bundled` feature builds DuckDB from source at `cargo
  build` time, which is a slow first build. Acceptable for a
  project where most contributors build infrequently.

**Neutral**

- Migrations are SQL files, not Rust code. Standard practice;
  keeps the storage schema visible without reading Rust.
- DuckDB supports ACID transactions (via MVCC). The pipeline
  stages can use them for atomic writes of related records.

## Code references

- `crates/storage/` — the storage crate. Stubbed as of Phase 2c;
  real implementation is Phase 2e.
- `migrations/0001_init.sql` — initial schema (empty stub).
- `migrations/0002_indexes.sql` — index strategy (empty stub).
- `Cargo.toml` workspace — `duckdb` currently absent, to be
  re-added Phase 2e.

## Review notes

Reviewed 2026-04-20. Codifies the Phase 1 storage-engine choice.
The "duckdb crate situation" section is new — it captures the
Phase 1b→2e state transition so a future contributor re-reading
this ADR isn't surprised that the crate is missing from the
workspace deps.

The core decision (DuckDB over SQLite and Postgres) has not been
revisited and does not need to be.
