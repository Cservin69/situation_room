# Session 66 — Handoff

Session 66 opened on Session 65's diagnostic-only conclusion: the
persistence bug had to land before anything ADR-0012- or
ADR-0019-related could be live-verified. Five things landed in code;
two of them need operator live runs on Mac to graduate from
"shipped" to "verified."

## What Session 66 changed

### Fix — signal-driven shutdown for the desktop binary

[`apps/desktop/src-tauri/src/main.rs`](apps/desktop/src-tauri/src/main.rs)
splits `tauri::Builder::default().run(…)` into the build / run pair,
captures a `Weak<Store>` before the App takes ownership, and spawns
a `std::thread` running a current-thread tokio runtime whose sole
job is to wait on SIGTERM + SIGINT, upgrade the weak ref, issue
`Store::checkpoint()`, and call `app_handle.exit(0)` for the clean
Tauri tear-down.

**Why `Weak<Store>` and not `Arc<Store>`:** a strong Arc held by the
signal task would keep the Store alive past App's Drop on the Cmd-Q
exit path (no signal ever arrives, the task parks forever, the
strong-ref population stays at 1, AppState's Drop fails to free the
Connection). With a Weak, the upgrade returns `None` once the
strong refs drain on Cmd-Q, the signal task's branch becomes a
no-op, and `Connection::drop` checkpoints normally as it did before
Session 65 — the working path stays working.

[`crates/storage/src/connection.rs`](crates/storage/src/connection.rs)
gains `Store::checkpoint()`. The method runs `CHECKPOINT;` against the
held connection and returns `Result<()>`. Two unit tests cover the
empty-store and write-then-checkpoint paths; the strict
"checkpoint persists writes through an SIGTERM-shaped kill" proof
is operator-runnable only (see verification gate below).

[`Cargo.toml`](Cargo.toml) adds the `signal` feature to the
workspace tokio dep so `tokio::signal::unix::signal` resolves.

The Tauri-Cmd-Q path (which worked pre-fix) is unchanged: AppKit →
Tauri → `app.run` returns → App drops → managed AppState drops →
Store drops → DuckDB's `Connection::drop` checkpoints. Only the
SIGTERM/SIGINT path is rewired.

`#[cfg(unix)]` gates the signal task. On Windows, no installation;
the fix-on-Mac scope of this session doesn't need it.

### Fix-companion — federalreserve.gov Class B case

[`docs/failure_cases/class_b/2026-05-13_www_federalreserve_gov_css_inner_no_elements.md`](docs/failure_cases/class_b/2026-05-13_www_federalreserve_gov_css_inner_no_elements.md)
is the case file for the Session 65 morning-screenshot failure
(recipe `019e1ffc` → re-author → `019e1fff` succeeds with 1
record). Spec + bytes sections are TBD pending Session 66 desktop
re-run because the Session 65 persistence bug wiped the DB. The
predicate string `selector matched no elements` is captured
verbatim. The case adds **host diversity** to ADR 0012's CssSelect
evidence: the prior four cases are all `www.nhc.noaa.gov`; this
one is the first non-NHC entry.

[`docs/failure_cases/class_b/README.md`](docs/failure_cases/class_b/README.md)
gate-status table updated. Strict Class B total **5 → 6**. Mode
diversity unchanged (still CssSelect + RegexCapture; JsonPath +
CsvCell still empty).

### Hunting infrastructure for JsonPath + CsvCell cases

[`session66-hunt-classB.sh`](session66-hunt-classB.sh) runs the
eval-harness against two topics chosen to maximise the chance of
producing JsonPath and CsvCell strict cases:

- **FEMA disaster declarations 2025** — `api.fema.gov` publishes
  JSON; expectation is the LLM authors `json_path` recipes.
- **US monthly unemployment by state 2025** — BLS / data.census.gov
  publish CSV; expectation is `csv_cell` recipes.

Five trials each, `--keep-dbs` retained. The script ends with
inline SQL the operator runs against the per-trial DBs to find any
`failure_message LIKE '%path matched no nodes%'` or
`'%no row matched filter%'` rows. Each match grounds a new strict
Class B case file.

This is speculative: the LLM may decline either topic at URL-propose,
or it may author CssSelect against an HTML index even when a JSON /
CSV endpoint exists. Session 66 ships the script; Session 67 reads
the results.

### Prompt experiment — recipe_author v1.21 (selector_trace)

[`config/prompts/recipe_author.md`](config/prompts/recipe_author.md)
header bumps `v1.20` → `v1.21`. A new "selector_trace" field is
added to the top-level shape in the "What to produce" section, with
explicit walk-through-the-descendant-check instructions for
iterator-bearing recipes. The changelog entry at the bottom of the
prompt names the motivating data (Session 64 hurricane eval 2/5
trials with `selector matched no elements`; Session 65 fed
screenshot's same predicate on a different host).

[`crates/pipeline/src/recipe_author.rs`](crates/pipeline/src/recipe_author.rs)
adds `selector_trace: String` as the **first** field on
`RecipeAuthoringOutput`. Schemars derives the JSON-Schema; first-
position means the LLM emits the trace before any other field —
the closest equivalent to "reasoning block before JSON" available
under strict-output constraints (JSON-schema-strict providers honor
declaration order). Empty-string-as-absent, same idiom as
`static_payload` / `decline_reason`. The validator at
`build_validated_recipe` enforces only the
[`Bounds::SELECTOR_TRACE`](crates/secure/src/bounds.rs) = 4 096
char cap; trace content is not parsed, not persisted on the
FetchRecipe, not consulted at apply time. Three unit tests:
empty-accepted, under-bound-accepted, over-bound-rejected. Two
existing `RecipeAuthoringOutput { … }` fixtures updated.

[`session66-v121-eval.sh`](session66-v121-eval.sh) runs the v1.21
prompt against the v1.20 hurricane baseline (Session 64 JSONL
remains the reference). `cargo test --workspace` first, then 5
trials with `--keep-dbs`. The footer prints the comparison SQL.

The Session 56 variance lesson stands: 5 trials at v1.21 vs 5 at
v1.20 might not detect the effect. If results are inconclusive
operator should pool to 10+ trials or compare across topics
(reasoning-block-before-JSON is mechanism-neutral for apply, so
it should generalize across topics if it helps at all).

### Fix — re-author decline UX (live-test bug from operator session)

Operator's first re-author attempt on the Fed plan triggered the
LLM's decline channel (`AuthoringError::Declined`) — the LLM looked
at the captured bytes + the failing recipe's selectors + the
"inner selector matched no elements" predicate and honestly
concluded that no css_select fix was possible. The pre-Session-66
code squeezed declines through `CommandError::ReauthorFailed` with
a `[declined]` prefix on the message; the frontend treated it as a
generic error, the dialog stayed open as if the IPC had crashed,
and no new recipe was persisted. Operator described it as "screen
stuck, it went for LLM talk but did not close, just reappeared
with the same message." The fix lands in three layers:

- **Backend** — `crates/api/src/commands.rs` gains
  `CommandError::ReauthorDeclined { prior_recipe_id, reason }`.
  `reauthor_recipe`'s `AuthoringError::Declined` match-arm returns
  this variant instead of `ReauthorFailed[declined]`. The original
  Session-29 follow-up comment is replaced with the new architecture
  note explaining why declines are not failures. Two unit tests pin
  the wire shape (Rust enum + DTO mirror).

- **Wire** — `crates/api/src/types_export.rs` mirrors the variant
  on `CommandErrorDto`; ts-rs regenerates
  `apps/desktop/src/lib/api/types/CommandErrorDto.ts` with the new
  union branch on `cargo test`. Manually patched in this session so
  the frontend code compiles before the operator's next cargo run.

- **Frontend** — `apps/desktop/src/stores/plans.svelte.ts`'s
  `reauthorRecipe` now returns a discriminated `ReauthorOutcome`
  (`'ok' | 'declined' | 'error'`). Declines do *not* set
  `plans.error` (a banner would read as a crash) — they populate a
  new per-recipe map, `plans.recipeReauthorDeclines: Record<recipe_id,
  reason>`. Both `FetchReport.svelte` and `RecipesPanel.svelte` close
  the dialog on either `ok` or `declined`, and render a per-row
  `[declined: <reason>]` badge in place of the `re-author` button
  on rows where the LLM has previously declined. Clicking re-author
  again on the same recipe would yield the same decline; the badge
  replacing the button is the honest signal.

The fix unblocks Track A on sources where the LLM *correctly*
identifies that a css_select fix isn't possible (federalreserve.gov
press-releases listing was the live example — the failing recipes
target shapes that aren't in the bytes, and the LLM honestly says
so). Subsequent re-author attempts on different failed recipes,
or with operator diagnosis notes that nudge the LLM's framing,
still flow through the normal authoring path.

### Verification scripts

- [`scripts/session66_verify.sql`](scripts/session66_verify.sql)
  is the post-restart query runbook: schema_migrations sanity,
  research_plans roster, ADR 0012 Condition 5 (prior_recipe_id
  chain), and the Fed-Class-B evidence (failure_message + bytes_
  excerpt against `federalreserve.gov`).

## Files changed

```
apps/desktop/src-tauri/src/main.rs                     (+~115 lines: build/run split, signal task, shutdown_on_signal helper)
crates/storage/src/connection.rs                       (+~80 lines: Store::checkpoint + 2 unit tests)
crates/pipeline/src/recipe_author.rs                   (+~120 lines: selector_trace field + bounds check + 3 unit tests + fixture updates)
crates/api/src/commands.rs                             (+~80 lines: ReauthorDeclined variant, AuthoringError::Declined match arm rewrite, unit test)
crates/api/src/types_export.rs                         (+~30 lines: CommandErrorDto::ReauthorDeclined mirror + unit test)
crates/secure/src/bounds.rs                            (+~12 lines: SELECTOR_TRACE = 4_096)
config/prompts/recipe_author.md                        (header v1.20 → v1.21; selector_trace section in What-to-produce; changelog entry)
Cargo.toml                                             (+`signal` feature on tokio)
docs/failure_cases/class_b/2026-05-13_www_federalreserve_gov_css_inner_no_elements.md   (new, spec+bytes filled in from Session 66 live verify)
docs/failure_cases/class_b/README.md                   (gate-status table: 5 → 6, host diversity noted)
scripts/session66_verify.sql                           (new — Q0..Q4 over the live-bin DB; column-name fix `endpoint_url` → `source_url`)
session66-hunt-classB.sh                               (new — FEMA + BLS topic eval runs)
session66-v121-eval.sh                                 (new — v1.21 hurricane re-run vs v1.20 baseline)
apps/desktop/src/lib/api/types/CommandErrorDto.ts      (regenerated by ts-rs on cargo test; patched manually to add `reauthor_declined`)
apps/desktop/src/lib/api/client.ts                     (reauthorRecipe doc-comment names the new throw variant)
apps/desktop/src/stores/plans.svelte.ts                (ReauthorOutcome discriminated union; recipeReauthorDeclines map; reauthorRecipe handles the declined variant; statusFilter default `pending` → `all`)
apps/desktop/src/components/FetchReport.svelte        (onReauthorSubmit handles 3-state outcome; failed-row renders decline-badge in place of button when prior decline recorded; .decline-badge CSS)
apps/desktop/src/components/RecipesPanel.svelte       (same shape as FetchReport: 3-state outcome + decline-badge on the recipe head row)
apps/desktop/src/components/PlanFilterStrip.svelte    (stale comment about default Pending → All)
apps/desktop/src/routes/+page.svelte                  (blank-canvas home view replaces cross-plan RecordsDashboard; refreshGlobalRecords no longer called on mount)
SESSION_66_HANDOFF.md                                  (this file)
```

## Verification gate

- **`cargo test --workspace`: green** on operator's Mac
  (2026-05-13). Five new tests passed:
  `checkpoint_on_fresh_in_memory_store_succeeds`,
  `checkpoint_durably_flushes_buffer_pool`,
  `empty_selector_trace_accepted`,
  `under_bound_selector_trace_accepted`,
  `over_bounded_selector_trace_is_invalid`. Plus the two
  ReauthorDeclined wire-shape tests in `commands.rs` and
  `types_export.rs`.

- **Persistence fix verified live.** Operator's first
  classify→fetch→Ctrl-C→restart cycle on the Fed plan: post-restart
  Q1 returned the Fed plan + the Session-65 survivors. Pre-fix this
  Ctrl-C path wiped the day's writes; post-fix every write
  persisted across restarts.

- **Re-author decline UX verified live.** Both Fed re-author
  attempts landed `CommandError::ReauthorDeclined`; both dialogs
  closed cleanly; italic `declined:` badges replaced the
  `re-author` buttons on the corresponding failed rows. Pre-fix
  path would have stuck the dialogs reappearing with the failure
  message. The new wire variant works end-to-end against real LLM
  declines on a real source.

- **Fed Class B case fully spec-grounded.** Both failing recipe
  ids named, all four candidate spec pairs captured, 1.6 KiB of
  real fetched bytes pasted, re-authoring outcome documented
  ("LLM declined twice — Track B at re-author time"). Reduces to
  zero TBDs in the case file as of Session 66.

- **ADR 0012 Condition 5 is still open.** Both re-authors took
  the decline path, so no `prior_recipe_id`-stamped row was
  persisted. Q2 of `session66_verify.sql` returns 0 rows.
  Subsequent live verification of Condition 5 is unblocked by the
  persistence fix; a different plan (likely the 2025 atlantic
  hurricane season, whose NHC failures had simpler shapes) is the
  natural next candidate. Not load-bearing for Session 66 — the
  retry loop that Condition 5 gates is still blocked by
  Conditions 1, 2, 4 anyway.

- **v1.21 prompt eval still pending.** The
  `session66-v121-eval.sh` 5-trial hurricane run hasn't executed.
  v1.20 baseline (Session 64) remains the comparison anchor.

## What Session 66 did NOT do

- **Did not run the loop.** ADR 0012's gate (≥10 cases, ≥3 modes,
  Condition 4 case, Condition 5 live) is still unmet: 6 strict
  cases, 2 modes. The Fed case adds host diversity but not mode
  diversity. The loop stays gated.
- **Did not change ADR 0009.** Browser-UA endpoint expansion is
  still ADR-territory and the operator memory flags
  `feedback_no_easy_wins.md`: no surface patches. Defer.
- **Did not touch ADR 0019.** Phase 2A is Accepted; Phase 2B
  (csv_cell + pdf_table + regex_capture inner) is the next bound,
  but only worth touching after the eval-harness shows Phase 2A's
  authoring rate is actually limiting records. v1.21 selector_trace
  is mechanism-orthogonal to ADR 0019.

## Session 67 candidates

In ADR-discipline order:

1. **Land Session 66's two pending verifications.** Run
   `session66-v121-eval.sh` (catches test pass + v1.21 eval).
   Run desktop, do a Ctrl-C, restart, run `session66_verify.sql`
   Q0..Q4. Fill in the Fed Class B case's TBD spec + bytes.
   Update ADR 0012 §"Condition 5" to "Verified" if Q2 returns ≥1
   non-NULL row.

2. **Read the FEMA + BLS hunt results.** Run
   `session66-hunt-classB.sh`, then the footer SQL on the
   per-trial DBs. Each match → a new case file. Goal: at least
   one strict JsonPath case, at least one strict CsvCell case.
   Mode-diversity Condition 2 climbs from 2 to 3 (or 4).

3. **Compare v1.21 records distribution against v1.20.** Pool
   trials if 5 vs 5 is inconclusive. If v1.21 demonstrably reduces
   the inner-no-elements failure rate, ADR 0012's authoring-time
   bottleneck (Session 64 finding) is the right thing to keep
   targeting. If not, the next prompt experiment is open.

4. **Condition 4 case hunt** — a Class C disguised as Class B.
   The eval-harness candidates noted in
   `docs/failure_cases/class_b/README.md` (apnews JS-rendered hub)
   are still unexplored. Session 67 has the hunt script template
   from #2 above; clone-and-edit for the apnews-style topic.

5. **The loop.** Probably Session 68–69 at current rate: needs
   Condition 1 (≥10), Condition 2 (≥3 modes), Condition 3 (each
   predicate string ≥2 spec-grounded), Condition 4 (Class C
   disguised as Class B), Condition 5 (live verification, this
   session's Q2 may close it).

End of handoff.
