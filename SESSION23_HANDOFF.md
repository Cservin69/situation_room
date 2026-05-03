# Session 23 — handoff

## State at end of Session 22

Records-on-the-workstation is shipping. The full read path —
storage join → API DTO → Tauri command → frontend store → bucket
panels with record cards — is in place and compiling. Operator can
now select an Accepted (or Rejected) plan and see its records bucketed
by type, with provenance chips that cross-reference the authoring
recipe.

Test count delta: +12 (storage +6, api +6) → expected workspace total
around **~392 green**.

The patch was applied as `session22-records-rendering.tar.gz`. Verify
by running `cargo test --workspace` and `npm run check` from
`apps/desktop`. Live xAI verification: select the south-korean-
elections plan from the Session 21 fetch run; the rss_feeds record
should appear in whichever bucket the recipe produces (Document or
Observation, depending on bindings).

## Session 23 priority — pick one

Two threads compete for next-session attention. Both are unblocked.

### Option A — Per-expectation slotting (the architectural follow-up)

**The ask.** Drill records down from per-bucket to per-expectation.
"This Observation satisfies the `production` metric expectation; that
one satisfies `voter_turnout`." Today every observation lands in the
observation bucket regardless of which metric it instantiates.

**Why it's not in Session 22.** The provenance string carries
`{src}#recipe:{recipe_uuid}@v{version}` — recipe id, but not binding
tag or expectation index. A recipe's `produces_json` knows which
`ProductionBinding` produced a record (because the recipe-apply
runtime walks the bindings), but `recipe_apply::build_record` doesn't
stamp the binding identity back onto the record. So at query time
there's no way to tell which expectation a given record satisfies.

**The fix is ADR-shaped.** Two reasonable paths:

1. **Stamp binding tag in provenance.** Extend the format to
   `{src}#recipe:{uuid}@v{ver}#binding:{tag}`. Cheap to read at
   query time (substring match again). Requires an ADR amendment to
   ADR 0007 (the provenance format is documented there) and a
   migration story for existing records (substring stays valid; new
   records add the binding suffix; old records render in the bucket
   without per-expectation slotting until they're re-fetched).

2. **Add a `record_binding` column.** New nullable column on each
   record table populated at insert time by `record_dispatch`. Three
   migration files (one per per-record-table that currently lacks
   it), one new column on six tables. The query becomes a normal
   joined SELECT instead of a substring LIKE. Faster long-term but
   more upfront work.

**Recommendation.** Path 1 keeps the schema unchanged and ships in
one session. Path 2 is the right answer if record volumes grow into
the hundreds of thousands and the substring scan becomes a problem.
Today, Path 1 is the right scope.

The operator should weigh: is per-expectation slotting actually the
next thing they want to see in the UI, or is it more important to,
say, ground-truth the LLM-produced records against real data first?
The recipes panel already exists for that flow. Per-expectation
slotting is a refinement of how records render, not a capability
that's missing.

### Option B — Rich per-type record renderers

**The ask.** Replace the JSON pretty-print in `RecordCard`'s
expanded view with per-type rich renderers. Observations get
metric/value/unit + a tiny sparkline if `period === "annual"`.
Documents get a title link + the body's first paragraph. Events get
event_type, headline, the entities involved, the place + time it
occurred. Entities get the canonical_name, kind, alternate ids,
geometry shown on a tiny map (or just lat/long). Relations get
from→to with both endpoints clickable. Assertions get the claimant,
stance, and the `kind`-dispatched content.

**Why it's not in Session 22.** Session 22 was about getting
records visible at all. Per-type rich renderers are work that's
sized in days, not hours: there are six types, each with its own
shape, each with its own decisions about what's worth surfacing. The
JSON view ships the audit affordance immediately and lets the
operator see what's in the database; the rich renderers are a
quality-of-life upgrade.

**Sub-decisions** (each non-trivial):
- Should `EntityDto.geometry` get a typed mirror? Currently
  `unknown`. To render even a simple "37.5°N 127.0°E" line, the
  rendering code has to know which `Geometry` variant the JSON
  represents. Either type it, or write a defensive read that handles
  all variants by best-effort property access (the `recordSummary.ts`
  pattern, scaled up).
- Same question for `SubjectsDto.places` and `SubjectsDto.time`.
- Same question for `AssertedContent` (six variants).

**Recommendation.** This is a 2-3 session arc. Pick one record type
per session, ship its renderer, and let the others stay JSON until
they're touched. Observation is the most operator-value-rich (the
data they actually came for); Document is the most distinctive
(title + body). Either is a reasonable starting point.

## Smaller follow-ups

These are all sub-session-sized; they can be folded into A or B
above, or done separately as housekeeping.

- **Session 22 caveat: rejected plans show records.** A plan rejected
  after a successful fetch keeps its records visible under the
  bucket panels. This is intentional (audit affordance), but if the
  operator finds it confusing in practice the gate is one boolean
  in `PlanReview.svelte`'s recordsLoaded computation.

- **Session 22 caveat: legacy records filtered.** Records whose
  provenance lacks `#recipe:` are excluded from `records_for_plan`.
  These are pre-recipe_apply records (from before Session 8). If
  any are discovered in production data, the cleanest fix is a
  one-time migration that backfills the provenance string for
  pre-recipe_apply records. Punt unless evidence shows up.

- **Empty `recipe_id` chip rendering.** `RecordCard` hides the
  recipe chip when `recipe_id === ""`. For records with the chip
  hidden, the operator has no visual cue that a recipe id couldn't
  be parsed. Consider an alternate "legacy" chip that explicitly
  marks the record as pre-recipe_apply, so the absence isn't
  ambiguous.

- **Session 21 architectural concern.** The `cobalt_supply_chain`
  classification produced empty buckets in earlier testing — root
  cause was identified and a prompt fix proposed. Verify it landed;
  if not, fold it in.

- **`sanitize_for_fence` duplicated.** The helper is duplicated
  between `research_classifier.rs` and `recipe_author.rs` (Session
  20 carry-forward). Promote to a shared utility module under
  `crates/pipeline/src/util/` or `crates/secure/src/`.

- **Anthropic provider stub.** Still stubbed; ADR 0010 has the plan.
  Not blocking go-live but worth picking up if xAI reliability
  becomes a sustained issue.

- **Apply-runtime permissive deserialization.** Outstanding from
  Session 11 — the recipe-apply path tolerates malformed JSON in
  some edge cases that a stricter deserializer would reject. Worth
  auditing once.

- **PdfTable extractor unimplemented.** Session 18 carry-forward.
  No production recipe needs it today. Leave for a session that has
  a real consumer.

- **P2 EUR-Lex CELEX failure-case banner.** Still blocked awaiting
  the commit-vs-local decision. Not session-blocking.

## What did *not* change in Session 22

- No schema migration. The records-routing query is a substring
  LIKE on the existing `source_id` column — additive query work, no
  table changes.
- No prompt edits. The classifier and recipe-author prompts are
  unchanged.
- No security primitives touched. `stockpile_secure`, the
  `SecureHttpClient`, the SSRF guards, the API-key handling — all
  unchanged.
- No ADR additions. Session 22 ships within ADR 0007's existing
  shape; the records query is a query, not a new architectural
  surface.
- No production-prompt edits. The xAI prompt for both Level-1 and
  Level-2 is unchanged.
- No `Cargo.toml` dependency changes. All new code uses crates
  already in the workspace.

## Hard rules to carry forward (unchanged)

- **ADR 0009.** No fresh `reqwest::Client::new()`. All HTTP through
  `SecureHttpClient`.
- **IPC string bounds.** `check_user_text` / `check_string` for any
  user-supplied text at command boundaries. The new
  `records_for_plan` only takes a UUID id (parsed by `Uuid::parse`),
  which is sufficient validation.
- **Tauri commands return `CommandError`.** Including the new
  `records_for_plan`.
- **Generated TS at `apps/desktop/src/lib/api/types/`** — Session 22
  ships hand-mirrors for the 10 new files; the next
  `cargo test -p situation_room-api` will regenerate them. Drift
  surfaces as a diff; trust ts-rs.
- **Components use only CSS vars from `global.css`.** No fresh
  hex literals in component styles. (One exception inherited from
  the existing `PlanReview.svelte`: `rgba(224, 165, 46, 0.1)` for
  the warning button hover. Session 23 doesn't need to touch it.)
- **xAI API key never appears in any code, log, or string.**
- **Source URLs not hard-coded into descriptors.** rss_feeds remains
  StubExcerpt-by-design.
- **Standing order: security > generalisation > simplicity.**

## Local backups

There is no git remote. Conversation files do not persist on
Anthropic's servers. The patch tarball plus this handoff document
are the canonical record of Session 22 — keep both somewhere
durable before starting Session 23.

## Session 23 first action

Read this document. Read `SESSION22_README.md`. Read ADR 0007 (the
provenance-format ADR) if pursuing Option A. Then decide between
Option A and Option B (or a smaller follow-up) and proceed.

The operator's communication style remains terse: short responses
("go", "continue") signal approval; architectural pushback should
be taken seriously. Candor over rubber-stamping.
