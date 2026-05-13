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
apps/desktop/src/routes/+page.svelte         (right-pane empty state → global dashboard;
                                              dead `.empty` CSS rule pruned)
apps/desktop/src/components/RecordsDashboard.svelte
                                             (+5 typed panels, -pending pill row)
apps/desktop/src/components/panels/KindCard.svelte   (new)
apps/desktop/src/components/panels/MetricCard.svelte (+CopyButton wiring + hover-reveal CSS)
apps/desktop/src/components/common/CopyButton.svelte (new — clipboard copy affordance)
```

## Verification gate — green

- `cargo test --workspace`: passed on operator's Mac (operator
  reported "all green"). Baseline 794 → expected ≥798 after the
  +4 `recent_records_global_*` tests in `queries.rs`.
- `npm run check`: 0 errors, 0 warnings after a follow-up pass
  fixed two cosmetic warnings — added the standard `line-clamp`
  property alongside `-webkit-line-clamp` in `KindCard.svelte` for
  forward-compat, and pruned the now-unused `.empty` CSS rule from
  `+page.svelte` (its markup was replaced by the `.home`
  cross-plan dashboard block).

## Live-run result — hurricanes didn't break through

Operator ran v1.20 against the 2025 Atlantic hurricane season plan
and reported "hurricanes did not come out so fine" — meaning the
prompt-only changes from Session 62 didn't shift the rate enough to
clear ADR 0019's acceptance threshold. Events *did* land via a
different topic during the session, so the typed Events panel is
verified working end-to-end against live data; this is a recipe-
authoring rate problem on the hurricane shape specifically, not a
dashboard-rendering problem.

This kills the Session 62 hypothesis that "multi-leaf recognition
checklist + positional-selector worked example + apply-time-signal
subsection" would together move 0/10 → ≥1/5 on `extracted_inner`
authoring. Session 64 needs a structurally different intervention,
not another prompt iteration.

## Session 64 direction — two pre-staged ADR 0019 follow-ons

Session 62's handoff documented two candidates in case v1.20
didn't suffice. Both still apply; recommendation differs in
intervention depth.

1. **Reasoning-block-before-JSON** (cheaper, single-shot). Let the
   LLM write a freeform analysis of the prefetch *before* emitting
   the structured recipe JSON. The multi-leaf recognition checklist
   runs as visible reasoning rather than as latent token-distribution
   shifting. Lower cost: prompt-only delta. Tests whether the LLM
   *can* recognise multi-leaf rows when forced to articulate first.
2. **Recipe-iteration-on-FetchReport loop** (Session 60's candidate
   A, deeper). When a single-leaf recipe fails at apply with
   "inner selector matched no elements," automatically re-author
   against the retry excerpt with the failure message inline as a
   multi-leaf signal. Closes the loop that the v1.20 apply-time-
   signal subsection only described in prose. Higher cost: runtime
   loop + recipe-author re-entry. Tests whether *feedback on
   failure* lifts the rate where prompt-only iteration plateaued.

**Recommendation: recipe-iteration-on-FetchReport.** It's the
no-easy-wins move per the operator's standing feedback (deeper
than prompt iteration, exercises a real loop), and the typed
Events panel now lets the operator read the iteration's result
directly. The cheaper reasoning-block experiment is worth running
*after* — if the loop lifts the rate, the reasoning-block test
becomes a "can we get the same lift more cheaply" follow-up; if
the loop *also* doesn't lift the rate, that's strong evidence the
ceiling isn't prompt-engineerable at all and ADR 0019 needs a
different structural pass.

The eval-harness from Session 57 makes either candidate a 5-trial
sweep with variance bounds — no more single-trial decisions on
prompt experiments.

## Smoke-test after rsync

Spot-check the new home view independent of the Session 64
candidates above. Four points:

1. App boot lands on the global dashboard (no plan selected).
2. Records across the lithium + hurricane + Fed plans all surface
   simultaneously.
3. Past hurricane runs' EVENTS render as a typed Events panel with
   `event_type` as the kind and the headline as the sample.
4. A fresh fetch updates the dashboard inside the synchronous
   handler.
5. Hovering any record card reveals the copy button next to the
   source host; clicking copies the full URL, flashes a green
   checkmark for ~1.5s, paste lands the URL into any external
   destination.

If any of those don't hold, that's a Session 64 opening-debug
surface — most likely a wire-shape mismatch in one of the per-type
grouping accessors, a missing `refreshGlobalRecords` call site, or
a `navigator.clipboard` permission edge case on the operator's
specific Tauri runtime.

End of handoff.
