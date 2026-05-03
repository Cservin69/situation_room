# SESSION 19 — patch README

ADR 0013 (recipe feedback channel) drafted, accepted, implemented
end-to-end. Single combined patch covering all phases — storage,
pipeline, API, frontend, prompt v1.7 → v1.8.

## Apply

From the repo root (`/Users/aben/RustroverProjects/situation_room`):

```sh
tar -xzf ~/Downloads/session19_recipe_feedback.tar.gz --strip-components=1 -C .
```

Then, in this order:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd apps/desktop && pnpm install && pnpm build
```

## What's in the tarball

17 files. None deleted.

```
docs/adr/0013-recipe-feedback-channel.md          NEW — design rationale, resolves Session 19's three open questions
migrations/0009_recipe_feedback.sql               NEW — fresh CREATE TABLE (no ALTER → no DuckDB pitfall)

crates/storage/src/recipe_feedback.rs             NEW — set/clear/get/list, 7 unit tests, ON CONFLICT upsert
crates/storage/src/lib.rs                         EDIT — `pub mod recipe_feedback`, re-exports
crates/storage/src/migrate.rs                     EDIT — register migration v9

crates/secure/src/bounds.rs                       EDIT — `Bounds::RECIPE_FEEDBACK = 2_000`

crates/pipeline/src/recipe_author.rs              EDIT — AuthoringContext.recipe_feedback,
                                                          render_recipe_feedback,
                                                          sanitize_for_fence,
                                                          build_prompt_with_fence_id,
                                                          {{RECIPE_FEEDBACK}} placeholder,
                                                          10 new tests
crates/pipeline/src/fetch_executor.rs             EDIT — author_one reads feedback before AuthoringContext

crates/api/src/types_export.rs                    EDIT — RecipeFeedbackDto + roundtrip test
crates/api/src/commands.rs                        EDIT — set_recipe_feedback, list_recipe_feedback_for_plan,
                                                          MAX_RECIPE_FEEDBACK_LISTING

apps/desktop/src-tauri/src/main.rs                EDIT — register two new commands

apps/desktop/src/lib/api/types/RecipeFeedbackDto.ts   NEW — hand-mirror of ts-rs output
apps/desktop/src/lib/api/client.ts                EDIT — setRecipeFeedback, listRecipeFeedbackForPlan
apps/desktop/src/stores/plans.svelte.ts           EDIT — recipeFeedback state map,
                                                          flagRecipe, clearRecipeFeedback,
                                                          refreshRecipeFeedback,
                                                          select/clear plan integration
apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte   NEW — modeled on RejectDialog
apps/desktop/src/components/RecipesPanel.svelte   EDIT — FLAGGED chip + flag button + dialog mount,
                                                          recipe-head layout fix

config/prompts/recipe_author.md                   EDIT — v1.7 → v1.8, new operator-feedback section,
                                                          changelog entry
```

## What the operator does

After applying, in the desktop app:

1. Classify a topic, accept the plan, run fetch (existing flow).
2. The recipes panel shows each authored recipe with the existing
   layout, plus a small `flag` button on the right side of the
   recipe-head.
3. Click `flag` → modal opens with a textarea. Type a note
   ("the regex matched the channel `<title>`, not the article
   titles"). Submit.
4. The recipe head now shows a `FLAGGED` chip in `--signal-info`
   blue. Hover the chip to read the note. Click the chip to edit
   or clear.
5. Re-run fetch. The next authoring call for the flagged
   `(plan_id, source_id)` pair sees the note as a fenced markdown
   section in the recipe-author prompt and (in the LLM's terms)
   gets the chance to author a different recipe.

## What it does NOT do (intentionally)

- Auto-trigger re-authoring when a flag is submitted. ADR 0012's
  manual-first regime is unchanged. The flag persists the note;
  the operator decides when to re-run fetch.
- Show the note in the failure-case workflow. ADR 0013 §"Separation
  from failure cases" — those are global prompt artifacts, this is
  plan-local feedback.
- Localize the dialog or the chip's tooltip. The operator's note
  is rendered in whatever language they typed.

## Important context

- **Patch assembled in a sandbox without a Rust toolchain.** The
  code was type-checked by eye against the existing codebase and
  not compiled in-session. **Verified green on first run by the
  operator** — `cargo build`, `cargo test`, `cargo clippy`, and
  `pnpm build` all clean, no flags or failures. The note is
  preserved here for the methodology record (see
  `SESSION20_HANDOFF.md` §"Why this session built green on first
  try"); future contributors should not infer that "no toolchain"
  patches normally land green, only that this one did because
  the discipline that produced it was deliberate.
- **Hand-mirrored TS file.** `RecipeFeedbackDto.ts` will be
  overwritten by ts-rs on the next `cargo test --package
  situation_room-api`. The hand-mirror exists so the SvelteKit
  build succeeds before the first test pass; if ts-rs's regenerated
  output diverges from the hand-mirror, take the regenerated file
  as canonical.
- **EUR-Lex CELEX-instance failure case is intentionally deferred
  to post-go-live.** The recipe feedback channel exists precisely
  to make per-(plan, source) corrections cheap, so the global
  prompt doesn't have to absorb every edge case. See
  `SESSION20_HANDOFF.md` §"EUR-Lex CELEX-instance failure case —
  DEFERRED" for the full reasoning. Do not iterate on the EUR-Lex
  failure case in Session 20.

## Read first if anything looks off

In order:

1. `docs/adr/0013-recipe-feedback-channel.md` — the design
   document, including the §"Code references" map.
2. `crates/pipeline/src/research_classifier.rs::render_user_feedback`
   — the pattern that `render_recipe_feedback` is modeled on,
   line-for-line. If the new function looks unfamiliar, the
   classifier version is the canonical reference.
3. `crates/storage/src/fetch_runs.rs` — closest precedent for the
   storage module shape (lock-and-execute, manual row iteration,
   `crate::Result` return type).
4. `apps/desktop/src/components/dialogs/RejectDialog.svelte` — the
   precedent for the new flag dialog. Layout, styles, and prop
   shape are deliberately parallel; the only material divergence is
   the `--signal-info` button hue (informational, not destructive).

End of README.
