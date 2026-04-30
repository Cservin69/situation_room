# situation_room — Session 7 handoff

You are starting Session 7. Session 6 shipped the Tauri desktop GUI:
the classify pipeline is wired end-to-end, the user can type a topic
and see a fully-rendered ResearchPlan on the screen with topic chips,
geographic scope, six bucket panels, and source nominations. Persisted
plans show up in the recent-plans listing on the left.

Read this whole document before writing any code. The ADRs in
docs/adr/ are still authoritative — this handoff is the layer above
them, not a replacement.


## What works today

- Three Tauri commands in crates/api/src/commands.rs: classify,
  list_recent_plans, get_plan. Each is thin and wraps existing
  pipeline functions.
- AppState in crates/api with shared Store, XaiProvider,
  classifier prompt, and source descriptors. Constructed once in
  apps/desktop/src-tauri/src/main.rs.
- ts-rs DTOs in crates/api/src/types_export.rs mirror the
  pipeline's ResearchPlan shape. Derive #[ts(export, export_to =
  "../../../apps/desktop/src/lib/api/types/")]. Fixed in Session 6
  patch 8 — older path was missing one ../ and wrote into
  crates/apps/ by mistake.
- Svelte 5 runes-based store at apps/desktop/src/stores/
  plans.svelte.ts (the .svelte.ts extension matters — runes don't
  compile in plain .ts).
- Three components: TopicInput, RecentPlansList, PlanReview, plus
  Chip and Bucket primitives. All styling from CSS vars in
  apps/desktop/src/lib/design/global.css.
- scripts/run_desktop.sh runs everything in one process group,
  traps INT/TERM/EXIT, frees :5173 on shutdown. Use it instead of
  `cargo tauri dev` directly.
- .env loader in main.rs walks upward from CWD to find the
  workspace root. Anchors situation_room.duckdb and config/sources.toml
  to that root regardless of where the binary was launched from.


## Session 7 priorities (in order)

### P1 — Soft-delete via plan status

Add a status column to research_plans. Three values:

- pending (default for newly classified plans)
- accepted (user has reviewed and approved; the future fetch
  executor uses these)
- rejected (user has discarded; the row stays for audit but is
  hidden from default views)

This is the gate that makes the future Phase 6 fetch executor
sensible — without it, every classified plan would automatically
become input to deterministic fetching, including the bad ones.

#### Storage changes (crates/storage)

- New migration v5: ALTER TABLE research_plans ADD COLUMN status
  TEXT NOT NULL DEFAULT 'pending'. Add an index on (status,
  created_at DESC) for the listing's filtered query.
- New methods on Store:
  - set_plan_status(id: Uuid, status: PlanStatus) -> Result<()>
  - recent_research_plans_by_status(status: Option<PlanStatus>,
    limit: usize) -> Result<Vec<StoredResearchPlan>>
  - The existing recent_research_plans stays for backward compat
    (returns all statuses).
- StoredResearchPlan gains a status field. Add a PlanStatus enum
  in crates/storage/src/research_plans.rs with serde and FromStr
  for the column round-trip.

#### API changes (crates/api)

- Two new commands: accept_plan(id) and reject_plan(id). Both
  return the updated ResearchPlanDto so the frontend can do
  optimistic UI cleanly.
- Update list_recent_plans to take an optional status filter.
  Defaults to None (returns all) so existing callers still work.
- ResearchPlanDto and PlanSummary gain a status: PlanStatusDto
  field. Add the enum to types_export.rs with #[derive(TS)].
- Both new commands wired into the invoke_handler! in main.rs
  using the full path form (situation_room_api::commands::accept_plan
  etc.) — see Session 6 patch 3 for why bare names break.

#### Frontend changes

- PlanReview header gets two buttons: Accept (warm-amber, primary)
  and Reject (subtle, secondary). Hide both when plan.status is
  not 'pending' — show a small badge instead saying the current
  status with the timestamp it was set.
- Listing rows show a status pill (small, color-coded: amber for
  accepted, dim for rejected, neutral for pending).
- Filter chip strip above the listing: All / Pending / Accepted /
  Rejected. Default to Pending so the user lands on what needs
  review. Persist the choice in the runes store, not in URL state.
- plans.svelte.ts gains acceptPlan(id) and rejectPlan(id) helpers.
  Optimistic — update plans.selected immediately, refresh the
  listing, roll back on error.


### P2 — Polish from Session 6 screenshot review

These are visible rough edges in the Session 6 working app. Take
them in the same session as P1; they're each tiny.

- The + buttons on bucket rows look clickable but do nothing.
  Remove them. They were design scaffolding for "expand this
  expectation to see matching records" which is a Phase 6 thing,
  not a Session 7 thing. A non-functional button is a lie.
- The duplicate-topic problem in the listing (the two "Hungarian
  sovereign debt" rows in the Session 6 screenshot) is now solved
  structurally by accept/reject — reject the duplicate.
- Empty-state copy on the right pane when no plan is selected.
  Today it says nothing. Should say something like "Select a plan
  on the left, or classify a new topic."


## What Session 7 is explicitly NOT

- No hard delete. Soft-delete via status is enough. ADR territory
  if reclaiming disk ever becomes a real need.
- No edit-the-plan flow. If a plan is wrong, reject and re-classify
  with a refined topic. Editing introduces "is this still the
  LLM's plan or yours" semantics we don't want yet.
- No bulk operations. One plan at a time. If the listing gets long
  enough to need bulk select, that's a signal to revisit.
- No Phase 6 fetch executor work. Accept/reject is the gate; the
  thing the gate guards comes later.
- No authentication / multi-user. Single-user desktop app. Status
  is "did the (sole) user accept this plan." No actor field.


## Hard rules (carry over from Session 5/6)

- ADR 0009 §"The rule": no fresh reqwest::Client::new(). All HTTP
  goes through SecureHttpClient.
- Bounds checking on every IPC string input. See the existing
  classify command for the pattern.
- Tauri commands return CommandError, not internal error types.
  Add a new variant if accept/reject failures need a distinct kind
  from existing ones — likely they don't, Storage covers it.
- Generated TS files in apps/desktop/src/lib/api/types/ are
  written by ts-rs via cargo test. Never hand-edit. The
  export_to paths are now correct (../../../).
- ts-rs DTOs and the typed pipeline structs are intentionally
  separate. Mirror, don't share. Pipeline doesn't take ts-rs as a
  dep.
- Components only use CSS vars from global.css. No hardcoded hex.
- Runes-using files end in .svelte.ts, not .ts.


## First thing to do in Session 7

Read this file. Read docs/adr/0007-research-function.md. Then look
at the existing classify command end-to-end as the template for the
two new ones. Don't start writing migrations until you've read the
existing migrations to see the style — the file naming, the ADR
references in the migration text, the test pattern.

Build incrementally:
1. Migration v5 + Store methods + tests. cargo test --workspace
   passes before moving on.
2. PlanStatus enum + DTOs + types_export round-trip test passes.
3. Two new commands + their unit tests. cargo check --workspace
   passes.
4. Frontend: store helpers, then PlanReview buttons, then listing
   pills, then the filter strip.
5. Run the app. Classify two topics. Accept one, reject the other.
   Verify the filter strip. Verify the badges.

That order is so that every step has a green build behind it. Do
not write the entire session and then run cargo check at the end.


## Continuity note

If you are reading this as Claude with no memory of Session 6: the
human you are working with is rigorous about security ("paranoid
about security" in their words — earned, not affected), prefers
honesty about uncertainty over false confidence, and reacts well to
direct disagreement when warranted. Stick to the plan. If you need
to deviate, say so and explain why. They've explicitly asked for
"do not deviate" discipline; honor it.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.
