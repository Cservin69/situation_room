# Session 48 — Patch 1

Both operator-introspection surfaces from the Session 48 handoff,
bundled per the operator's explicit "larger scope, in one go"
override of the handoff's "one piece, one tarball" rule. Piece B
(per-host backoff status panel) and piece C (sources-memory panel)
share the read-only IPC + Svelte-panel + plans-store-refresh shape
the Session 46 patch established; pairing them in one commit reuses
the wiring without doubling the prose surface.

Piece A (the Session 47 live-run observation pieces) is deferred —
it requires a live cargo run against the lithium fixture and is the
operator's, not the agent's, to drive.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

Two new ts-rs DTO derives (`SourcesMemoryEntryDto`; the
`HostBackoffSnapshotDto` was already shipped by Session 46 as a
drive-by). Two new IPC commands (`host_backoff_state`,
`sources_memory`). Two new Svelte components. Pure additive across
the workspace; no schema change, no migration, no LLM-path edits, no
prompt edits.

## What's intentionally not in this patch

- **The recipe-author prompt is unchanged.** v1.15 stands. Both
  surfaces here are pure reads over runtime state; no LLM is called
  by either path.
- **No source-specific routing anywhere.** The `host_backoff_state`
  command reads what the runtime adaptation layer has *observed*; it
  does not configure behaviour. The `sources_memory` command reads
  the same rows the classifier already consumes; the panel does not
  filter, prioritise, or curate.
- **No new closed-vocabulary entries.** Both DTOs reuse fields that
  already exist on the storage / pipeline side.
- **No backfill.** `host_backoff_state` is process-state (resets on
  binary boot); `sources_memory` reads the same view the classifier
  reads (no historical reconstruction).

## Files changed

### API DTOs

- `crates/api/src/types_export.rs` —
  - New `SourcesMemoryEntryDto` ts-rs derive next to the existing
    `HostBackoffSnapshotDto`. Mirrors
    `situation_room_storage::MemorySource` with two principled
    renames (`endpoint_url` → `url`, `last_attempted_at` →
    `last_succeeded_at`); the storage column names predate the
    HAVING clause that filters to successes only, so the wire shape
    uses what the values actually mean.
  - `SourcesMemoryEntryDto::from_typed` lifts the storage row into
    wire shape. Pure renaming; no field aggregation.
  - Three new tests pinning the DTO shape:
    - `sources_memory_entry_dto_renames_storage_fields_session_48` —
      round-trip of every field through `from_typed`.
    - `sources_memory_entry_dto_serializes_with_renamed_fields_session_48`
      — JSON form carries `url` / `last_succeeded_at`, never the
      storage names. Guards against a future serde `rename` flip.
    - `host_backoff_snapshot_dto_collapses_duration_to_seconds_session_48`
      — pins the `Duration → u64` collapse from Session 46's
      `from_typed` so a future ts-rs Duration support widening
      doesn't accidentally pass through sub-second precision.
    - `host_backoff_snapshot_dto_zero_wait_recovering_state_session_48`
      — the "recovering" state (counter > 0, wait == 0) round-trips
      with both fields visible on the wire.

### API IPC commands

- `crates/api/src/commands.rs` —
  - Imports widened: `HostBackoffSnapshotDto` and
    `SourcesMemoryEntryDto` added to the `types_export` use list.
  - **New IPC command `host_backoff_state()` →
    `Vec<HostBackoffSnapshotDto>`** (Session 48 piece B). No
    parameters; pure read over `state.host_backoff.snapshot()`. The
    accessor is infallible; the command's only error path is the
    typed wire shape, which can't be reached today. Documents the
    three operator-readable states (clean / recovering / blocked) so
    a future `consecutive_failures` / `wait_seconds_remaining`
    refactor stays accountable to the contract.
  - **New IPC command `sources_memory()` →
    `Vec<SourcesMemoryEntryDto>`** (Session 48 piece C). No
    parameters; pure read over
    `Store::sources_memory(SOURCES_MEMORY_LIMIT)`. The same cap the
    classifier reads, in the same order. ADR 0007 §"runtime path"
    cited in the doc-block — the surface mirrors what's stored, no
    curation in this command.
  - Five new tests (in the existing `tests` module):
    - `host_backoff_state_maps_snapshot_into_dtos_session_48` — two
      hosts with different signals (Retry-After-honoring 429 vs.
      timeout) round-trip into DTOs with the correct counter and
      wait values. Tests assert per-host (not positional) because
      HashMap iteration order is unspecified.
    - `host_backoff_state_empty_snapshot_yields_empty_vec_session_48`
      — fresh-boot accessor returns an empty Vec; the panel's
      empty-state hint is the legitimate state.
    - `sources_memory_command_maps_storage_rows_into_dtos_session_48`
      — a single (URL, source_id) pair with one successful attempt
      round-trips through the same Store the command uses; the wire
      DTO carries the renamed fields and the topic-tag aggregation.
    - `sources_memory_command_empty_store_yields_empty_vec_session_48`
      — fresh installation surfaces no rows; cold-start contract.
    - `sources_memory_command_filters_to_successes_only_session_48`
      — a recipe with only failed attempts must not surface.
      Mirrors the storage layer's existing
      `returns_only_sources_with_at_least_one_success` test, but at
      the DTO-mapping layer.

### Tauri composition root

- `apps/desktop/src-tauri/src/main.rs` —
  - Two new commands registered in the `tauri::generate_handler!`
    macro: `host_backoff_state`, `sources_memory`. Comment block
    names them as Session 48 operator-introspection surfaces over
    network-layer + classifier-grounding state; "no LLM call, no
    fetch."

### Frontend types + client

- `apps/desktop/src/lib/api/types/SourcesMemoryEntryDto.ts` (new) —
  the ts-rs-generated TS file mirroring the new derive. Also
  regenerated by ts-rs at the next `cargo test --package
  situation_room-api`; pre-populated here so `npm run check` can run
  before cargo if the operator's terminal order is reversed.
- `apps/desktop/src/lib/api/client.ts` —
  - Two new typed wrappers: `hostBackoffState()` and
    `sourcesMemory()`. Both take no parameters; both return the
    typed DTO array. Long-form doc-blocks describe the three-state
    vocabulary (piece B) and the cold-start contract (piece C).
  - Type imports added for the two DTOs.

### Plans store

- `apps/desktop/src/stores/plans.svelte.ts` —
  - State extended with `hostBackoff: HostBackoffSnapshotDto[]` and
    `sourcesMemory: SourcesMemoryEntryDto[]`. Both initialise to
    empty arrays.
  - **New polling lifecycle.** Module-level constant
    `HOST_BACKOFF_POLL_INTERVAL_MS = 5000`. Module-level timer
    handle `hostBackoffPollTimer`. New helpers
    `startHostBackoffPolling()` / `stopHostBackoffPolling()`. The
    polling cadence is documented (5s = at least one mid-run snapshot
    under the typical 10–60s end-to-end fetch). The timer survives
    component re-mounts within the same selection because
    `$effect.root` lifetime is per-component, not per-selection;
    keying the timer to the selection (`selectPlan` /
    `clearSelection`) is the right scope.
  - `selectPlan` calls `refreshHostBackoff` + `refreshSourcesMemory`
    fire-and-forget, then `startHostBackoffPolling()`.
  - `clearSelection` resets both surfaces and calls
    `stopHostBackoffPolling()`.
  - `runFetch`'s success path calls `refreshHostBackoff` and
    `refreshSourcesMemory` so a freshly-landed signal surfaces
    immediately rather than waiting for the next poll tick.
  - Two new exported helpers: `refreshHostBackoff()` and
    `refreshSourcesMemory()`. Same non-fatal posture as
    `refreshFetchRuns` / `refreshRecipes` — preserve the previous
    list on transient failure.

### Svelte components

- `apps/desktop/src/components/HostBackoffStatus.svelte` (new) —
  - Reads `plans.hostBackoff`. Renders one row per host with the
    host string, the three-state label (clean / recovering /
    blocked), the consecutive-failures counter, and the
    seconds-remaining wait.
  - **Tone-driven left border** mirroring FetchReport's row-border
    convention: `--signal-positive` for clean, `--signal-warning`
    for recovering, `--signal-negative` for blocked. The state
    label uses the same tone colour so the row's identity is legible
    in monochrome.
  - **Both fields are explicitly rendered.** The handoff flagged
    "don't surface wait_remaining as the only signal" — the
    `consecutive_failures` counter sits next to the wait value with
    its own tooltip, so the recovering case (counter > 0, wait == 0)
    is visually distinct from the clean case (counter == 0, wait
    == 0) at a glance.
  - **Stable sort across polling refreshes.** Rows sort by tone
    (blocked first, then recovering, then clean), then alphabetical
    by host within a tone, so a re-render from the next poll tick
    doesn't shuffle the strip even though `HostBackoff::snapshot()`
    iterates a HashMap.
  - **Empty state** mirrors `RecipeOutcomesHeatmap`'s posture: a
    dim panel with the same chrome and a short explainer (`"No host
    signals observed this session. Run a fetch to populate."`)
    rather than a hidden surface.
  - `bigint → Number` coercion in `formatWait` — ts-rs v8+ emits
    Rust `u64` as TS `bigint`; the wait value is bounded by the
    schedule cap (60s), so the coercion is lossless in practice.

- `apps/desktop/src/components/SourcesMemoryPanel.svelte` (new) —
  - Reads `plans.sourcesMemory`. Renders one row per (URL,
    source_id) pair: the URL as an `external` link (target=_blank,
    rel=noopener noreferrer), the source_id as a small secondary
    line, the success count + last-success timestamp on the right,
    and the associated topic-tag chips on a wrap-row beneath.
  - **Identical contract with the classifier view.** Same data,
    same recency-sorted order, same cap. The header copy says
    "same as the classifier sees" so the operator knows the panel
    is the classifier's grounding, not a parallel curation.
  - **No polling.** Refresh on plan selection + after each
    successful `runFetch`. The data only changes when a fetch run
    lands a new success or re-fetches an existing URL; a 5s timer
    here would pay IPC cost for nothing.
  - **Empty state** explains the cold-start contract: "The
    classifier falls back to training knowledge until a fetch run
    lands a success." Matches the classifier prompt's worked
    examples.
  - **URL truncation** is purely visual; the `title` attribute and
    the link's `href` carry the full URL.

### PlanReview integration

- `apps/desktop/src/components/PlanReview.svelte` —
  - Imports `HostBackoffStatus` and `SourcesMemoryPanel`.
  - **`HostBackoffStatus` slotted at the top** of the review pane,
    immediately after the header and before the trust paragraph. The
    handoff calls for "top of `PlanReview.svelte` (or in a sibling
    status strip)"; placing it *inside* the article preserves the
    review pane's existing visual scrollbar and keeps the network-
    layer state visually adjacent to the plan it pertains to.
  - **`SourcesMemoryPanel` slotted at the bottom**, after
    `RecipesPanel`. The vertical scan order now reads: live fetch
    report → outcome history heatmap → expectation coverage matrix
    → recipes themselves → classifier sources memory. The memory
    panel sits at the end because it's whole-system context (not
    plan-specific) — leaving it at the foot of the pane mirrors how
    the classifier consumes it (as background context, not
    foreground state).

## Design notes worth preserving

### Why polling for piece B but not piece C

`HostBackoff` state is process-internal and changes during a
synchronous fetch run (signals are recorded as the executor sees
429s / Retry-After headers / timeouts in real time). The fetch
runner doesn't return until all recipes complete; without polling
the strip would only update on the run's terminal handoff, blinding
the operator to mid-run behaviour. 5s is fast enough to surface
those signals before run completion.

`Store::sources_memory` only changes when a fetch run lands a
success or re-fetches an existing URL. Both events are gated on the
synchronous `runFetch` handler's success path; refreshing alongside
that handler covers the change window without a background timer.

### Why module-level timer state rather than a Svelte effect

The timer handle lives at module scope (rather than inside
`HostBackoffStatus.svelte`'s `onMount` / `$effect`) so the polling
lifecycle is tied to the **selection**, not the **component
instance**. A future PlanReview re-mount within the same selection
(e.g. a Tauri menu refresh, a hot-reload during dev) would otherwise
either restart the timer (creating duplicates) or stop it. Keeping
the timer at the store level — with `selectPlan` /
`clearSelection` as the lifecycle hooks — keeps the cadence stable
across UI churn.

### Why `host_backoff_state` doesn't take a plan_id

The state is process-global (one `HostBackoff` per `AppState`,
shared across every fetch). A plan-scoped variant would be possible
(filter to hosts touched by *this plan's* recipes) but would couple
the panel to the plan-recipe-host walk and surface a different shape
for a fresh-pasted URL than for an existing recipe. The simpler
"this binary's session" framing is honest and matches the layer's
own scope.

### What `consecutive_failures > 0, wait == 0` (recovering) means

The schedule expired without an intervening success, so the next
request to this host fires immediately, but the failure counter is
still in effect — the next observed failure will start the schedule
at the *next* exponential step rather than at 1s. The handoff
explicitly called this out as the wrong-fix risk: collapsing it
into "clean" would hide a meaningful lead-indicator.

## Test deltas

- `crates/api/src/types_export.rs` — 4 new tests (2 for the new
  SourcesMemoryEntryDto serialisation, 2 for the existing
  HostBackoffSnapshotDto's `from_typed`).
- `crates/api/src/commands.rs` — 5 new tests (2 for the
  host-backoff command's mapping, 3 for the sources-memory
  command's mapping). All exercise the same accessor / Store the
  Tauri commands use, so the assertions cover the wire shape end-
  to-end without needing a Tauri runtime.

Pipeline test count: unchanged at 335. API test count: rises by 9.
Other crates' counts unchanged. All ignored tests (12) remain the
existing `#[ignore]` live integration tests.

## What's NOT in scope

- **Live-run observation of Session 47.** Piece A from the Session
  48 handoff. Requires a live cargo run against the lithium fixture
  and is the operator's, not the agent's, to drive. The
  `failure_cases/recipe_author/` and
  `docs/observations/2026-XX-XX-session-47-live-run.md` artefacts
  are deferred to a future session that observes the run.
- **Per-plan filtering on the host-backoff panel.** The layer's
  state is process-global; a per-plan view would require walking the
  plan's recipes' hosts to narrow. Out of scope today; the
  whole-session view is the honest summary.
- **Persistence of host-backoff state across binary restarts.**
  Same posture Session 45 chose — the state is short-lived (rate-
  limit windows, transient timeouts) and a fresh start on each boot
  is the right default. Persisting would mean serialising observed-
  signal history into the DB; not earned today.
- **Topic-overlap filtering on the sources-memory panel.** ADR 0015
  §"Memory query" defers it; the chicken-and-egg problem (the
  classifier hasn't picked topics yet at memory-read time) still
  applies on the operator side. The current top-30-by-recency
  surface is what the classifier reads.
- **Promotion pipeline (ADR 0004).** Substantial piece. Its own
  handoff. The handoff before that one will need to spend time on
  the authoritative-vs-consensus design choice.
- **Iterator Phase 2 (ADR 0016).** Its own session.
- **Charts on Observations / Events.** Hold until promotion lands.
- **xAI Responses API migration.** Only if a live `grok-4.3` run
  shows chat/completions silently ignoring `reasoning_effort`.

## Hard rules carried over

Same as Sessions 41–47:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. This patch adds none.
- ADR 0009: every HTTP call goes through `SecureHttpClient`. This
  patch adds no HTTP calls.
- Bounds checking on every IPC string input. Both new commands take
  no string inputs.
- Tauri commands return `CommandError`. Both do.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; never hand-edit. The new `SourcesMemoryEntryDto.ts` is
  the ts-rs-shaped output, regenerable at the next `cargo test`.
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`. The store extension here
  is in `plans.svelte.ts` per the existing convention.
- L1 prompt edits come from observed classifications, not
  speculation. This patch edits no prompts.
- **Stockpile prompts: principle-only language.** Untouched here.
- **Do not write code to pass tests.** Every new test pins a
  contract the new commands explicitly hold.

End of patch.
