# Contributing to situation_room

situation_room is an open-source project. Contributions of all kinds are welcome:
new data source descriptors, frontend panels, documentation, bug
reports, and design feedback.

## Before you start

1. Read the ADRs in [`docs/adr/`](docs/adr/) — they encode the
   load-bearing architectural decisions. ADR 0007 (research
   function: two-level LLM architecture) is the most important
   for understanding how the workstation is built.
2. Read [`README.md`](README.md) for what situation_room is and
   how to run it locally.
3. The schema and architectural decisions are deliberate; please
   discuss in an issue before proposing changes to them.

## Development setup

```bash
cp .env.example .env       # add your LLM API key(s)
just bootstrap             # equivalent to `cargo build --workspace` after the .env copy
just check                 # full pre-tag check (fmt + IPC guard + check + clippy + test)
```

If `just` is not installed:

```bash
brew install just            # macOS
# or follow https://just.systems/man/en/packages.html
```

The Justfile at the repo root is the canonical task runner. Use `just`
to list every target. The minimum-viable commit-time check is:

```bash
just check
```

— which runs `cargo fmt --check`, the Tauri command-registration guard
(see below), `cargo check --workspace`, `cargo clippy -D warnings`, and
`cargo test --workspace` in that order.

## Workspace layout

The Cargo workspace has seven library crates plus two binaries:

```
crates/core         schema, vocabularies, record types
crates/secure       security primitives (see ADR 0009)
crates/storage      DuckDB persistence
crates/llm          LLM provider integrations (xAI, Anthropic)
crates/pipeline     classify → fetch → extract → store; the research function
crates/api          Tauri command surface + ts-rs type export
crates/apps_common  helpers shared by the desktop and CLI binaries
                    (currently: source-descriptor TOML loader)
apps/desktop/       Tauri 2 desktop app (Rust under src-tauri/, Svelte 5 under src/)
apps/situation_room/  CLI binary that classifies a topic and persists the plan
```

Per ADR 0001, dependency direction is strict and visible in
per-crate `Cargo.toml`s. Composition happens in
`apps/desktop/src-tauri/src/main.rs` and `apps/situation_room/src/main.rs`.

## How to add a new data source

Don't. There is no static source registry to add to.

Under ADR 0015 (Session 37), the LLM emits source nominations
directly during classification. Each nomination carries its own
`endpoint_url`, `priority_tier`, and an optional `known_id` stamped
from the operator's in-DB sources memory (the recipes ⨝
recipe_fetch_attempts ⨝ research_plans join — see
`crates/storage/src/sources_memory.rs`). There is no
`config/sources.toml` to edit, no Rust adapter to write, no
descriptor to author.

To "add" a new source — meaning, to teach situation_room to fetch
from a source it has never seen — classify a topic that maps to
that source. The LLM nominates the URL from training-distribution
knowledge; the operator accepts the plan and runs a fetch; the
first successful fetch enters the sources memory. The next
classification on a related topic sees that URL as recognized
context and stamps `known_id` against it.

The two surviving entries in `config/sources.toml` (`csv_demo`,
`json_demo`) are test fixtures used by the executor's `#[ignore]`
live tests. They are not a registration surface. Don't edit them
unless you are touching those tests; if you remove either, the
matching live test fails.

The source-specific principles that previously lived as TOML
annotations (PDF-vs-HTML strategy, JS-rendering caveat,
rate-limit notes, paywall caveats) have been absorbed into
`config/prompts/recipe_author.md` v1.10–v1.12 as principles, not
routing rules. Edit the prompt when a *category* of failure
needs new guidance; never name a specific source in the prompt.

## How to add a new `#[tauri::command]`

Two edits, both mandatory:

1. Define the function in `crates/api/src/commands*.rs` with the
   `#[tauri::command]` attribute.
2. Register it in `apps/desktop/src-tauri/src/main.rs` inside the
   `tauri::generate_handler![…]` macro.

The Rust compiler accepts (1) without (2). The TypeScript compiler
accepts a frontend `invoke<T>('name', …)` call without (2). The
mismatch only surfaces the first time a user clicks the affected
feature, at which point Tauri returns "Command \<name\> not found".
Session 22 → 23 had a two-session bug of exactly this shape.

To catch the omission deterministically, run:

```bash
just check-tauri        # or: bash scripts/check_tauri_commands_registered.sh
```

`just check` runs this guard automatically. CI runs it before
`cargo check`. The guard is sub-second and fail-fast.

## Code style

- `cargo fmt` (or `just fmt`) formats everything; CI checks with
  `cargo fmt --all -- --check`.
- `cargo clippy` must pass with `-D warnings`; `just lint` enforces
  this locally.
- Public items in `crates/core` need doc comments.
- Tests go alongside the code that they test (Rust convention) — a
  `#[cfg(test)] mod tests` block at the bottom of the source file
  is the default; integration tests under `tests/` are for
  cross-crate behavior tests.
- Components in `apps/desktop/src/` use only CSS variables from
  `apps/desktop/src/lib/design/global.css`. No hardcoded hex.
- Svelte 5 runes-using files end in `.svelte.ts`, not `.ts`.

## Reviews

This project is reviewed by humans before merge. Keep PRs small and focused.
If you're changing the schema in `crates/core`, expect deeper review.
