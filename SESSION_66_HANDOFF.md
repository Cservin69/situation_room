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
crates/secure/src/bounds.rs                            (+~12 lines: SELECTOR_TRACE = 4_096)
config/prompts/recipe_author.md                        (header v1.20 → v1.21; selector_trace section in What-to-produce; changelog entry)
Cargo.toml                                             (+`signal` feature on tokio)
docs/failure_cases/class_b/2026-05-13_www_federalreserve_gov_css_inner_no_elements.md   (new)
docs/failure_cases/class_b/README.md                   (gate-status table: 5 → 6, host diversity noted)
scripts/session66_verify.sql                           (new — Q0..Q4 over the live-bin DB)
session66-hunt-classB.sh                               (new — FEMA + BLS topic eval runs)
session66-v121-eval.sh                                 (new — v1.21 hurricane re-run vs v1.20 baseline)
SESSION_66_HANDOFF.md                                  (this file)
```

## Verification gate

- `cargo test --workspace`: **not run from sandbox** per
  `memory/workflow_cargo_mac.md` (proxy blocks crates.io /
  sh.rustup.rs). Operator runs on Mac via
  `session66-v121-eval.sh` step [1/2] or directly. Expected new
  passing tests: `checkpoint_on_fresh_in_memory_store_succeeds`,
  `checkpoint_durably_flushes_buffer_pool`,
  `empty_selector_trace_accepted`,
  `under_bound_selector_trace_accepted`,
  `over_bounded_selector_trace_is_invalid`.

- **Persistence-fix live verification** is operator-runnable on Mac
  via `scripts/session66_verify.sql`. Procedure: launch
  `./scripts/run_desktop.sh`, classify a plan, accept, run fetch,
  **Ctrl-C the terminal hosting run_desktop.sh** (the path that
  lost writes pre-fix). Expected: new signal-shutdown task logs
  "received shutdown signal — checkpointing DuckDB then asking
  Tauri to exit" before exit. Restart desktop; today's plan
  must be in `research_plans` per Q1 of the script.

- **ADR 0012 Condition 5 + Fed Class B case bytes** are operator-
  derivable in the same desktop session once persistence is
  verified (Q2 + Q3 + Q4 of `session66_verify.sql`). Both depend
  on the persistence fix surviving; both can be filled in within
  one desktop session after that.

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
