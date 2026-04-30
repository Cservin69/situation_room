# Contributing to situation_room

situation_room is an open-source project. Contributions of all kinds are welcome:
new data sources, new anomaly detectors, frontend panels, documentation, bug
reports, and design feedback.

## Before you start

1. Read [`docs/architecture/overview.md`](docs/architecture/overview.md).
2. Read the relevant ADR(s) in `docs/adr/`. The schema and architectural
   decisions are deliberate; please discuss in an issue before proposing
   changes to them.

## Development setup

```bash
cp .env.example .env       # add your LLM API key(s)
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## How to add a new data source

See [`docs/sources/adding_a_source.md`](docs/sources/adding_a_source.md).
Short version: implement the `Source` trait in `crates/sources/src/adapters/`,
register it, add a `config/sources/your_source.toml` schedule.

## How to add a new anomaly detector

Add a file under `crates/analytics/src/detectors/`, implement the `Detector`
trait, and register it. Tunable thresholds belong in
`config/detectors/thresholds.toml`, never hardcoded.

## Code style

- `cargo fmt` formats everything.
- `cargo clippy` must pass without warnings.
- Public items in `crates/core` need doc comments.
- Tests for any new pipeline logic go in `tests/integration/` or
  `tests/golden/` for output regressions.

## Reviews

This project is reviewed by humans before merge. Keep PRs small and focused.
If you're changing the schema in `crates/core`, expect deeper review.
