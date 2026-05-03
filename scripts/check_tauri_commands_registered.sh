#!/usr/bin/env bash
# check_tauri_commands_registered.sh — fail if any #[tauri::command]
# in `crates/api/src/commands*.rs` is not registered in the desktop
# binary's `tauri::generate_handler!` macro list.
#
# Background. Tauri 2 registers commands at runtime: the
# `#[tauri::command]` attribute makes a function *registrable*, not
# *registered*. The Rust side compiles fine when a command is missing
# from the macro list, the TypeScript side compiles fine because
# `invoke<T>('name', …)` is a string literal, and the mismatch only
# surfaces the first time the user clicks the thing that triggers
# the call — at which point Tauri returns "Command <name> not found".
#
# Session 22 added `records_for_plan` (storage query, DTO,
# #[tauri::command] function, frontend caller) but missed the macro
# registration in `apps/desktop/src-tauri/src/main.rs`. Session 23
# (which generalised the LLM provider) also missed it because the
# session's diff didn't touch the macro. The user only saw it in
# Session 23 because they exercised the records pane for the first
# time. This guard catches the class.
#
# Usage:
#   bash scripts/check_tauri_commands_registered.sh
# Exit codes:
#   0 — every #[tauri::command] in crates/api is registered.
#   1 — at least one isn't; the offending names are printed.
#   2 — usage / file-not-found error.
#
# Not-yet-CI'd: wire this into a make / just / xtask target before
# tagging a release. Cheap to run; sub-second on a normal repo.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMMANDS_GLOB="$REPO_ROOT/crates/api/src/commands*.rs"
MAIN_RS="$REPO_ROOT/apps/desktop/src-tauri/src/main.rs"

if ! ls $COMMANDS_GLOB > /dev/null 2>&1; then
    echo "error: no commands*.rs files found at $COMMANDS_GLOB" >&2
    exit 2
fi
if [ ! -f "$MAIN_RS" ]; then
    echo "error: $MAIN_RS not found" >&2
    exit 2
fi

# Pull every fn name immediately following `#[tauri::command]` (with
# optional whitespace and an optional `pub`/`pub(crate)`/`async`).
# `grep -A1` walks one line forward; `awk` strips down to the bare
# function name. Tolerates both `pub async fn name(...)` and
# `pub fn name(...)`.
declared=$(
    grep -hE -A1 '^[[:space:]]*#\[tauri::command\]' $COMMANDS_GLOB \
        | grep -E '^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+' \
        | sed -E 's/^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+([a-zA-Z0-9_]+).*/\3/' \
        | sort -u
)

# Pull every fn name registered in the generate_handler! macro. The
# entries look like `situation_room_api::commands::classify,` or
# `situation_room_api::commands_records::records_for_plan`. We just
# want the trailing identifier.
registered=$(
    awk '/generate_handler!\[/,/\]/' "$MAIN_RS" \
        | grep -E '^\s*situation_room_api::' \
        | sed -E 's,^\s*situation_room_api::[a-zA-Z0-9_]+::([a-zA-Z0-9_]+).*,\1,' \
        | sort -u
)

missing=$(comm -23 <(echo "$declared") <(echo "$registered") || true)

if [ -z "$missing" ]; then
    count=$(echo "$declared" | wc -l | tr -d ' ')
    echo "ok: all $count #[tauri::command] functions are registered."
    exit 0
fi

echo "error: the following #[tauri::command] functions are declared in" >&2
echo "       crates/api/src/commands*.rs but are NOT registered in" >&2
echo "       $MAIN_RS:" >&2
echo "" >&2
echo "$missing" | sed 's/^/  - /' >&2
echo "" >&2
echo "Add each one to the tauri::generate_handler![…] list. The path" >&2
echo "depends on which module the function lives in: commands::<name>" >&2
echo "for everything in commands.rs, commands_records::<name> for the" >&2
echo "records-rendering join, etc." >&2
exit 1
