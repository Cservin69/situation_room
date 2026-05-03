# Justfile — top-level developer task runner.
#
# Why this exists
# ---------------
# Session 23.1 added scripts/check_tauri_commands_registered.sh as a
# guard against the class of bug where a #[tauri::command] is added
# in crates/api but its sibling registration in
# apps/desktop/src-tauri/src/main.rs::generate_handler! is forgotten.
# Both compilers (Rust and TypeScript) accept the omission; only a
# user clicking the affected feature surfaces it. Until the guard is
# wired into something that runs before tag, the operator has to
# remember to call it. This Justfile makes "remember to call it" an
# easy habit: `just check` runs everything, including the guard.
#
# Session 24 reshaped this into the canonical pre-tag flow per the
# Session 24 handoff P4: "If there's no such file: write a tiny
# Justfile with two targets (check-tauri and check, where check runs
# both this and the cargo / clippy / fmt sequence). Five lines."
# This file is more than five lines because it also exposes the dev
# / build / hooks shortcuts the operator already had as discrete
# scripts, so `just` becomes the single entry point. Each non-trivial
# target either calls an existing script in scripts/ or runs a
# bare-cargo / npm command — no logic is duplicated.
#
# Install just on macOS: `brew install just`. On Linux: see
# https://just.systems/man/en/packages.html.
#
# Usage:
#     just                    # list targets
#     just check              # full pre-tag check (run before commits)
#     just check-tauri        # the Session 23.1 IPC-registration guard
#     just test               # cargo test --workspace
#     just lint               # clippy with -D warnings
#     just fmt-check          # rustfmt --check
#     just fmt                # rustfmt write
#     just dev                # desktop dev (cleans up :5173 on exit)
#     just dev-cli            # CLI binary with sane RUST_LOG
#     just build              # release build of the desktop binary
#     just bootstrap          # first-time clone setup
#     just install-hooks      # install pre-commit hooks
#     just check-types        # SvelteKit / TS check (frontend only)

# Default target: list everything available.
default:
    @just --list

# Pre-tag composite: every check the operator should run before
# committing or tagging. Order matters — fmt-check is fastest and
# fails earliest; check-tauri is sub-second; cargo check is faster
# than clippy; clippy is faster than test.
#
# A failure at any step short-circuits the rest (just exits non-zero
# on the first failed recipe call).
check: fmt-check check-tauri
    cargo check --workspace --all-targets
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Session 23.1 IPC-registration guard. Sub-second; safe to run
# arbitrarily often. Prints the offending function names with a
# remediation hint when it fails. See the script's header for
# background on why this guard exists.
check-tauri:
    bash scripts/check_tauri_commands_registered.sh

# Run the full test suite.
test:
    cargo test --workspace

# Run a focused subset of tests by name pattern.
test-name name:
    cargo test --workspace {{name}}

# Run the live-tagged tests (require XAI_API_KEY / ANTHROPIC_API_KEY
# in the env or .env). These are #[ignore]'d by default so `just
# test` doesn't spend network time on them; this target opts in.
test-live:
    cargo test --workspace -- --ignored

# Clippy with hard-fail on warnings, matching CI.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# rustfmt --check (read-only). Used by `just check`.
fmt-check:
    cargo fmt --all -- --check

# rustfmt write — actually formats the code.
fmt:
    cargo fmt --all

# SvelteKit / TypeScript type check on the frontend. Operates inside
# apps/desktop because that's where the package.json lives. Run
# after any DTO change in crates/api so an out-of-date generated
# .ts file fails loudly before the next dev session.
check-types:
    cd apps/desktop && npm run check

# Launch the desktop app in dev mode. Wraps scripts/run_desktop.sh
# which puts cargo tauri dev + Vite in one process group and frees
# port 5173 on Ctrl-C (see the script's header for the failure
# modes it works around).
dev:
    bash scripts/run_desktop.sh

# Run the CLI binary directly. The script applies a sane default
# RUST_LOG so situation_room logs are visible without overriding
# the rest of the workspace.
dev-cli *args:
    RUST_LOG="${RUST_LOG:-situation_room=debug,info}" cargo run --bin situation-room -- {{args}}

# Release build of the desktop binary. Produces .app / .dmg /
# .AppImage under target/release. First run also produces signed
# artifacts on macOS if signing is configured in tauri.conf.json.
build:
    cd apps/desktop && npm run tauri -- build

# First-time setup on a fresh clone: copy .env.example to .env if
# absent, then build the workspace.
bootstrap:
    bash scripts/bootstrap.sh

# Install the pre-commit hook that scans staged changes for likely
# secret patterns and runs cargo fmt --check on staged Rust.
install-hooks:
    bash scripts/install-hooks.sh
