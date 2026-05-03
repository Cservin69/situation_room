# Session 22 — Records-on-the-workstation rendering

Ships the records-rendering surface end-to-end: storage join, API
DTOs, Tauri command, frontend store wiring, six bucket panels with
record cards. The operator can now select an Accepted plan, run a
fetch, and see the actual records the recipes produced — bucketed by
record type, with provenance chips that link each record back to the
recipe that authored it.

## What's changed

### Storage (`crates/storage/`)

- **New** `Store::records_for_plan(plan_id) -> RecordsByPlan` in
  `src/queries.rs`. Joins records to a plan via the recipe-stamped
  `envelope.provenance.source_id` substring (`#recipe:<uuid>@v…`).
  Six per-table SELECTs reuse the existing `reconstruct_envelope`
  helper.
- **New** `RecordsByPlan` struct (six per-type Vecs, `Default`-able).
- **New** `pub use queries::{RecordsByPlan, TopicUsage}` in `src/lib.rs`.
- **Tests:** +6 in `queries.rs` (empty plan, observation join, plan
  isolation, multi-recipe aggregation, legacy provenance skipping,
  provenance-string round-trip).

The plan→record join is *recipe-routed*, not topic-routed. A record
that was produced by a recipe attached to plan A, but happens to share
a topic with plan B, will only show up under A. This is by design —
plan B never asked that recipe for anything, so the records aren't
"plan B's records" in any operational sense.

### API (`crates/api/`)

- **New** `src/records_dto.rs` — `EnvelopeDto`, `ProvenanceDto`,
  `SubjectsDto`, six per-record-type DTOs (`ObservationDto`,
  `EventDto`, `EntityDto`, `RelationDto`, `DocumentDto`,
  `AssertionDto`), and `RecordsByPlanDto`. Envelope is strongly typed;
  content is opaque (`unknown` in TS) — same trade-off as
  `RecipeDto.extraction`. The provenance string is exposed in two
  forms: the raw `source_id` and the parsed `recipe_id` (empty string
  when the format doesn't match — wire convention for "absent").
- **New** `src/commands_records.rs` — `records_for_plan(id)` Tauri
  command. Status-gates: `pending` → `InvalidInput` (no fetch has
  happened yet, so listing is meaningless); `accepted` and `rejected`
  both allowed (rejected plans may carry records from a prior accept
  cycle).
- **Updated** `src/lib.rs` — adds `pub mod records_dto;` and
  `pub mod commands_records;`.
- **Tests:** +6 in `records_dto.rs` (parse_recipe_id valid+invalid,
  ObservationDto round-trip, RecordsByPlanDto bucket shape, wire
  per-type Vec emission, legacy empty-recipe-id, EntityDto vocab lift).

### Desktop binary (`apps/desktop/src-tauri/`)

- **Updated** `src/main.rs` — registers
  `situation_room_api::commands_records::records_for_plan` in
  `invoke_handler!`. No other change.

### Frontend (`apps/desktop/src/`)

- **New** TypeScript types under `lib/api/types/`:
  `EnvelopeDto.ts`, `ProvenanceDto.ts`, `SubjectsDto.ts`,
  `ObservationDto.ts`, `EventDto.ts`, `EntityDto.ts`,
  `RelationDto.ts`, `DocumentDto.ts`, `AssertionDto.ts`,
  `RecordsByPlanDto.ts`. Hand-mirrored in ts-rs's exact format;
  running `cargo test -p situation_room-api` regenerates them and
  any drift will surface as a diff.
- **New** `lib/api/recordSummary.ts` — six per-type summarizers
  (`summarizeObservation`, etc.) that produce a one-line summary
  for each record type by reading `unknown` content via best-effort
  property access.
- **Updated** `lib/api/client.ts` — adds `recordsForPlan(id)` and
  the `RecordsByPlanDto` import.
- **Updated** `stores/plans.svelte.ts` — adds
  `records: RecordsByPlanDto | null` to state, plus a
  `refreshRecords(planId)` helper. Called from `selectPlan` (when
  the plan is past `pending`), from `runFetch` (after the fetch
  report lands), and from `transitionSelected` (after a fresh
  accept) so the bucket panels stay in lockstep with the data.
  Cleared on `clearSelection` and at the start of `selectPlan`.
- **New** `components/panels/RecordCard.svelte` — a record line under
  a bucket. Discriminated-union props (six `kind` variants), per-type
  summary via the recordSummary helpers, a "recipe" chip linking back
  to the source recipe (id truncated to 8 chars; full id on hover),
  and an expand caret that reveals the full record as pretty-printed
  JSON.
- **Updated** `components/panels/Bucket.svelte` — adds optional
  `recordsCount` prop. The "(no expectations for this type — by
  design)" empty state now only appears when *both* expectations and
  records are empty. (Records existing without expectations is a
  legitimate state — a recipe produced records the plan didn't
  anticipate — and the "by design" copy would be misleading there.)
- **Updated** `components/PlanReview.svelte` — threads records into
  each of the six `<Bucket>` invocations. Renders, per bucket: the
  expectations rows (existing) + a records section (new). The records
  section appears only when `plans.records !== null` (i.e., we've
  asked the backend at least once). When records are loaded but
  empty for a bucket that has expectations, the section shows a "0
  records yet — run a fetch to populate" hint inline.

## Tests delta

- Storage: **+6** new tests in `queries.rs` (existing 4 carried
  forward verbatim).
- API: **+6** new tests in `records_dto.rs`.

Workspace test count expected to land near **~392 green** (Session 21
baseline of ~380 + 12 new).

## How to apply

From the repo root:

```sh
tar -xzf ~/Downloads/session22-records-rendering.tar.gz --strip-components=1 -C .
```

The patch is additive (no file deletions). It overwrites these
existing files:

- `crates/storage/src/queries.rs`
- `crates/storage/src/lib.rs`
- `crates/api/src/lib.rs`
- `apps/desktop/src-tauri/src/main.rs`
- `apps/desktop/src/lib/api/client.ts`
- `apps/desktop/src/stores/plans.svelte.ts`
- `apps/desktop/src/components/panels/Bucket.svelte`
- `apps/desktop/src/components/PlanReview.svelte`

And adds these new files:

- `crates/api/src/records_dto.rs`
- `crates/api/src/commands_records.rs`
- `apps/desktop/src/lib/api/recordSummary.ts`
- `apps/desktop/src/lib/api/types/{Envelope,Provenance,Subjects,Observation,Event,Entity,Relation,Document,Assertion,RecordsByPlan}Dto.ts` (10 files)
- `apps/desktop/src/components/panels/RecordCard.svelte`

## Verification

```sh
# Compile.
cargo check --workspace

# Storage tests (new ones).
cargo test -p situation_room-storage records_for_plan

# Full storage tests (incl. carry-forward topics_in_use).
cargo test -p situation_room-storage queries

# API tests (regenerates ts-rs output as a side effect).
cargo test -p situation_room-api

# Full workspace.
cargo test --workspace

# Frontend type-check.
cd apps/desktop && npm run check
```

Then run the desktop app:

```sh
cd apps/desktop && npm run dev
```

Pick the south-korean-elections plan from Session 21 (id
`019de886-ba15-…` should still be in the local DB). Its status is
Accepted, and the Session 21 fetch run produced one record from
`rss_feeds`. The bucket panels should now render that record under
either Documents or Observations (depending on which type the
`rss_feeds` recipe produces — the operator should verify this is
sensible). The records section under buckets that *do* have
expectations but no records should show "0 records yet — run a fetch
to populate."

For a fresh end-to-end smoke: classify a new topic, accept it, run a
fetch, and verify the bucket panels populate alongside the fetch
report. The records appear in the bucket grid above the fetch report
section.

## Known caveats

- **Per-expectation slotting is not implemented.** Records render
  per bucket (record-type), not per expectation. Drilling down to
  "show me the records that satisfied the `production` metric
  expectation" requires changes to `recipe_apply::build_record` that
  are out of Session 22's scope — see Session 23 handoff for the
  ADR-shaped follow-up.

- **Legacy records are filtered out.** Any record whose
  `envelope.provenance.source_id` doesn't contain the recipe-stamped
  substring (`#recipe:<uuid>@v…`) won't appear in the bucket — the
  query is recipe-routed. This is correct: such records were
  produced by code paths that pre-date `recipe_apply` and aren't
  attributable to any specific plan.

- **Content rendering is JSON.** The expanded view of each record
  card is pretty-printed JSON, not a per-type rich renderer. A
  follow-up session can replace the JSON with per-type renderers
  once we know what the operator actually wants to see at depth.

- **Records section appears on rejected plans.** A plan rejected
  after a successful fetch will still show its records under the
  bucket panels — a deliberate audit affordance. If this surfaces
  as confusing in practice, the gate is one boolean in
  `PlanReview.svelte`.

## Architectural surface introduced

The decision to make records-rendering recipe-routed (rather than
plan-routed via a junction table or a `record_plan_id` column) keeps
the storage schema unchanged. The substring LIKE join is honest about
the actual relationship — records belong to the recipe that produced
them; plans only own records via the recipes attached to them. Adding
a denormalized `record_plan_id` column would have meant another
migration plus per-insert maintenance for a query that runs once per
plan-selection. If volumes ever grow into the hundreds of thousands
per type, that migration becomes interesting; today it's premature.

The `recipe_id` field on `ProvenanceDto` is parsed from the source_id
string in the DTO conversion, not stored separately. This means the
parsing logic lives next to the DTO that exposes it, and a future
change to the provenance format only touches one parse function.
