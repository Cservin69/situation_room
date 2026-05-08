# situation_room

An open-source desktop analyst workstation. Type a research topic;
get a single-screen workstation populated from public authoritative
sources, with every claim traceable to its origin.

Think of it as a Palantir-shaped situation room for an individual
analyst, with the LLM as the only specialist. There are no
hand-coded per-source adapters, no per-commodity registries, no
domain-specific extractors. The LLM classifies the topic into a
structured plan; per-source recipes are LLM-authored once against
real bytes and then applied deterministically by an LLM-free
runtime.

## How it works

```
   user types a topic
          │
          ▼
   ┌──────────────────┐
   │  Level-1: LLM    │  classifies into a ResearchPlan with six
   │  classifier      │  expectation buckets (observation, event,
   │                  │  entity, relation, document, assertion)
   └──────────────────┘  and 5–10 source nominations
          │
          ▼
   user reviews + accepts the plan in the desktop UI
          │
          ▼
   ┌──────────────────┐
   │  Level-2: LLM    │  one author run per nominated source.
   │  recipe author   │  Authors when the source's bytes fit the
   │                  │  plan; declines when they don't.
   └──────────────────┘
          │
          ▼
   ┌──────────────────┐
   │  LLM-free        │  applies each recipe deterministically.
   │  apply runtime   │  Five extraction modes: json_path,
   │                  │  css_select, csv_cell, pdf_table,
   └──────────────────┘  regex_capture.
          │
          ▼
   typed records (six types) inserted into local DuckDB,
   rendered in the workstation panels
```

The architectural commitments are encoded as ADRs in `docs/adr/`.
The two most important to read first are **ADR 0007** (research
function: two-level LLM architecture) and **ADR 0011** (plan
lifecycle: pending → accepted → rejected, fetch executor gated on
acceptance). ADR 0007's most recent amendment (Amendment 6,
Session 35) encodes plan-first authoring and multi-source-by-
default as architectural norms.

## Stack

- **Backend**: Rust workspace, seven library crates plus two
  binaries (desktop, situation-room CLI).
- **Frontend**: SvelteKit, Svelte 5 (runes).
- **Storage**: local DuckDB file (`stockpile.duckdb`).
- **LLM provider**: xAI by default; provider trait is generic
  (Anthropic / others are stubs).
- **Desktop shell**: Tauri 2.

The seven library crates:

```
core         the schema (zero workspace deps)
storage      DuckDB persistence
secure       SecureHttpClient, ApiKey, UrlGuard, FsGuard, Bounds
llm          provider router, structured-output extraction
pipeline     classify → author → apply, plus the executor
api          Tauri command surface and ts-rs export
apps_common  shared types between the desktop and CLI binaries
```

Composition root: `apps/desktop/src-tauri/src/main.rs`.

## Running locally

You will need:

- A recent Rust toolchain (`rust-toolchain.toml` pins the version).
- Node.js + npm for the desktop frontend.
- An xAI API key.

```
cp .env.example .env
```

Edit `.env` and set `XAI_API_KEY`. Then:

```
./scripts/run_desktop.sh
```

The script runs the Tauri dev shell with the SvelteKit frontend.
First launch creates `stockpile.duckdb` in the workspace root.

For the CLI variant (no GUI, prints the plan as JSON):

```
cargo run -p situation-room -- "your research topic"
cargo run -p situation-room -- recent
```

## Project structure

```
crates/                 the seven library crates
apps/
  desktop/              Tauri 2 desktop binary + SvelteKit frontend
  situation_room/       CLI binary
config/
  prompts/              the two LLM prompts (classifier, recipe author)
  sources.toml          registered source descriptors
  vocab/                controlled vocabularies (units, codes)
  detectors/            anomaly detector configs
docs/
  adr/                  architecture decision records
  failure_cases/        empirically observed failure cases
  security/             threat model
migrations/             DuckDB migrations
scripts/                run_desktop, etc.
tests/                  cross-crate integration tests
```

## Development

`Justfile` is the canonical task runner.

```
just check               fmt + IPC guard + check + clippy + test
just bootstrap           cargo build --workspace
```

If `just` is not installed:

```
brew install just                                            macOS
cargo install just                                           any platform
```

`just check` runs in this order: `cargo fmt --check`, the Tauri
command-registration guard, `cargo check --workspace`, `cargo
clippy --workspace --all-targets -- -D warnings`, `cargo test
--workspace`. The clippy step is non-negotiable; warnings are
errors.

## Hard rules (carry-over across sessions)

- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Closed enum of five extraction modes. Adding a sixth requires
  an ADR amendment. (ADR 0007)
- UUIDv7 + dedup_key for identity.
- All HTTP through `SecureHttpClient`. No fresh
  `reqwest::Client::new()`. (ADR 0009)
- Code validates format; the prompt teaches content.
- The plan is the specification; the source is a candidate.
  Author when fit; decline when not. (ADR 0007 Amendment 6)
- Multi-source by default: 5–10 source nominations per plan.
  (ADR 0007 Amendment 6)
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test --package situation_room-api`.
  Never hand-edit.
- Prompts teach principles, not source-by-source routing.
  Source-specific rules ("if URL contains X, do Y") belong in
  source descriptors, not in prompt prose. (ADR 0007 golden rule)

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). New work generally
starts by reading the relevant ADR, the most recent
`STOCKPILE_HANDOFF_SESSION*.md`, and the existing code in the
crate you intend to touch.

## License

**Starting from this point forward**, this project is licensed under the **PolyForm Noncommercial License 1.0.0**.

- You may **freely copy, modify, and use** the code for **non-commercial** purposes (personal use, research, education, non-profit, etc.).
- **Commercial use is not allowed** — this includes selling the software, offering it as a paid service, or using it in a commercial product without permission.

Previous versions remain under the MIT License.

If you need a **commercial license**, please contact me.
