#!/usr/bin/env bash
# scripts/run_desktop.sh — launch Tauri dev and clean up cleanly on exit.
#
# Why this exists
# ---------------
# `cargo tauri dev` already spawns Vite (port 5173) via tauri.conf.json's
# `beforeDevCommand`. In normal operation it tears Vite down on exit.
# But two failure modes keep biting:
#
#  1. A second Ctrl-C while the Rust side is still compiling can detach
#     the Vite child and leave it owning :5173. The next `cargo tauri
#     dev` then blocks on the port.
#  2. If the Tauri runtime crashes during webview spinup, the spawned
#     `npm run dev` survives.
#
# This script puts everything in one process group, traps SIGINT/SIGTERM
# /EXIT, kills the group on cleanup, and double-checks port 5173 is
# free before returning. Result: Ctrl-C once, port is gone, no zombies.
#
# Usage
# -----
#     ./scripts/run_desktop.sh
#
# Optional env:
#   STOCKPILE_DEV_PORT — default 5173. Override if you change
#                        vite.config.ts's `server.port`.

set -euo pipefail

PORT="${STOCKPILE_DEV_PORT:-5173}"

# Resolve workspace root from this script's location so it's CWD-agnostic.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd)"
cd "$ROOT"

# ----- frontend deps ---------------------------------------------------
# tauri.conf.json's beforeDevCommand is "npm run dev", so we install
# with npm too. Skip if node_modules already exists.
if ! command -v npm >/dev/null 2>&1; then
  echo "[run_desktop] npm not found in PATH" >&2
  exit 1
fi

if [ ! -d "$ROOT/apps/desktop/node_modules" ]; then
  echo "[run_desktop] installing frontend deps with npm…"
  (cd "$ROOT/apps/desktop" && npm install)
fi

# Ensure SvelteKit's generated files exist. Cheap and idempotent;
# without this, Vite warns about a missing ./.svelte-kit/tsconfig.json
# on the first dev start after a fresh checkout, and `src/app.html`
# resolution can race the kit-internal manifest.
(cd "$ROOT/apps/desktop" && npx svelte-kit sync >/dev/null 2>&1 || true)

# ----- cleanup hook ----------------------------------------------------
# Whatever path we exit through (graceful, signal, error), kill the
# whole process group and double-check the dev port is free.

cleanup() {
  local rc=$?
  trap - EXIT INT TERM HUP   # avoid recursive cleanup

  echo
  echo "[run_desktop] shutting down (exit $rc)…"

  # 1. Politely signal the whole group. -$$ targets pgid == our pid.
  if kill -0 -- "-$$" 2>/dev/null; then
    kill -TERM -- "-$$" 2>/dev/null || true
  fi

  # 2. Give children up to 3s to exit gracefully.
  for _ in 1 2 3; do
    if ! pgrep -g "$$" >/dev/null 2>&1; then break; fi
    sleep 1
  done

  # 3. Anything still alive in the group: SIGKILL.
  if pgrep -g "$$" >/dev/null 2>&1; then
    kill -KILL -- "-$$" 2>/dev/null || true
  fi

  # 4. Belt-and-suspenders: if anything is still listening on the dev
  #    port, find it by port and kill it. lsof is on both macOS and
  #    Linux and is the most portable tool for this.
  if command -v lsof >/dev/null 2>&1; then
    local pids
    pids="$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)"
    if [ -n "$pids" ]; then
      echo "[run_desktop] port $PORT still held by: $pids — killing"
      # shellcheck disable=SC2086
      kill -TERM $pids 2>/dev/null || true
      sleep 1
      # shellcheck disable=SC2086
      kill -KILL $pids 2>/dev/null || true
    fi
  fi

  echo "[run_desktop] done."
  exit $rc
}

trap cleanup EXIT INT TERM HUP

# ----- pre-flight: port is free ---------------------------------------
if command -v lsof >/dev/null 2>&1; then
  if lsof -tiTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    held_by="$(lsof -tiTCP:"$PORT" -sTCP:LISTEN | tr '\n' ' ')"
    echo "[run_desktop] port $PORT already in use (pids: $held_by) — leftover from prior run; freeing it."
    # shellcheck disable=SC2086
    kill -TERM $held_by 2>/dev/null || true
    sleep 1
    # shellcheck disable=SC2086
    kill -KILL $held_by 2>/dev/null || true
  fi
fi

# ----- start tauri ----------------------------------------------------
# We run the tauri CLI through npx so we don't depend on a global
# install. tauri.conf.json's beforeDevCommand is "npm run dev", which
# inherits our process group, so Ctrl-C reaches Vite too.
echo "[run_desktop] starting: npx tauri dev"
echo "[run_desktop] dev port: $PORT  (Ctrl-C to stop)"
echo

cd "$ROOT/apps/desktop"
npx tauri dev &
TAURI_PID=$!

# `wait` returns when the child exits OR when a signal arrives. Either
# way the EXIT trap above does the cleanup.
wait "$TAURI_PID"
