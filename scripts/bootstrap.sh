#!/usr/bin/env bash
# Bootstrap a fresh clone: copy .env.example → .env if not present, build.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -f .env ]; then
  cp .env.example .env
  echo "Created .env from .env.example — edit it to add your API keys."
fi

echo "Building workspace…"
cargo build --workspace
echo ""
echo "✓ Workspace built. Run \`cargo run --bin situation_room\` to verify."
