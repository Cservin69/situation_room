#!/usr/bin/env bash
# Run the dev binary with sane defaults.
set -euo pipefail
cd "$(dirname "$0")/.."
RUST_LOG="${RUST_LOG:-situation_room=debug,info}" cargo run --bin situation_room
