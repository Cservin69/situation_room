# Session 65 — Handoff

Session 65 opened on Session 64's candidate #2 — live-verify the Fed
coercion fix + close ADR 0012 Condition 5 — and discovered a
persistence bug that took the whole session. No code changed. The
session's product was a sharper diagnosis: an explanation of why
today's desktop writes are intermittent, ruling out three plausible
causes, and a concrete first move for Session 66.

## What Session 65 found

### Re-author flow works end-to-end in the UI

Operator's morning screenshot captured the moment we'd been chasing
since Session 64: federalreserve rate policy plan, recipe `019e1ffc`
failed @ apply with the verbatim Session-64 NHC predicate string
("inner selector matched no elements within iterator match… likely
cause: the inner selector is targeted at a sibling rather than a
descendant of the iterator's match"), the re-author button fires,
recipe `019e1fff` succeeds with 1 record. Recipe-history column
renders `019e1ffc → 019e1fff` oldest-to-newest, which means the
`prior_recipe_id` column was written. ADR 0012 Track A's UI surface
is alive on a real failure.

### …but the writes didn't persist between desktop sessions

`research_plans` table at session start carried 2 rows: ebola (May 11
11:54) + lithium (May 11 11:47). No federalreserve. No
`019e1ff*` recipes anywhere in the DB. By session end a third plan
existed — `019e2023-…` atlantic_hurricanes, classified today — proving
persistence is *intermittent*, not broken outright.

### Three hypotheses falsified

- **iCloud rollback.** Only xattr is `com.apple.provenance` (benign).
  PID 1148 holding a read fd is `com.apple.Virtualization.framework`
  — Cowork's Linux VM mount. No `com.apple.fileprovider.*` anywhere.
- **DuckDB silent rollback.** `insert_research_plan` in
  `crates/storage/src/research_plans.rs` is plain `conn.execute("INSERT
  …")` — no explicit BEGIN, autocommit on. The insert path is clean.
- **Run-desktop.sh's SIGKILL-after-3s.** Cmd-Q on the Tauri window
  bypasses the script's signal handlers entirely; the binary exits on
  its own and the script's `wait` returns. So that branch isn't the
  fault path on its own.

### Current best hypothesis

The desktop binary has **no SIGTERM handler**. The run_desktop.sh
script wires SIGINT/SIGTERM/EXIT traps to kill the process group, and
SIGTERM with no handler instant-kills the Rust binary — `Drop` never
runs, DuckDB's buffer pool never checkpoints to disk, writes lost.
**Cmd-Q on the Tauri window** with the script left untouched lets
AppKit→Tauri→`main()`-return→`Drop` proceed naturally — writes
checkpoint. **Ctrl-C in the script's terminal** routes through the
trap → process-group SIGTERM → instant kill. The May 11 plans + the
atlantic_hurricanes plan survived because they exited via the Cmd-Q
path. federalreserve + "graceful shutdown test 1" died because they
exited via the Ctrl-C path.

A `.wal` file never appearing on disk — even mid-session — is
consistent with this: DuckDB's Rust binding holds writes in the
buffer pool until checkpoint, which fires on `Drop`.

## Files changed

```
scripts/session65_verify.sql      (new — Condition 5 + Class B evidence queries)
scripts/session65_diag.sql        (new — row-count + prior_recipe_id IS NOT NULL diagnostic)
scripts/session65_schema.sql      (new — table-existence + migration-version snapshot)
SESSION_65_HANDOFF.md             (this file)
```

No `crates/`, `apps/`, `docs/adr/`, or `migrations/` changes. The
Session 64 schema-aware coercion fix in `recipe_apply.rs` and the
`recipes_with_extracted_inner` instrumentation in `eval_harness/main.rs`
remain shipped and unit-tested but unverified live.

## Verification gate

- `cargo test --workspace`: not re-run (no code changed).
- Operator's morning UI screenshot is the only live verification of
  the re-author flow on real data; the DB it ran against did not
  preserve that state.

## Workaround until Session 66

**Cmd-Q the Tauri window. Do not Ctrl-C the run_desktop.sh terminal.**
Let the script's `wait` return on its own once the binary exits; the
cleanup trap fires harmlessly with all children already gone. This
preserves DuckDB's checkpoint path.

## Session 66 candidates

In ADR-discipline order (the persistence bug blocks everything else
ADR 0012- and ADR 0019-related until it's fixed):

1. **Install a SIGTERM/SIGINT handler in
   `apps/desktop/src-tauri/src/main.rs`.** A `tokio::signal::ctrl_c()`
   (or the equivalent `tauri::RunEvent::ExitRequested`) hook that
   drops `AppState` before `std::process::exit` — fewer than 20 lines.
   Unblocks every subsequent live-verification candidate. The fix
   itself is small enough not to need an ADR; the bug it fixes is
   ADR-territory by virtue of how easy it is to lose data without it.

2. **Re-verify the Session 64 Fed coercion fix live.** With #1
   landed, re-classify a Fed plan, trigger the coercion failure (or
   reuse whatever shape the proposer picks today), re-author, query
   for the `prior_recipe_id` chain. This closes Session 64's
   candidate #2 and ADR 0012 Condition 5's "verified in a real run"
   half in one pass.

3. **Author the federalreserve.gov Class B case** — also unblocked by
   #1. The morning screenshot already shows the predicate string
   verbatim; the case file needs the captured bytes from
   `recipe_fetch_attempts.bytes_excerpt` (queryable post-fix) to
   ground spec. This adds host-diversity to the CssSelect
   inner-no-elements predicate (Session 64 added 4 NHC cases; the Fed
   case breaks the host-monoculture).

4. **Hunt the JsonPath + CsvCell strict Class B cases** (Session 64
   candidate #3). A different plan topic likely produces these
   naturally. Run with `--keep-dbs` on the eval harness so the cases
   land spec-grounded.

5. **Reasoning-block-before-JSON prompt experiment** (Session 64
   candidate #4). Prompt-only, not the loop; unblocked by ADR 0012.

6. **The loop itself** when Conditions 1–5 are all green. Probably
   Session 68–69 at current rate.

End of handoff.
