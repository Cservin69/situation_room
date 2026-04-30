# situation_room

> An open-source analyst workstation for critical-minerals intelligence.

Type a commodity, get a single screen with price, flow, production, inventory,
policy signals, and corporate guidance — every number traceable to a public
source, refreshed at the cadence that source actually allows.

This is **Phase 1**: workspace structure only. The crates compile but most do
not yet do anything. See `docs/architecture/overview.md` for what's coming.

## Prerequisites

- **Rust 1.82+** (a `rust-toolchain.toml` will install the right version automatically via rustup)
- **Node.js 20+** and **pnpm** (only needed once we add the desktop frontend in a later phase)
- macOS, Linux, or Windows

## Quick start

```bash
# 1. Clone (or unzip)
git clone https://github.com/Cservin69/situation_room.git
cd situation_room

# 2. Configure your API keys
cp .env.example .env
$EDITOR .env       # paste your ANTHROPIC_API_KEY (and/or others)

# 3. Build everything
cargo build --workspace

# 4. Verify the workspace is wired up correctly
cargo check --workspace
```

If `cargo check --workspace` succeeds, Phase 1 is working on your machine.

## What works in Phase 1

- The workspace builds cleanly across all 7 crates.
- The `Source` trait is defined; new data adapters can be added against it.
- Configuration loading (sources, prompts, vocabularies, detector thresholds)
  is wired through `figment` and reads `config/*.toml`.
- Logging via `tracing` is configured.
- `.env` loading via `figment`'s env provider works.

## What doesn't work yet

- No record types implemented (Phase 2).
- No source adapters implemented (Phase 3+).
- No LLM extraction implemented (Phase 3+).
- No frontend (Phase 4+).
- No DuckDB schema (Phase 2).

## Project layout

See `docs/architecture/overview.md` for the full guided tour. In brief:

```
crates/
  core/        the schema — six record types, envelope, controlled vocab
  storage/     DuckDB persistence
  sources/     data adapters, one per source
  llm/         provider router, prompts, structured extraction
  pipeline/    ingest → normalize → extract → promote
  analytics/   anomaly detectors, aggregates
  api/         Tauri command surface
apps/desktop/  Tauri + Svelte app (composition root lives here)
config/        editable runtime config (prompts, vocab, thresholds)
docs/adr/      architecture decision records
```

## Architecture decisions

The four load-bearing decisions are documented as ADRs:

- [ADR 0001 — Monorepo workspace layout](docs/adr/0001-monorepo-layout.md)
- [ADR 0002 — Tauri + Svelte over Leptos/WASM](docs/adr/0002-tauri-vs-leptos.md)
- [ADR 0003 — Six record types](docs/adr/0003-six-record-types.md)
- [ADR 0004 — Assertion promotion model](docs/adr/0004-assertion-promotion.md)
- [ADR 0005 — DuckDB as the storage engine](docs/adr/0005-duckdb-storage.md)

## License

AGPL-3.0-or-later. See `LICENSE`.

## Contributing

See `CONTRIBUTING.md` and `docs/sources/adding_a_source.md`.

## Security

Security is a first-class concern. See `docs/security/threat_model.md`
for the full threat model and `docs/adr/0009-security-posture.md` for
the posture decisions.

Quick checklist for contributors:

- All outbound HTTP goes through `situation_room_secure::http::SecureHttpClient`.
- All URLs that could be influenced by user input or fetched content go
  through `situation_room_secure::url_guard::UrlGuard`.
- All filesystem writes influenced by user input go through
  `situation_room_secure::fs_guard::FsGuard`.
- API keys are `situation_room_secure::secrets::ApiKey`, loaded only from env.
- Logging uses `situation_room_secure::logging::init()`; never `println!` with
  sensitive values.
- Install the local git hooks: `./scripts/install-hooks.sh`.

Security-relevant CI jobs run on every PR:
- `cargo deny check` (licenses, sources, denied crates)
- `cargo audit` (vulnerability database)
- Grep-based secret scan (belt-and-suspenders)
