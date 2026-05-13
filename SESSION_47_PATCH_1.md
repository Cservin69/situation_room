# Session 47 — Patch 1

The architectural piece from the Session 47 handoff: multi-recipe per
nomination. One nomination now drives up to N authoring calls (one
per target expectation) against the same prefetched bytes, so a
single source whose prefetch supports multiple expectations (the
lithium MCS PDF carrying both production and reserves was the
motivating case from end-of-Session-45's live run) can author N
narrow recipes — each targeting one expectation — instead of one.

Single tarball / one commit pair per the handoff's explicit
"Bundle the multi-recipe architectural piece with anything else.
Don't" rule.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
```

No new dependencies. No schema change. No migration. The change
threads `Option<ExpectationRef>` through the recipe-author public
API, bumps the recipe-author prompt to v1.15, refactors the
executor's authoring loop into a one-propose-per-attempt /
N-author-per-attempt shape, and widens the per-recipe `dedup_key`
from `{plan_id}:{nomination_id}` to
`{plan_id}:{nomination_id}:{bucket}:{index}`.

Pure additive on the wire / DB shape: the `dedup_key` column is
`TEXT` and accepts the wider key without DDL; existing recipes
authored under the legacy shape continue to load and apply (their
keys are still unique under the wider naming convention because no
post-Session-47 recipe will ever produce the legacy shape).

## Files changed

### Recipe-author prompt

- `config/prompts/recipe_author.md` —
  - Version heading bumped from v1.14 to v1.15 with a dated
    changelog entry naming the output-contract change.
  - New placeholder `{{TARGET_EXPECTATION}}` rendered between
    `## The plan you are authoring for` and the existing feedback
    placeholders. The placeholder substitutes to a markdown
    subsection naming the bucket, the index, and the human-readable
    label resolved from the plan; substitutes to the empty string
    on the manual re-author path so that path's contract is
    unchanged.
  - "Coverage discipline" section gained a Session-47 paragraph:
    when the target-expectation section names a specific
    expectation, every binding must reference that expectation;
    bucket coverage is now achieved by the *set* of recipes the
    executor authors across multiple calls, not by padded
    `produces` arrays inside any single recipe.
  - "What NOT to produce" gained one new bullet: "do not target a
    different expectation than the one named — the executor will
    call you again for the other expectation against the same
    prefetched bytes."
  - The prompt's "Before reading the closed vocabulary" checklist
    moved its first bullet (which expectation bucket you intend to
    populate) into the placeholder; the checklist now opens with
    "the metric name / event_type / etc. from the target
    expectation." The reframing is principled, not source-
    specific.

### Pipeline / recipe author

- `crates/pipeline/src/recipe_author.rs` —
  - `build_prompt` and `build_prompt_with_fence_id` gained a
    `target_expectation: Option<ExpectationRef>` parameter (third
    positional, before `fence_id` on the latter). When `Some`, the
    `{{TARGET_EXPECTATION}}` placeholder substitutes to a markdown
    subsection; when `None`, it substitutes to the empty string.
  - New free function `render_target_expectation(target, plan)`
    builds the substitution. Bucket name strings match the
    Session-47 dedup_key shape (`observation_metric` /
    `event_type` / `entity_kind` / `relation_kind` /
    `document_source`); label resolution looks up the bucket's
    name field on the plan and degrades gracefully when the index
    is out of range. **No source-specific text — the renderer
    reads only the plan and the target reference.**
  - `author_recipe` gained `target_expectation: Option<ExpectationRef>`
    as the 7th parameter. `#[allow(clippy::too_many_arguments)]`
    targeted to the function. Threads through to `build_prompt`
    and `build_validated_recipe`.
  - `build_validated_recipe` gained the same parameter (4th
    positional). New step 4a: when `Some(target)`, every binding's
    `expectation` must equal `target`; mismatches return
    `AuthoringError::InvalidRecipe` with a message naming both
    the offending binding and the constrained target. Checked
    before step 5's duplicate-expectation check because the
    constraint failure is the more informative diagnostic.
  - `reauthor_recipe` (legacy free-choice path) passes `None`
    through. Track A's manual re-author flow continues to work
    identically.
  - Module rustdoc on `build_prompt` updated to enumerate the
    v1.15 placeholder set.

### Pipeline / fetch executor

- `crates/pipeline/src/fetch_executor.rs` —
  - New constant `MAX_AUTHORS_PER_NOMINATION: usize = 4` bounds the
    per-nomination LLM call count. Documented rationale: covers a
    small research plan's top-priority bucket fully; keeps a
    7-nomination plan under ~30 author calls per fetch run in the
    typical case.
  - New `expectation_ref_parts(ExpectationRef) -> (&'static str, u32)`
    closed-lookup helper. Returns the bucket-string + index pair
    used by `dedup_key_for_recipe` and
    `derive_source_id_for_decline`. Same vocabulary as the v1.15
    prompt's `{{TARGET_EXPECTATION}}` rendering.
  - New `dedup_key_for_recipe(plan_id, nomination_id, target)`
    builds the per-recipe dedup_key under the widened shape:
    `{plan_id}:{nomination_id}:{bucket}:{index}`. Pre-Session-47
    `{plan_id}:{nomination_id}` is dead — every authored recipe
    now flows through this helper.
  - New `build_target_expectations(plan, max)` enumerates the
    plan's record-typed buckets (observation_metric → event_type
    → entity_kind → relation_kind, in declaration order) and
    truncates to `max`. **document_source is excluded** because
    the nomination *is* a document_source entry; targeting it
    would have the source authoring a record about itself.
  - New private struct `ExpectationDecline { expectation, reason }`
    carries one per-target decline from the new authoring
    orchestrator.
  - `derive_source_id_for_decline` widened to take an
    `Option<ExpectationRef>`. `None` returns the legacy
    `nom:{nomination_id}` shape (preserved for nomination-level
    declines so RecipeFlagDialog wiring keeps operating against
    the nomination's standing identity). `Some(target)` returns
    `nom:{nomination_id}:{bucket}:{index}` mirroring the
    `dedup_key_for_recipe` shape.
  - `author_one` removed. The retry loop and the per-target
    iteration are now interleaved in one consolidated function.
  - `author_for_nomination(ctx, plan, nomination)` — new function
    returning `(Vec<FetchRecipe>, Vec<ExpectationDecline>,
    Option<String>)`. The third element is the nomination-level
    decline reason when URL discovery itself fails (propose-URL
    declined on first attempt, deadline elapsed, or every URL
    produced no recipe for any target); `None` when at least one
    URL locked and at least one target authored.
  - **Two interleaved loops.** Outer loop (up to
    `MAX_AUTHORING_ATTEMPTS_PER_SOURCE`) discovers a candidate URL
    via `propose_source_url` (target-agnostic) and pre-fetches
    bytes. Inner loop iterates `build_target_expectations` and
    calls `author_recipe` once per target against those bytes.
    First target that authors locks the URL; subsequent declines
    surface as `ExpectationDecline` entries against the locked
    URL (no re-fetch for declined targets). If every target
    declines against an attempt's URL, that URL is recorded as a
    prior attempt and the outer loop retries with a fresh
    `propose_source_url` call.
  - `load_or_author_recipes` consumes the new 3-tuple from
    `author_for_nomination`: persists the recipes, projects
    per-target declines into `RecipeOutcome::Declined` entries
    with widened `source_id`, and projects the nomination-level
    decline (if set) into one `RecipeOutcome::Declined` with the
    legacy `nom:{nomination_id}` shape.
  - Unused import `load_latest_recipes_for_plan` removed (it was
    imported but never used in the file).

### Tests

- `crates/pipeline/src/recipe_author.rs` (test module) — eight
  new tests pinning the constraint:
  - `target_expectation_match_validates_session_47` —
    same-target-as-binding round-trips through the validator.
  - `target_expectation_mismatch_is_rejected_session_47` — a
    binding targeting a different index of the same bucket is
    rejected with `InvalidRecipe`.
  - `target_expectation_cross_bucket_mismatch_is_rejected_session_47`
    — a binding targeting a different bucket is rejected.
  - `target_expectation_none_preserves_free_choice_session_47` —
    legacy reauthor path's `None` constraint accepts the LLM's
    own choice.
  - `target_expectation_decline_short_circuits_constraint_session_47`
    — a non-empty `decline_reason` short-circuits the
    constraint check (decline path runs first per
    `build_validated_recipe`'s ordering rationale).
  - `target_expectation_renders_into_prompt_session_47` —
    `{{TARGET_EXPECTATION}}` substitution carries bucket name,
    label, and the constraint sentence into the rendered prompt.
  - `target_expectation_none_renders_empty_session_47` —
    placeholder substitutes to empty when no target is named.
  - `target_expectation_event_type_renders_label_session_47` —
    label resolution works for non-observation buckets.

- `crates/pipeline/src/fetch_executor.rs` (test module) — eight
  new tests pinning the helper shapes:
  - `build_target_expectations_concatenates_buckets_in_declaration_order_session_47`
    — concatenation order is observation → event → entity →
    relation per the plan's declaration order.
  - `build_target_expectations_truncates_to_cap_session_47` —
    per-nomination cap fires honestly when the first bucket
    fills it.
  - `build_target_expectations_empty_plan_yields_empty_session_47`
    — degrades to empty for plans with no record-typed
    expectations (the orchestrator surfaces this as a
    nomination-level decline).
  - `build_target_expectations_excludes_document_source_bucket_session_47`
    — document_source is never returned (the recipe-targeting-
    its-own-source circular case).
  - `dedup_key_for_recipe_widens_with_bucket_and_index_session_47`
    — pinned shape `{plan_id}:{nomination_id}:{bucket}:{index}`.
  - `dedup_key_for_recipe_distinguishes_siblings_under_same_nomination_session_47`
    — three siblings under one nomination produce three
    pairwise-distinct keys.
  - `derive_source_id_for_decline_legacy_shape_when_no_target_session_47`
    — `None` target returns `nom:{full-uuid}` (40 chars, Session
    40 uniqueness invariant preserved).
  - `derive_source_id_for_decline_widens_when_target_provided_session_47`
    — `Some(target)` returns the widened shape mirroring
    `dedup_key_for_recipe`'s vocabulary.
  - `expectation_ref_parts_round_trips_buckets_session_47` —
    closed-lookup vocabulary is internally consistent across all
    five `ExpectationRef` variants.

## What's NOT in scope

- **Piece B (per-host backoff status panel).** The `snapshot()`
  accessor and `HostBackoffSnapshotDto` ship in Session 46; the
  IPC + Svelte panel remains drive-by-ready for a future session.
- **Piece C (sources-memory panel).** Carried over from the
  Session 46 handoff; still un-shipped.
- **Promotion pipeline (ADR 0004).** Substantial piece; its own
  handoff per the explicit out-of-scope note in the Session 47
  handoff.
- **Iterator Phase 2 (ADR 0016).** Its own session.
- **Charts on Observations / Events.** Hold until promotion lands.
- **Re-author flow constraint.** The manual re-author path
  (`reauthor_recipe`) passes `None` and continues to work as
  before. Threading the original recipe's expectation through as
  a constraint is a future tightening — today's recipes can be
  re-authored against any expectation the LLM chooses; the new
  initial-author path is constrained, but the re-author path
  trusts the operator's intent.
- **Migration of pre-Session-47 recipes.** Recipes authored
  before this patch keep their legacy `{plan_id}:{nomination_id}`
  dedup_keys; they continue to load and apply. New recipes use
  the wider shape. The two coexist in the same column without
  interference because the legacy shape never collides with the
  wider shape (same prefix, different suffix lengths).

## Test deltas

- `crates/pipeline/src/recipe_author.rs` — 8 new tests.
- `crates/pipeline/src/fetch_executor.rs` — 8 new tests.

Pipeline test count: 319 → 335 (16 net). Other crates' counts
unchanged. All ignored tests (12) remain the existing `#[ignore]`
live integration tests.

End of patch.
