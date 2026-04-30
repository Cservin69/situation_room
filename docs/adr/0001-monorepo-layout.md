# ADR 0001 — Monorepo workspace layout

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0002 (Tauri + Svelte), ADR 0005 (DuckDB), ADR 0009
(security posture)

## Context

Situation_room is a Rust-heavy project with several distinct concerns:
schema definitions, data storage, source adapters, LLM integration,
a pipeline that moves data through stages, analytical detectors,
and a Tauri desktop shell. Early in Phase 1 we had to decide how to
lay these out: a single crate with modules, a workspace of small
crates, or something in between.

The failure modes to avoid:

- **The mega-crate.** One crate with everything in it. Compilation
  is slow, dependency boundaries are implicit, circular deps appear
  easily. Works for small projects; quickly stops working as the
  surface grows.
- **The micro-crate swarm.** Thirty tiny crates each with their own
  Cargo.toml. Compilation is fast incrementally but slow from
  scratch, and the cognitive overhead of tracking thirty `Cargo.toml`
  files is real. Premature modularization.
- **The "app" crate trap.** A separate crate whose only job is to
  wire everything together, sitting alongside a `cli` crate and a
  `desktop` crate and maybe a `web` crate. Usually ends up with most
  of the composition logic duplicated across the entry-point crates.

## Decision

**Single Cargo workspace. Seven library crates. One binary
composition root in the Tauri app directory.**

```
crates/
  core/         schema, vocabularies, record types
  secure/       security primitives (see ADR 0009)
  storage/      DuckDB persistence
  sources/      source adapters and registry
  llm/          LLM provider integrations
  pipeline/     ingest → normalize → extract → promote, plus research
  analytics/    detectors
  api/          Tauri command surface + ts-rs type export

apps/
  desktop/
    src-tauri/  Tauri binary — the composition root
    src/        SvelteKit frontend
```

Each library crate has a single, well-defined concern. Dependency
direction is strict: `core` depends on nothing in the workspace;
`secure` depends on nothing in the workspace; every other crate
depends on `core` and `secure`; `pipeline` sits at the top of the
dependency tree, pulling from `storage`, `sources`, `llm`, and
`analytics`. There is no `app` crate. Composition happens in
`apps/desktop/src-tauri/src/main.rs`.

## Rationale

**Why a workspace, not a mega-crate.** Independent compilation
matters when iterating. Editing the schema shouldn't require
recompiling the Tauri shell. The workspace structure gives cached
incremental builds per crate and makes dependency direction
explicit — you can't accidentally `use storage::Foo` from `core`,
because `core`'s `Cargo.toml` doesn't list storage.

**Why seven crates and not more.** Each of the seven has a real
compile-unit's worth of code. `core` holds the schema, which every
other crate imports; separating it makes the boundary explicit.
`secure` is the subject of ADR 0009; it's cross-cutting and needs
to be a separate crate so everyone depends on it uniformly.
`storage`, `sources`, `llm`, `analytics`, and `pipeline` are each a
substantial body of code with distinct dependencies (DuckDB, HTTP
clients, LLM SDKs, numerical libraries, orchestration). Splitting
them further (one crate per source adapter, one crate per LLM
provider) would be the micro-crate failure mode; each adapter is
200–400 lines and they share enough machinery that a common crate
is cheaper than per-adapter crates.

**Why no separate `app` / `cli` / `core-binary` crate.** The
composition root is small — a `main.rs` that configures logging,
loads config, constructs the storage handle, wires the providers,
and hands control to Tauri. Extracting that into its own library
crate would mean the Tauri shim *also* has a `main.rs` that calls
the library crate's composition function. That's one extra layer
of indirection for no gain. If we later need a non-Tauri entry
point (a CLI, a server), we'll add one then, duplicating whatever
wiring is shared at that point. YAGNI.

**"Structure follows code, not anticipates it."** Several of the
folders inside crates (detectors, providers, source adapters) are
single files today. They become folders when they grow internal
complexity worth hiding. Creating empty folders "because we'll need
them eventually" accumulates cruft and forces future contributors
to navigate mostly-empty directory trees.

## Alternatives considered

**Mega-crate.** Rejected: compile time, implicit boundaries.

**Micro-crates (20+).** Rejected: cognitive overhead, from-scratch
compile time, Cargo.toml sprawl.

**Separate `app` crate with thin binary shims per platform.**
Rejected: extra indirection, no current consumer that benefits.
Revisitable when a second entry point appears.

**No `api` crate — put Tauri commands in `apps/desktop/src-tauri`.**
Rejected: Tauri commands are the IPC contract with the frontend,
and the contract benefits from being testable and type-exportable
independently of the Tauri binary. `api` holds the command handlers
and the ts-rs type exports; the binary just mounts them.

**No `secure` crate — put security primitives in `core`.** Rejected:
`core` is the schema crate and should have minimal runtime deps.
`secure` pulls in `reqwest`, `rustls`, `secrecy`, `zeroize` —
heavyweight runtime machinery that `core` shouldn't transitively
require.

## Consequences

**Positive**

- Incremental compilation works well; editing one crate doesn't
  force rebuilding the rest.
- Dependency direction is visible in `Cargo.toml` files; violations
  fail the build.
- The composition root is one file, one place to understand how the
  system boots.

**Negative**

- A workspace is slightly more ceremony than a mega-crate for
  someone new to Cargo. Mitigated by the workspace `Cargo.toml`
  centralizing version numbers and feature flags.
- Adding a crate is a real operation: new `Cargo.toml`, new entry
  in the workspace members, new `lib.rs`. We accept this friction
  because the alternative is the micro-crate swarm.

**Neutral**

- If we later need a non-desktop entry point, adding it is
  straightforward — another directory under `apps/`, another
  binary in its Cargo.toml. No restructuring required.

## Code references

- Root `Cargo.toml` — workspace definition, shared dependencies.
- `crates/*/Cargo.toml` — per-crate dependencies.
- `apps/desktop/src-tauri/Cargo.toml` — composition root.
- `apps/desktop/src-tauri/src/main.rs` — the actual boot sequence.

## Review notes

Reviewed 2026-04-20. This ADR codifies the Phase 1 layout decision.
No changes from the shipped structure; the rewrite adds the
explicit rationale and alternatives that were previously implicit
in the handoff document.
