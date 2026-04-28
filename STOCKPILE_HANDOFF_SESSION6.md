# Stockpile — Session 7 Handoff

Continuation document for the next session. Covers the state of the
codebase as of end of Session 6, what works, what's still imperfect,
and what Session 7 should pick up.

## State of the codebase

**Phase 5 — the GUI — is in place but un-tried-in-anger.** The Tauri 2
desktop binary now boots a Svelte 5 webview wired to three IPC
commands (`classify`, `list_recent_plans`, `get_plan`) that wrap the
exact same pipeline calls the situation-room CLI uses. The frontend
renders a real Plan Review screen (P1 in last session's handoff), a
recent-plans list (P2), and a topic-input bar with wire-state status
line (P3).

**An honest constraint up front:** the Session 6 sandbox had no Rust
toolchain and no network. None of the new code was compiler-verified
end-to-end. Every type signature was cross-referenced against the
crates the new code calls into (`pipeline::research_classifier`,
`pipeline::research_plans_store`, `storage::Store`, `llm::XaiProvider`,
`secure::SecureHttpClient`), so the code should build, but the first
thing Session 7 should do is `cargo build --workspace` and see what
the compiler flags. Treat the list under "Things to verify on first
build" below as the start of the punchlist.

| Phase | Status |
|---|---|
| 4a — Level-1 classification | done since Session 4, untouched |
| 4b — Level-2 recipe authoring | done since Session 3, untouched |
| 4c–e — Plan store + CLI + GeoScope | done since Session 5 |
| 5a — `crates/api` real commands | done, types-checked but not compiled |
| 5b — Tauri 2 shell wiring | done, types-checked but not compiled |
| 5c — Capabilities allow-list | done; only core plugins enumerated |
| 5d — ts-rs DTOs + From conversions | done, with tests |
| 5e — Generated `.ts` types committed | done (hand-written to match ts-rs output) |
| 5f — Svelte 5 frontend (P1/P2/P3) | done, never opened in a browser |

Workspace is unchanged at seven library crates plus two binaries
(`stockpile-desktop`, `stockpile-situation-room`).

## What works (in theory; verify on first build)

- **Three Tauri commands** in `crates/api/src/commands.rs`:
  - `classify(topic: String) -> ResearchPlanDto`
  - `list_recent_plans(limit: usize) -> Vec<PlanSummary>`
  - `get_plan(id: String) -> ResearchPlanDto`
  Each is a thin wrapper over functions that already worked from the
  CLI. The wiring (store open, `SecureHttpClient`, `XaiProvider`,
  `config/sources.toml` load) mirrors `apps/situation_room/src/main.rs`
  exactly.

- **`AppState`** is the shared container. Built once in
  `apps/desktop/src-tauri/src/main.rs` and registered with
  `tauri::Builder::manage`. Holds `Arc<Store>`, `Arc<XaiProvider>`,
  the embedded classifier prompt, and the loaded source descriptors.
  `TOPICS_INJECTION_LIMIT` is a const at 30 to match the CLI default.

- **`CommandError`** is the IPC error type. Discriminated union with
  four kinds (`invalid_input`, `classification_failed`, `storage`,
  `not_found`); `serde(tag = "kind", rename_all = "snake_case")`.
  Frontend pattern-matches on `.kind`. Tests in `commands.rs` and the
  shadow `CommandErrorDto` in `types_export.rs` guard the wire shape
  in lockstep.

- **DTOs and ts-rs.** The full plan shape is mirrored as
  `ResearchPlanDto` / `GeoScopeDto` / `RecordExpectationsDto` and
  per-bucket DTOs in `crates/api/src/types_export.rs`. Each has a
  `From<…>` impl from the corresponding `pipeline::research::*` type.
  The decision to mirror rather than `#[derive(TS)]` directly on the
  pipeline's types keeps `ts-rs` out of the pipeline crate's
  dependency tree (it would otherwise be in the situation-room
  CLI's transitive deps for nothing).

- **`PlanSummary`** is the lightweight row for the listing screen:
  id, topic, created_at, plus per-bucket counts. Computed from the
  stored JSON columns at marshalling time. Surfacing parse failures
  rather than zeroing them out is intentional — see
  `PlanSummary::from_stored`.

- **Frontend.** Svelte 5 with runes. One SPA route at `+page.svelte`
  composing three components: `TopicInput`, `RecentPlansList`,
  `PlanReview`. Common pieces (`Chip`, `Bucket`, `ExpectationRow`)
  live in `src/components/{common,panels}`. State is a single
  `$state` object in `src/stores/plans.ts` exposing `classifyTopic`,
  `selectPlan`, `refreshRecent`, `clearSelection`. All styling reads
  from the CSS variables in `src/lib/design/global.css`; no
  hardcoded hex values anywhere.

- **Capabilities (ADR 0009).** `apps/desktop/src-tauri/capabilities/default.json`
  enumerates only the core window permissions Stockpile needs. Custom
  Tauri commands (the three above) are exposed via `invoke_handler`
  on the builder, which is the standard Tauri 2 path. `core:fs`,
  `core:http`, `core:shell`, `core:process` remain disabled.

- **CONTRIBUTING.md** has been corrected to remove references to
  the long-deleted `crates/sources` and `crates/analytics`. It still
  links to `docs/sources/adding_a_source.md` which I haven't checked
  for staleness — if it's also stale, that's a small follow-up.

## Test count

I wrote new tests but did not run them. The new ones, by file:

- `crates/api/src/commands.rs`: 3 tests (CommandError serialization
  in three variants).
- `crates/api/src/types_export.rs`: 5 tests (DTO round-trips, summary
  marshalling, corrupt-JSON surface, error DTO shape).

That's +8 against the existing 97. **Verify the count on first
`cargo test --workspace`.** If anything regresses, the most likely
culprit is one of the type-conversion sites in `types_export.rs`
(see "Things to verify" below).

## Things to verify on first build

In rough order of likelihood-something-breaks:

1. **Tauri 2 default features.** `Cargo.toml` has
   `tauri = { version = "2", features = [] }` with no features. The
   default `tauri` feature set includes the `wry` webview backend,
   which on Linux pulls in `webkit2gtk`. If the build environment
   doesn't have the system libs, this is the failure that surfaces
   first. The fix for headless CI is `default-features = false`,
   plus an explicit feature list — but for actually opening the
   webview, the defaults are what we want.

2. **`tauri-build` running cleanly.** It reads `tauri.conf.json` and
   the capabilities file and writes generated code under
   `apps/desktop/src-tauri/gen/`. If the conf file is missing
   anything Tauri 2 expects, this fails at compile time with a
   reasonable error.

3. **`#[tauri::command]` async signatures.** The three commands take
   `tauri::State<'_, AppState>` as a parameter. Tauri 2 expects this
   parameter to be the *last* one. I have it last in all three —
   verify if a macro error appears.

4. **`Vec<PlanSummary>` from a `?`-chain.** In `list_recent_plans`,
   the `.map(...).collect::<Result<Vec<_>, _>>()?` pattern works,
   but `?` only converts via `From`. The error there is a
   `serde_json::Error`, not a `CommandError` — I wrap it with an
   explicit `.map_err(...)` rather than `?`. Confirm that pattern
   compiles; if not, a single `?` after a `From<serde_json::Error>`
   impl on `CommandError` cleans it up.

5. **`tracing` macro `%` formatter on `path.display()`.** Both the
   CLI and the new desktop binary use `path = %db_path.display()`
   inside `info!`. The CLI compiles; the desktop binary should too.

6. **The hand-written `.ts` files match what ts-rs would emit.** I
   wrote `apps/desktop/src/lib/api/types/*.ts` to match ts-rs's
   default formatter (one line per type, semicolons after each field,
   `Array<T>` rather than `T[]`). On first run of
   `cargo test --package stockpile-api`, ts-rs will overwrite these
   files. If the files diverge, treat ts-rs's output as canonical
   and update the frontend's imports if any field name changes
   slipped through (none should — I cross-referenced).

## Known imperfections

### 1. `XAI_API_KEY` is a hard requirement to even open the GUI

The desktop binary aborts at boot if the key isn't loaded — same
posture as the CLI. This means a user with old persisted plans can't
just browse them; they have to set up a key first. The handoff for
Session 6 didn't ask for graceful degradation here, and the cleaner
fix (a "browse-only" mode) is its own design pass: do the LLM-call
sites need to surface a typed `NoProvider` error, or do we conditionally
build a `MaybeProvider` enum? Worth doing, but properly, not
half-done.

### 2. Accept / Reject / Re-classify buttons are absent from the review screen

The Session-6-incoming handoff lists them under P1, but the storage
layer has no soft-delete or supersede operation (handoff §5: "no way
to delete or amend a plan"). Adding non-functional buttons would be
UI theater. They land naturally when storage grows the underlying
operation, which is itself a small add — see Session 7 priorities.

### 3. Topic-tag chips don't show usage counts

The handoff suggests "hover showing usage count" on topic-tag chips.
Doing it requires either a fourth Tauri command (`topics_in_use`) or
embedding the counts in `ResearchPlanDto`, neither of which the
classifier flow currently produces. Cheap to add later — the
storage-layer query is already there (`Store::topics_in_use`).

### 4. No bookmarkable plan URLs

Single SPA route. Opening a plan mutates the page state but not the
URL. SvelteKit can give us `/plan/:id` cheaply; the handoff was
explicit that Session 6 doesn't introduce routing, and I respected
that. Add when the use case appears.

### 5. The Plan Review screen scrolls the whole page

Vertical-scroll discipline: the bucket grid is the natural focus area
but it's wrapped by a single scroll container. On long plans (lots of
event types or document sources), the user scrolls past the
interpretation and topic strips. A two-zone layout (sticky header +
buckets-only-scroll) is a one-CSS-pass improvement.

### 6. `tauri.conf.json` references `pnpm dev` / `pnpm build`

The frontend has no lockfile checked in, so the package manager isn't
forced. If the user prefers `npm` or `bun`, they edit
`beforeDevCommand` and `beforeBuildCommand`. Worth normalizing one
way; not worth a session.

### 7. Empty placeholder directories cleaned up, but not all

I removed `apps/desktop/src/components/charts/` and
`apps/desktop/src/routes/commodity/` (both Phase-1 `.gitkeep`-only
dirs). I did not touch `docs/sources/` or `docs/architecture/` even
though the latter is referenced by the now-corrected
`CONTRIBUTING.md`; check whether those docs still reflect reality.

### 8. Carried forward from Session 5

- Anthropic provider and others are stubs.
- Apply-runtime strict deserialization is permissive.
- PdfTable extractor is unimplemented.
- Authoring latency is 30–60s (xAI gateway, not us).
- `SecureHttpClient` doesn't surface response headers.
- Crate-level `#![allow(...)]` lint suppressions still need a sweep
  across crates that aren't `api` (I removed it from `api` since
  every item there is now real).

## Patches shipped in Session 6

For history.

1. **Workspace deps: Tauri 2.** Added `tauri = "2"` and
   `tauri-build = "2"` to `Cargo.toml`'s `[workspace.dependencies]`.
   The desktop binary picks both up; the build script
   (`apps/desktop/src-tauri/build.rs`) runs `tauri_build::build()`
   so the capabilities file gets compiled in.

2. **`crates/api` rewritten.** Phase-1 stub modules (`queries`,
   `subscriptions`) were deleted — they were placeholders for work
   downstream of recipe-authoring-in-the-UI, which the handoff
   defers. `commands` and `types_export` are now real:
   - `commands::AppState` — the shared container.
   - `commands::{classify, list_recent_plans, get_plan}` — the three
     `#[tauri::command]` handlers.
   - `commands::CommandError` — the wire-error union.
   - `types_export::*Dto` — wire DTOs with `#[derive(TS)]` and
     `From<pipeline::…>` impls.
   - `types_export::PlanSummary` — listing-row marshaller.

3. **Desktop binary rewritten.** `apps/desktop/src-tauri/src/main.rs`
   went from a Phase-1 print-banner stub to the real composition
   root: open the store, build the LLM provider via
   `SecureHttpClient`, load `config/sources.toml`, register the
   three commands, run Tauri. `XAI_API_KEY` is required (matches
   CLI). New `build.rs` invokes `tauri_build::build()`.

4. **Capabilities file made strict.** `core:default` plus only the
   window-management permissions. No `core:fs`, `core:http`,
   `core:shell`, `core:process`. ADR 0009 satisfied.

5. **Frontend.** Real components in `apps/desktop/src/components/`:
   `TopicInput.svelte`, `RecentPlansList.svelte`, `PlanReview.svelte`,
   plus `common/Chip.svelte`, `panels/Bucket.svelte`,
   `panels/ExpectationRow.svelte`. State store at
   `src/stores/plans.ts` (Svelte 5 runes). Typed API client at
   `src/lib/api/client.ts`. Generated TS types at
   `src/lib/api/types/`. Page composition at `src/routes/+page.svelte`.
   All styling from CSS variables in `src/lib/design/global.css` —
   no hardcoded hex anywhere in components.

6. **CONTRIBUTING.md corrections.** Dropped references to deleted
   `crates/sources` and `crates/analytics`; the "How to add a data
   source" section now describes the `config/sources.toml` model.

## Suggested Session 7 priorities

Recipe authoring in the UI is the natural next big piece, but it
needs background-job machinery first (handoff §"What Session 6
should NOT do"). Unless you've decided that's the next session, the
shorter-leverage moves are:

### Priority 1 — Run the build and fix what the compiler flags

Per the constraint at the top of this doc, every Session 6 file is
type-checked-by-eyeball but not compiler-verified. Start with
`cargo build --workspace`, then `cargo test --workspace`, then
`cargo clippy --workspace --all-targets -- -D warnings`. If the
front-end is going to be exercised, `cd apps/desktop && pnpm install
&& pnpm dev` (or your package manager of choice). Resolve in that
order; don't skip the clippy pass.

### Priority 2 — Plan delete / supersede

The smallest meaningful storage extension: a soft-delete column
(`deleted_at NULLABLE TIMESTAMP`) on `research_plans`, plus a
`Store::soft_delete_research_plan` method, plus a
`Store::recent_research_plans_excluding_deleted` (or a
`include_deleted: bool` flag). Then add a `delete_plan(id)` Tauri
command, and wire a button on the review screen. This unlocks the
Accept / Reject UX the handoff originally wanted.

### Priority 3 — Topic-tag usage hovers

Add `topics_in_use` as a fourth Tauri command (it already exists in
storage) and have `RecentPlansList` or `PlanReview` render counts.
Small, but it makes the existing-topics-injection mechanic visible
to users, which is one of the project's distinctive properties.

### Priority 4 — Basic packaging story

Right now the dev loop is `cargo run -p stockpile-desktop` with the
frontend served by Vite. A first `tauri build` pass to produce a
real `.app` / `.dmg` / `.AppImage` is a half-day's work and surfaces
icon assets, signing posture, and the macOS hardened-runtime
entitlements. Worth doing once before the cosmetic polish session.

## What Session 7 should NOT do

- **Build recipe authoring into the UI without a job table.** Same
  rule as Session 6. Authoring is 30–60s per source per plan; a UI
  that surfaces it as a synchronous action will feel broken.
- **Re-architect the frontend.** The component split (TopicInput /
  RecentPlansList / PlanReview, with `Chip` / `Bucket` /
  `ExpectationRow` shared) lands well. Add components when a screen
  needs them.
- **Replace ts-rs.** The DTO mirror pattern is deliberate; it's not
  a wart. If a future session wants to *generate* the DTOs from the
  pipeline types automatically, that's a pipeline-side change to
  derive `TS` (and accept ts-rs in pipeline's deps). Don't introduce
  a third type system.
- **Localize UI strings.** The point of `GeoScope::display` is that
  the LLM picks the register per-session. The UI is in English by
  design; `Magyarország` and `Ungarn` come from the LLM, not from
  i18n.
- **Touch the classifier prompt.** v1.2 is what the GUI session
  uses; bumps come from observed classifications, not from
  speculation.

## Files to read first when starting Session 7

In order of importance:

1. This file.
2. `STOCKPILE_HANDOFF_SESSION5.md` — still the architectural-context
   primer for the GUI work.
3. `crates/api/src/commands.rs` — the Tauri command surface.
4. `crates/api/src/types_export.rs` — the wire schema and From
   conversions.
5. `apps/desktop/src-tauri/src/main.rs` — the composition root.
6. `apps/desktop/src/routes/+page.svelte` — the page composition.
7. `apps/desktop/src/components/PlanReview.svelte` — the heaviest
   component, where most rendering decisions live.
8. `apps/desktop/src/stores/plans.ts` — frontend state model.
9. `docs/adr/0002-tauri-vs-leptos.md` — for the framework rationale,
   and `docs/adr/0006-design-language.md` for the visual contract.

## Rules of the road (carried forward, with a Session-6 addition)

- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Classification produces RecordExpectations, not new schemas.
- Closed enum of 5 extraction modes. Adding a sixth needs an ADR.
- UUIDv7 + dedup_key for identity.
- Security primitives in stockpile_secure. No
  `reqwest::Client::new()` anywhere.
- Structure follows code, not anticipates it. No empty folders.
- Code validates format, prompt teaches content. The LLM is trusted
  for what to put in the plan; the code is responsible for what
  shape it must take.
- When the user pushes back on a category of work that "keeps
  clinging back", purge it.
- **New for Session 6**: the wire schema between Rust and the
  frontend lives in `crates/api`. Pipeline types are internal.
  `From` conversions sit at the boundary so internal type changes
  produce a single, obvious place to update the wire shape — and the
  frontend's TypeScript build flags any mismatch.
