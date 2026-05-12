# Session 63 — Handoff

Session 63 shifted scope mid-stream. The session opened on the
v1.20-prompt hurricane re-run plan from Session 62's handoff (the
ADR 0019 acceptance gate). Operator pivoted to a user-visible
product gap: the records dashboard is plan-scoped, so every plan
switch hides every prior plan's records. The v1.20 prompt is
actually working — the operator's screenshot showed
`EVENTS 1` rendering on a hurricane plan that was mid-fetch —
but the EVENTS counter was a collapsed pill, not a typed panel, so
the visible improvement landed nowhere. Two gaps, one session.

## What Session 63 changed

### Backend — cross-plan records query

[`crates/storage/src/queries.rs`](crates/storage/src/queries.rs)
gains `Store::recent_records_global(limit)` returning the newest N
records of each type across **every plan**. No plan filter; no
recipe-routing substring match. The operator's view of "what have we
collected" doesn't reset every time a fresh classification lands.

The existing `list_*` helpers were refactored to share row-decoders
with new `list_*_recent` helpers — six new `decode_<type>_row`
functions own the table-specific column-index layout, so the
WHERE-filtered (plan-scoped) and LIMIT-bounded (global) paths share
the same envelope-reconstruction logic. Four new tests in
`queries.rs` pin the global query: empty-store, two-plan union,
limit-per-type, and legacy-provenance inclusion (the global query
is unfiltered; the per-plan query is recipe-routed — that's the
deliberate semantic difference).

### Backend — Tauri command

[`crates/api/src/commands_records.rs`](crates/api/src/commands_records.rs)
gains `records_recent_global(limit)`. Reuses the existing
`RecordsByPlanDto` wire shape — the per-type Vecs are bucket-shaped,
not plan-shaped, so the name is now a slight misnomer but the shape
is generic. Clamps operator-supplied limit to a sane ceiling
(`MAX_GLOBAL_LIMIT = 500`); defaults to `DEFAULT_GLOBAL_LIMIT = 200`
when `None`.

[`apps/desktop/src-tauri/src/main.rs`](apps/desktop/src-tauri/src/main.rs)
registers the command in `generate_handler![]`. The existing
`check_tauri_commands_registered.sh` guard will pass — every
`#[tauri::command]` in `commands*.rs` is wired in.

### Frontend — store + boot wire

[`apps/desktop/src/lib/api/client.ts`](apps/desktop/src/lib/api/client.ts)
adds the `recordsRecentGlobal(limit = 200)` invoke wrapper.

[`apps/desktop/src/stores/plans.svelte.ts`](apps/desktop/src/stores/plans.svelte.ts)
adds `plans.globalRecords: RecordsByPlanDto | null` plus a
`refreshGlobalRecords(limit)` helper. The lifecycle is **decoupled
from plan selection**: `selectPlan` / `clearSelection` leave the
field alone (the whole point of the cross-plan view is that prior
plans' records persist). Wired into `runFetch` so newly-produced
records surface inside the synchronous fetch-button handler's
lifetime, and into `onMount` in `+page.svelte` so the dashboard has
data on first render.

### Frontend — home view

[`apps/desktop/src/routes/+page.svelte`](apps/desktop/src/routes/+page.svelte)
replaces the pre-Session-63 "select a plan from the list…" empty
hint with the cross-plan `RecordsDashboard` mounted on
`plans.globalRecords`. Renders whenever no plan is selected — the
new home view. Selecting a plan still drops into `PlanReview`'s
per-plan view unchanged.

### Frontend — copy-link affordance on every record card

[`apps/desktop/src/components/common/CopyButton.svelte`](apps/desktop/src/components/common/CopyButton.svelte)
is new — a small hover-reveal button that copies a string to the
clipboard via `navigator.clipboard.writeText` and flashes a
checkmark for 1.5s on success. Two-rectangle copy icon idle,
checkmark in `--signal-positive` green when copied,
aria-label / tooltip swap in lockstep with the visual state.

`MetricCard` and `KindCard` both gained a `.source-wrap` group in
their footers: the existing host text now sits next to the copy
button. CSS hover-reveal pattern — button is `opacity: 0` by default,
opacity 1 on card-hover, on the button's own `focus-visible`, and
during the `.copied` flash so the confirmation stays visible after
the operator moves the mouse off the card.

**Why this surface, not open-in-browser.** ADR 0009 explicitly
disables `core:shell` from the frontend — opening URLs from the
renderer would have needed either a Tauri shell plugin (loosens
the boundary, surface grows beyond one URL pattern over time) or a
new Rust-side `open_url_in_browser` `#[tauri::command]` (preserves
the boundary but adds an external-effect operation to the trust
surface). Copy-link is strictly narrower: clipboard-write only, no
plugin, no new capability, no new Rust command, no ADR 0009
amendment. An attacker who compromises the renderer can put a
payload URL on the clipboard, but the operator then has to *paste
and click* to actually visit it — two operator-in-the-loop actions
instead of one renderer→shell pivot. The operator also keeps full
choice of *where* the URL goes (private window, different profile,
notes, citation), which fits a research workflow better than
open-in-default-browser.

### Frontend — typed panels for all six record types

[`apps/desktop/src/components/panels/KindCard.svelte`](apps/desktop/src/components/panels/KindCard.svelte)
is new — a uniform `(kind, count, sample, when, sourceHost)` card
matching MetricCard's visual rhythm but generic across the five
non-Observation types. Sample line clamps to 3 lines with the full
string on hover.

[`apps/desktop/src/components/RecordsDashboard.svelte`](apps/desktop/src/components/RecordsDashboard.svelte)
gains five new panels: Events grouped by `content.event_type`,
Entities by top-level `kind`, Relations by `content.kind`, Documents
by `kind` (doc_kind), Assertions by `stance`. The pre-Session-63
`pendingTypes` "pill row" surface is removed entirely — every
non-Observation type with at least one record now renders a typed
panel; empty types stay represented by the dimmed tile in the
type-count strip at the top.

Grouping keys come from the closed-vocabulary fields in
`crates/core/src/schema/content.rs` — no host-strings, no
source-specific routing. Sample-line per type chooses the most
operator-readable single field: Event → headline, Entity →
canonical_name, Relation → `"{from} → {to}"`, Document → title (or
clamped body preview when title is empty), Assertion → claimant.

## Discipline preserved

- **Closed-vocabulary.** All five new grouping keys are enumerated
  schema fields (`event_type`, `entity.kind`, `relation.kind`,
  `doc_kind`, `stance`); none are host strings or source names.
- **Product framing.** The session direction came from the
  operator's screenshot of `EVENTS 1` rendering as a pill, not from
  ADR triage. The product gap (lost-records-across-plans, plus
  zero-typed-panels-beyond-Observation) was visible and immediate.
- **Schema-first.** No schema change, no DTO change. The wire shape
  `RecordsByPlanDto` is reused; the new command and new dashboard
  panels are pure read-and-render layers over the existing schema.
- **Memory persisted.** The cross-plan + typed-panels direction is
  saved to
  `spaces/c19dac53-…/memory/project_sr_session_63_global_dashboard.md`
  and indexed in `MEMORY.md` — at the operator's explicit request
  ("make it persist as some sessions losing these").

## Files changed

```
crates/storage/src/queries.rs                (+~290 lines, refactored row-decoders, +4 tests)
crates/api/src/commands_records.rs           (+~60 lines)
apps/desktop/src-tauri/src/main.rs           (+1 command registration)
apps/desktop/src/lib/api/client.ts           (+1 wrapper)
apps/desktop/src/stores/plans.svelte.ts      (+globalRecords field + helper + 2 hook calls)
apps/desktop/src/routes/+page.svelte         (right-pane empty state → global dashboard)
apps/desktop/src/components/RecordsDashboard.svelte
                                             (+5 typed panels, -pending pill row)
apps/desktop/src/components/panels/KindCard.svelte   (new)
```

## Verification gate — pending operator

The operator runs cargo on Mac and tees the logs. Verification
commands:

```
cd ~/Documents/Claude/Projects/SituationRoom && \
  (cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log && \
  (cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../tsc.log
```

Expected pass thresholds:
- `cargo test --workspace`: previous baseline 794 passed → expect
  ≥798 (the +4 `recent_records_global_*` tests added in
  `queries.rs`).
- `npm run check`: 0 errors, 0 warnings.

## Path to Accepted — Session 64

Once the verification logs land green, rsync to the live binary and
spot-check the home view:

1. App boot lands on the global dashboard (no plan selected).
2. Records across the lithium + hurricane + Fed plans all surface
   simultaneously.
3. The `EVENTS 1` from the in-progress hurricane v1.20 run renders
   as an Events typed panel with the event_type as the kind and the
   headline as the sample.
4. Running a fresh fetch updates the dashboard inside the
   synchronous handler.

If any of those four don't hold, that's the next session's
debugging surface — most likely a wire-shape mismatch in one of the
per-type grouping accessors, or a missing call site for
`refreshGlobalRecords`.

## What this defers from Session 62's planned work

Session 62 left the v1.20 hurricane re-run as the ADR 0019
acceptance gate. The screenshot the operator shared *is* a v1.20
run (1 event, mid-fetch), and the new typed panel will be how the
operator visually verifies whether ≥1 `extracted_inner` recipe was
authored. So Session 63's dashboard work is the empirical-gate
visualization — the gate decision lands once the operator can read
the panel data after a 5-trial sweep, which is Session 64+
territory.

End of handoff.
