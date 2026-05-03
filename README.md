# Session 15 patch

This patch applies on top of the post-Session-14 codebase (Session 14's
record-counts / SatisfactionPanel work integrated into `crates/api/`,
`crates/storage/`, `apps/desktop/`, and `docs/adr/0012-…`). If your
local workspace has not yet merged the Session 14 deliverables, do
that first.

## What this delivers

The full consensus design from the Session 15 conversation:

1. **Failure case writeup** for the UDB / EU AI Act framing-leak
   incident (Session 14 testing). Lives at
   `failure_cases/classification/2026-04-30-udb-eu-ai-act-framing-leak.md`.
   New parallel category (`classification/`) for classifier-quality
   failures, distinct from ADR 0012's runtime-failure classes
   (B/C/D/E in `failure_cases/class_b/`).

2. **`Bounds::REJECTION_REASON` + `check_user_text` validator** in
   `crates/secure/src/bounds.rs`. Length + control-char + zero-width
   + bidi-override rejection, with `\r` → `\n` normalization on
   accept. Comprehensive tests including adversarial payloads.

3. **Migration v7** (`migrations/0007_research_plans_rejection_and_lineage.sql`):
   adds `rejection_reason TEXT NULL` and `reclassified_from UUID NULL`
   on `research_plans`, plus an index on `reclassified_from` for
   future chain-walk queries. Both nullable to sidestep the DuckDB
   ALTER constraint trap from migration 0005.

4. **Storage layer** (`crates/storage/src/research_plans.rs`,
   `crates/storage/src/migrate.rs`): new `set_plan_rejection(id,
   reason)` method (split from `set_plan_status` so accept/un-reject
   paths don't clobber the reason); new fields on `ResearchPlanRow`
   and `StoredResearchPlan`; SELECTs and INSERTs updated; new tests
   covering round-trip, idempotence, the rejection-reason
   overwrite/clear semantics, and the lineage column.

5. **Pipeline layer**:
   - `crates/pipeline/src/research_plans_store.rs` — adds
     `save_research_plan_with_lineage`, keeps `save_research_plan`
     as a thin wrapper.
   - `crates/pipeline/src/research_classifier.rs` —
     `ClassificationContext.previous_rejection_reason`,
     `{{USER_FEEDBACK}}` placeholder substitution with a per-call
     UUID nonce in fence delimiters, and case-insensitive
     sanitization of literal closing-tag occurrences. Fresh tests
     including adversarial payloads (bare closing tag, role
     override, pasted transcript with stale nonce).

6. **API layer** (`crates/api/src/{commands,types_export}.rs`):
   - `reject_plan(id, reason: Option<String>)` — was 1-arg, now
     accepts an optional reason (validated through
     `check_user_text` at the boundary, returns the normalized
     string to storage).
   - **New** `reclassify_plan(id, edited_reason: Option<String>)` —
     loads the rejected plan, resolves the effective reason
     (edited > stored > error), runs Level-1 classification with
     `previous_rejection_reason` populated, persists the new plan
     via `save_research_plan_with_lineage`, returns the new
     `ResearchPlanDto`.
   - `ResearchPlanDto` gains `rejection_reason` and
     `reclassified_from` (empty-string-as-absent wire convention,
     consistent with `unit_hint` and `display`).
   - `PlanSummary` gains `has_rejection_reason: bool` and
     `is_reclassified: bool` so the listing can show indicators
     without dragging the full text through the summary payload.

7. **Frontend** (`apps/desktop/`):
   - `lib/api/client.ts` — `rejectPlan(id, reason?)` and new
     `reclassifyPlan(id, editedReason?)`.
   - `stores/plans.svelte.ts` — `rejectSelected(reason?)` (replaces
     the old form), new `reclassifySelected(editedReason?)`.
   - `components/PlanReview.svelte` — reject button now opens the
     dialog; rejected plans show a "re-classify" button (re-uses
     the same dialog with the stored reason pre-filled); a lineage
     banner appears for plans with `reclassified_from`; a rejection-
     note panel appears under the trust paragraph for rejected
     plans.
   - **New** `components/dialogs/RejectDialog.svelte` — modal with
     soft-warn at 800 chars, hard limit at 2,000, Cmd/Ctrl+Enter
     submits, Escape cancels.
   - Generated TS types `ResearchPlanDto.ts` and `PlanSummary.ts`
     updated to match the Rust DTOs. (When you run `cargo test
     --package situation_room-api`, ts-rs may regenerate these in
     a slightly different formatting; treat ts-rs's output as
     canonical and commit the regenerated form.)
   - Desktop binary's `invoke_handler!` registers the new
     `reclassify_plan` command.
   - CLI binary's `ClassificationContext` construction picks up
     the new field with `previous_rejection_reason: None`.

8. **Prompt v1.4 bumps**:
   - `config/prompts/research_classifier.md` — substantive-reuse
     discipline replacing "plausibly about the same subject"; UDB
     anti-example; interpretation-honesty rule for associative
     reuse; new `{{USER_FEEDBACK}}` section.
   - `config/prompts/recipe_author.md` — `headline = extracted` is
     the default with a three-condition predicate for `literal`;
     explicit "do not lift plan-interpretation framing into
     literal per-record fields" rule.

## How to apply

The tarball is structured for `--strip-components=1` from the repo
root:

```bash
tar -xzf ~/Downloads/session_15_patch.tar.gz \
    --strip-components=1 \
    -C /Users/aben/RustroverProjects/situation_room
```

The patch overwrites canonical paths under `crates/`, `apps/`,
`config/`, `migrations/`, and adds new files under
`failure_cases/classification/`. No existing files are deleted.

## Build verification order

The patch is structured so each layer can be verified independently
before moving up:

```bash
# 1. Storage / secure first — foundational, no upstream callers.
cargo test --package situation_room-secure
cargo test --package situation_room-storage

# 2. Pipeline next — depends on storage + secure.
cargo test --package situation_room-pipeline

# 3. API + binaries last.
cargo test --package situation_room-api
cargo test --workspace

# 4. ts-rs types — regenerate and check for diff.
cargo test --package situation_room-api
git diff apps/desktop/src/lib/api/types/  # should be empty after regen

# 5. Frontend.
cd apps/desktop
npm run check
npm run dev  # smoke test in browser
```

If anything in step 1 or 2 fails, stop and surface the error — the
upper layers depend on those compiling cleanly.

## Known things to verify

I built this patch in a sandbox with no Rust toolchain and no
network — every type signature is cross-referenced against the
existing crates by eyeball, but **none of it has been compiled**.
First-build issues to watch for:

1. **Test fixtures using `Uuid::now_v7()`** — I added new tests in
   `crates/api/src/types_export.rs` that use `uuid::Uuid::now_v7()`
   (qualified) to match the existing pattern. If the existing tests
   imported it differently, you may see a path resolution error.

2. **`StoredResearchPlan` field order in test literals** — the
   storage tests construct `StoredResearchPlan { ... }` with the
   new `rejection_reason` and `reclassified_from` fields at the
   end. If the original used struct-update syntax (`..Default::default()`)
   or named-only construction, the order doesn't matter. If it
   relied on positional construction anywhere, the new fields
   need to land in the right place.

3. **The `tauri::command` async signature** — `reclassify_plan`
   takes `id: String, edited_reason: Option<String>, state:
   tauri::State<'_, AppState>`. Tauri 2 requires the `State`
   parameter last; that's where it is. If a macro error appears,
   the parameter order is the first thing to check.

4. **ts-rs regeneration** — my hand-written TS files
   (`ResearchPlanDto.ts`, `PlanSummary.ts`) match what ts-rs 9.0
   should emit from the new struct shapes. If the regenerated form
   is different (different field order, different `Array<T>` vs.
   `T[]` style), prefer ts-rs's output.

5. **Existing test in `commands.rs`** — `storage_not_found_maps_…`
   and the three `command_error_*_serializes` tests should still
   pass unchanged; my edits didn't touch the `CommandError` shape.

## Things this patch does NOT do

These were considered and explicitly deferred:

- **Recipe-author v1.4 verification on the UDB case.** The prompt
  bumps are in this patch; running them against the original UDB
  topic and observing the fix is the verification step that
  belongs to the user, not to this patch. The
  `failure_cases/classification/…` writeup has a "Verification:
  pending" subsection that should be filled in after the user
  re-runs the case.

- **A separate `rejection_events` audit table.** The minority
  report suggested it; we chose `reclassified_from` chains plus
  the latest-reason-on-row instead. To reconstruct history, walk
  via `reclassified_from`. If chain-walking-UI ever needs all
  prior reasons rather than just the latest, that's an additive
  schema change.

- **NFC Unicode normalization in `check_user_text`.** Considered
  and rejected: the homoglyph attack the minority report worried
  about (Cyrillic `а` vs. Latin `a`) is not caught by NFC anyway,
  and the per-request UUID nonce in the classifier prompt's fence
  delimiters defeats the homoglyph fence-breakout case directly.
  Adding the `unicode-normalization` crate would be cost without
  the security benefit.

- **An adversarial-example one-shot in the classifier prompt.**
  The minority report suggested it; we declined because adding
  adversarial content to system prompts has known failure modes
  (the model can over-anchor on the example, treat near-matches
  as the only injection patterns to worry about, or echo the
  example in legitimate output). Test before adopting; don't ship
  on intuition.

- **Topic-input parity treatment.** I considered also fencing the
  `{{TOPIC}}` placeholder with a per-call nonce. Skipped because
  the existing `RESEARCH_TOPIC` length bound + classifier prompt
  context already constrain the topic surface meaningfully, and
  the user supplies the topic interactively (whereas rejection
  reasons are more likely to be paste-prone). Worth revisiting if
  observed misuse warrants it.

## File list

```
failure_cases/classification/
  README.md
  2026-04-30-udb-eu-ai-act-framing-leak.md

migrations/
  0007_research_plans_rejection_and_lineage.sql

crates/secure/src/
  bounds.rs

crates/storage/src/
  migrate.rs
  research_plans.rs

crates/pipeline/src/
  research_classifier.rs
  research_plans_store.rs

crates/api/src/
  commands.rs
  types_export.rs

apps/desktop/src-tauri/src/
  main.rs

apps/desktop/src/
  lib/api/client.ts
  lib/api/types/PlanSummary.ts
  lib/api/types/ResearchPlanDto.ts
  stores/plans.svelte.ts
  components/PlanReview.svelte
  components/dialogs/RejectDialog.svelte

apps/situation_room/src/
  main.rs

config/prompts/
  research_classifier.md
  recipe_author.md
```
