# Session 23.1 — Amendment: register `records_for_plan` in the desktop `invoke_handler!`

> **Amendment to Session 23**, not a separate session. Everything from
> Session 23 (Anthropic provider, `AppState.provider` lift,
> `LLM_PROVIDER` env var, `.env.example` updates, the new docs) still
> applies; this patch ships the one-line fix Session 23 should have
> caught and a small CI guard so the class of mistake doesn't recur.

## What this fixes

After applying Session 23 and exercising the records pane on the
desktop UI for the first time, the operator hit:

    storage: Command records_for_plan not found

The error is from Tauri's IPC layer: the frontend invoked a
`#[tauri::command]` that the desktop binary's `invoke_handler!`
macro never registered. The command had been added in Session 22
(storage join in `crates/storage/src/queries.rs`, DTO in
`crates/api/src/records_dto.rs`, function in
`crates/api/src/commands_records.rs`, frontend caller in
`apps/desktop/src/lib/api/client.ts` and the runes store) — but
the corresponding line in the macro list at
`apps/desktop/src-tauri/src/main.rs` was missed.

Why neither the Rust nor TypeScript build caught it:

- `#[tauri::command]` makes a function *registrable*, not
  *registered*. Rust compiles fine when a `#[tauri::command]` fn
  isn't named in `generate_handler![…]`.
- TypeScript's `invoke<T>('records_for_plan', { id })` is a string
  literal; tsc has no idea which command names exist.
- The error only surfaces at runtime, the first time a user clicks
  the thing that calls the command.

Session 23 (mine) didn't notice because the records pane wasn't
exercised during my read of `main.rs`, and the Anthropic-provider
diff didn't touch the macro. The user surfaced it the first time
they scrolled into the run report after a real classify-and-fetch
run.

## Apply

From the repo root (`/Users/aben/RustroverProjects/situation_room`):

    tar -xzf ~/Downloads/session23_1_amendment.tar.gz --strip-components=1 -C .

This is **additive on top of Session 23's patch already applied**.
It supersedes Session 23's `apps/desktop/src-tauri/src/main.rs` (so
both `pick_provider` from Session 23 *and* the `records_for_plan`
registration from this amendment land together).

## What this ships

| File | Change |
|---|---|
| `apps/desktop/src-tauri/src/main.rs` | One new line in the `tauri::generate_handler![…]` macro list registering `situation_room_api::commands_records::records_for_plan`. The path uses `commands_records::`, not `commands::`, because the function lives in a sibling module added in Session 22. |
| `scripts/check_tauri_commands_registered.sh` | New shell guard: greps every `#[tauri::command]` fn out of `crates/api/src/commands*.rs`, greps every registered command out of `apps/desktop/src-tauri/src/main.rs`, fails non-zero if any declared command isn't registered. Prints the offending names with a clear remediation hint. Sub-second to run. Not yet wired into a make / just / xtask target — the followup is to plumb it into whatever pre-tag check the operator uses. |

## What this does NOT ship

- **No structural change to the macro registration model.** Tauri 2's
  `generate_handler!` is what it is; the macro list stays manual.
  The CI guard catches the mistake at check time rather than asking
  Tauri's macro to do something it doesn't.
- **No frontend changes.** The frontend was already calling the
  command correctly; the bug was that the backend didn't answer.
- **No prompt changes.** This is an unrelated category. See
  "On the stub-excerpt observation" below.
- **No new dependency.** The CI guard is a 60-line shell script that
  uses tools available on every Unix.
- **No new test.** The CI guard *is* the test; it lives outside the
  cargo test surface because it spans the Rust crate and the Tauri
  binary, which `cargo test` doesn't natively cross-check.

## Verification

After applying:

    bash scripts/check_tauri_commands_registered.sh
    # expected: "ok: all N #[tauri::command] functions are registered."

    cargo build -p situation_room-desktop
    # expected: clean build (the change is one line inside the macro)

    cd apps/desktop && npm run dev
    # in the UI: classify a topic, accept it, run fetch, scroll to
    # the records pane. The "storage: Command records_for_plan not
    # found" banner is gone; the records pane populates from the
    # join.

To deliberately break the guard and see the error message:
comment out one line in the `generate_handler!` list,
re-run the script, observe the message, then restore the line.

## Test count

No change. The amendment is a one-line registration plus a shell
script outside the cargo test surface. Expected total remains the
**401** Session 23 set (or whatever the prior baseline was; the
amendment doesn't change that count).

## On the stub-excerpt observation

Worth recording here even though it's not what this amendment
fixes, because it's what the operator will see once the records
pane works:

The Session 23 verification run on "south-korea elections"
nominated two sources (`rss_feeds`, `gdelt`) that both produced
**stub-authored** recipes (ADR 0014's chip showing). One reason
each:

- `rss_feeds` has no `endpoint_hint` in `config/sources.toml`, so
  the recipe author had only the description text to go on. It
  invented `https://www.yna.co.kr/rss` (Yonhap News's Korean RSS
  endpoint), which returned 400.
- `gdelt`'s `endpoint_hint` was rate-limited from the operator's IP
  with HTTP 429 during pre-fetch. The author fell back to the
  stub-excerpt path and produced a recipe extracting
  `$.articles[0].title` — a single news article title, technically
  successful at fetch time but useless against the plan's
  expectations (`polling_support`, `voter_turnout`, etc.).

This is the system working as designed: ADR 0014's STUB-AUTHORED
chip surfaced exactly the recipes that warranted operator
suspicion. **The fix is not in this amendment.** The two real
levers, both candidates for Session 24, are:

1. **Add `endpoint_hint` values to thin source descriptors.**
   Anything in `config/sources.toml` without a hint is one prompt
   away from this failure mode on every plan that nominates it.
   Cheap; pure config.
2. **Teach the recipe-author prompt to refuse rather than guess
   when only a stub excerpt is available.** Currently the prompt
   accepts the stub-excerpt path as a valid authoring input;
   making it surface a structured "no recipe" response would be
   stricter (no chip, no recipe, no false positive in the records
   pane). This is an ADR-amendment-shaped decision (changes the
   semantics of stub-authored), not a session-shaped one.

Neither is in scope for this amendment. The corrected Session 24
handoff (shipped alongside this README) elevates these to P1.

## Hard rules honored

- ADR 0009 §"The rule": no HTTP touched, no client created.
- ADR 0011 plan lifecycle: untouched.
- ADR 0007 runtime invariant: untouched.
- API key handling: untouched.
- Generated TS files: untouched. The DTO shape didn't change; only
  the IPC dispatch table did.
- Migrations: untouched.
- The CI guard is opt-in (script in `scripts/`, not in `Cargo.toml`
  or any build hook). The operator decides whether to wire it into
  CI.

## Followups for next session

See `SESSION24_HANDOFF.md` (this patch ships the corrected
version, superseding the one shipped with Session 23).
