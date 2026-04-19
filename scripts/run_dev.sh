#!/usr/bin/env bash
# Run the dev binary with sane defaults.
set -euo pipefail
cd "$(dirname "$0")/.."
RUST_LOG="${RUST_LOG:-stockpile=debug,info}" cargo run --bin stockpile
