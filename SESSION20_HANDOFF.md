# SESSION 20 — handoff

**Prior session:** Session 19 (2026-05-02)
**Repo:** `/Users/aben/RustroverProjects/situation_room`
**Patch landed:** Recipe feedback channel (ADR 0013) — full P2 of
the Session 19 priorities.

---

## What Session 19 shipped

ADR 0013 (recipe feedback channel) — drafted, accepted, implemented
end-to-end. The operator can now flag a recipe in the inspection
panel with a free-text note explaining what's wrong; the note
persists per-`(plan_id, source_id)` and is fed back into the LLM
the next time recipe-authoring runs for that pair via the new v1.8
prompt's `{{RECIPE_FEEDBACK}}` placeholder.

Resolves the three open questions Session 19's handoff flagged:

- **Keying** — per-`(plan_id, source_id)`. Survives `dedup_key`-
  bumped re-authoring (where keying by `recipe_id` would silently
  abandon the note at the moment we want it to be visible).
- **History** — overwrite, not append. Mirrors the
  `set_plan_rejection` precedent. If a future consumer earns a
  history table, it can be added additively.
- **Length bound** — `Bounds::RECIPE_FEEDBACK = 2_000`,
  named-distinct from `REJECTION_REASON` (same value) so call sites
  read cleanly and the constants can diverge cleanly later.

### What landed in code

| Layer | File | Notes |
|---|---|---|
| ADR | `docs/adr/0013-recipe-feedback-channel.md` | Resolves the open questions; lists triggers for amendment/supersession |
| Migration | `migrations/0009_recipe_feedback.sql` | Fresh CREATE TABLE — sidesteps the DuckDB ALTER pitfall |
| Storage | `crates/storage/src/recipe_feedback.rs` | `RecipeFeedbackRow`, `StoredRecipeFeedback`, set/clear/get/list. 7 unit tests. Upsert via `ON CONFLICT (plan_id, source_id) DO UPDATE` |
| Storage | `crates/storage/src/{lib,migrate}.rs` | Module + migration registration |
| Bound | `crates/secure/src/bounds.rs` | `Bounds::RECIPE_FEEDBACK` (named-distinct alias of REJECTION_REASON value) |
| Pipeline | `crates/pipeline/src/recipe_author.rs` | `AuthoringContext.recipe_feedback`, `build_prompt_with_fence_id`, `render_recipe_feedback`, `sanitize_for_fence`. 10 new tests |
| Pipeline | `crates/pipeline/src/fetch_executor.rs` | `author_one` reads `recipe_feedback_for_source` from store before assembling AuthoringContext (logs + continues on storage error — feedback is a hint, not a precondition) |
| API DTO | `crates/api/src/types_export.rs` | `RecipeFeedbackDto` + `from_stored` impl + roundtrip test |
| API command | `crates/api/src/commands.rs` | `set_recipe_feedback` (single command for set + clear, mirrors `reject_plan`'s shape), `list_recipe_feedback_for_plan`. `MAX_RECIPE_FEEDBACK_LISTING = 100` |
| API wire | `apps/desktop/src-tauri/src/main.rs` | Two new commands registered in `invoke_handler!` |
| TS | `apps/desktop/src/lib/api/types/RecipeFeedbackDto.ts` | Hand-mirrored from ts-rs output. Will be regenerated on next `cargo test --package situation_room-api` |
| TS client | `apps/desktop/src/lib/api/client.ts` | `setRecipeFeedback`, `listRecipeFeedbackForPlan` invokers |
| Store | `apps/desktop/src/stores/plans.svelte.ts` | `recipeFeedback: Record<source_id, RecipeFeedbackDto>`, `flagRecipe`, `clearRecipeFeedback`, `refreshRecipeFeedback`. Optimistic with rollback on error |
| Dialog | `apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte` | Modeled on RejectDialog with adapted copy. Distinct semantic hue: `--signal-info` (informational annotation, not destructive) |
| Panel | `apps/desktop/src/components/RecipesPanel.svelte` | FLAGGED chip when a note exists (clickable to edit), `flag` button when not. Layout fix: `margin-left: auto` on `.recipe-id` so the variable trailing slot doesn't fight for `space-between` position |
| Prompt | `config/prompts/recipe_author.md` | v1.7 → **v1.8**. New `{{RECIPE_FEEDBACK}}` placeholder between the plan and the source context. Empty when no feedback (the common case); a fenced markdown section with per-call UUID nonce when present |

### Security posture

The feedback channel inherits the classifier's defense-in-depth
discipline. The note travels through `check_user_text` against
`Bounds::RECIPE_FEEDBACK` at the api boundary (length + control
chars + zero-width + bidi-override + line-ending normalization).
The recipe-author prompt then renders it inside a `<recipe_feedback
id="...">` fence carrying a per-call UUID nonce; the closing-tag
sanitizer catches the literal-tag pattern (case-insensitive bare
form + nonced form) as a belt-and-suspenders layer. The fence tag
is *distinct* from the classifier's `<user_feedback>` so any prompt
that ever carries both stays unambiguous to the LLM.

No new HTTP path. No new LLM provider call. ADR 0009 §"The rule"
satisfied.

### Test count posture

The patch added (eyeballed, not run):

- 7 storage tests in `recipe_feedback.rs`
- 10 pipeline tests in `recipe_author.rs` (rendering + build_prompt
  placeholder behaviour)
- 1 api roundtrip test in `types_export.rs`

That's +18 against the Session 18 baseline of 340. **Session 20's
first move is `cargo build --workspace && cargo test --workspace
&& cargo clippy --workspace --all-targets -- -D warnings`** — the
patch was assembled against read-only mounts in a sandbox without a
Rust toolchain, so all type-checking was eyeballed. Anything the
compiler flags at first build is the priority.

---

## What was *not* shipped, intentionally

### P1 (empirical verification of v1.7) — still owed by the operator

Session 19's stated default was "complete P1 before drafting ADR
0013." That ordering was inverted under explicit instruction:
network access wasn't available in-session, the patch operator
chose to ship P2 anyway, and ADR 0013 was drafted to be robust to
P1's likely findings (it doesn't change v1.7's content; it adds an
orthogonal channel above it, and the prompt bump is additive).

The three P1 verification slots from Session 19's handoff still
need running, ideally in this order:

1. **HTML-equivalent path** (cheapest sanity check) — confirm v1.8
   picks the HTML route when both PDF and HTML are addressable.
   Candidate: USGS MCS.
2. **CELEX re-run** (closes an open failure case) — see the
   deferred-by-design note below before doing this one.
3. **BAKED PDF source** (highest-risk new behaviour) — confirm
   `static_payload` end-to-end on a PDF-only source.

P1 verifies v1.7-or-later content. Now that v1.8 is the production
prompt and adds a feedback channel — but does NOT modify v1.7's
existing sections — the P1 results inform whether the v1.7
*content* still works; v1.8's additive section is verified
separately the first time an operator flags a recipe and re-runs
fetch.

### EUR-Lex CELEX-instance failure case — DEFERRED to post-go-live

**Operator instruction (recorded verbatim from Session 19):**
"the EUR-lex we left it intentionally but I want it to postpone for
post go-live as it is an edge case. All sessions have a quarrel
with that."

**What this means for Session 20 and onward:** Do NOT iterate on
`apps/desktop/failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`.
Do NOT bump the recipe-author prompt to address its specific
pattern. Do NOT add a new failure case for an EUR-Lex CELEX
recurrence; record the recurrence (one line, with date) at the
bottom of the existing file but otherwise pass it.

The decision discipline:

- **It is an edge case.** EUR-Lex CELEX instance URLs (one
  regulation page) being a structurally bad target for a
  multi-expectation event-type bucket is a real failure pattern,
  but the topics it actually arises on — specific named regulations
  with multiple enforcement events — are a small slice of plausible
  research topics for go-live.
- **Multiple sessions have already iterated on it.** v1.5 added
  endpoint discipline; v1.6 added "hunt the URL end-to-end"; v1.7
  added the static-payload fallback. Each addition was prompted by
  a re-run that the previous iteration didn't fully solve.
  Continuing to bump the prompt for this one case has hit
  diminishing returns and risks polluting the prompt's general
  guidance with an EUR-Lex-shaped wart.
- **Post-go-live is the right time to revisit.** Once Stockpile
  has live operator usage, the EUR-Lex case will either recur
  often enough to justify a focused fix (at which point we have
  real recurrence data, not three sessions' worth of speculative
  prompt-tuning) or it will recur rarely enough to confirm the
  edge-case classification.

**The file stays.** The failure case is documented; the
verification block records what's been tried; the deferral is now
recorded both here and (per Session 20's first cleanup task — see
below) at the top of the failure-case file itself.

If a later session is tempted to re-open this — because a real
plan came back with the same shape, because the prompt has new
slack tokens, because v1.8's feedback channel made a new
intervention possible — read this paragraph again. Read the
existing failure case's Verification block. Then consider whether
go-live has happened yet. If not, defer.

The recipe feedback channel from this session makes deferral
*operationally cheap*: an operator who runs into the EUR-Lex case
in the wild can flag the recipe with a concrete note, and the
next authoring run for that specific (plan, source) pair gets the
correction without touching the global prompt. That's exactly the
shape the channel was designed for. Use the channel; do not bump
the prompt.

### Other deferred items (from Session 19 / earlier handoffs)

- **`pdf_table` extraction-mode removal.** Session 19's gate was:
  "defer until at least one full session goes by where the LLM,
  given v1.7's strategy, never authors a `pdf_table` recipe."
  Session 19 didn't run real plans. Session 20's verification work
  (P1 above) is the observation window: tally `pdf_table` recipes
  authored during P1 + any operator runs. If zero across all runs,
  Session 21 can cleanly delete the variant. If non-zero, the
  variant stays.
- **Endpoint_hint coverage sweep** on `config/sources.toml` (7 of
  12 sources still without an `endpoint_hint`). Per Session 16:
  reading source documentation, better as a user-driven sweep than
  an LLM session.
- **Per-expectation SatisfactionPanel** (Session 16 P4). UI work,
  independent of prompt and runtime layers. Defer.
- **Repo-root cleanup** of accumulated `SESSION*` patch READMEs
  and `*.broken-2026-05-01` DB files. Risk of deleting something
  with non-obvious value. Defer to a deliberate housekeeping pass
  with the Git remote question on the table.

---

## Session 20 priorities

### P1 — Build, test, clippy (the patch is unverified)

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd apps/desktop && pnpm install && pnpm build
```

In that order. Anything the compiler flags is the actual P1 of this
session. Likely candidates if anything fails (eyeballed against the
existing codebase, but not exhaustive):

- The `chrono::Utc::now()` call in `commands.rs::set_recipe_feedback`
  uses the fully-qualified path; if rustc complains about ambiguity,
  add `use chrono::Utc;` near the top of the file (the rest of the
  module already uses chrono types, so this would be tidy anyway).
- The hand-mirrored `RecipeFeedbackDto.ts` will be overwritten by
  ts-rs on first `cargo test --package situation_room-api`. Verify
  the generated file matches what the hand-mirror has; if not,
  ts-rs's output is canonical.
- The Svelte 5 reactivity on
  `plans.recipeFeedback[sourceId] = ...` mutation should work via
  the runes proxy, but if the chip doesn't re-render after `flagRecipe`
  resolves, `plans.recipeFeedback = { ...plans.recipeFeedback }`
  (whole-object reassignment) is the fix.

### P2 — P1 verification (live xAI runs of v1.8)

Per the deferred-from-Session-19 list above, in order:

1. HTML-equivalent path (USGS MCS).
2. CELEX re-run — but only as a *recording* exercise, NOT as a
   prompt-iteration trigger. Append the v1.8 outcome to the
   Verification block; do not edit the prompt in response.
3. BAKED PDF source.

### P3 — Cosmetic: top-of-file deferral note in the EUR-Lex failure case

Add a short Status banner at the top of
`apps/desktop/failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`
recording the post-go-live deferral. Two or three sentences. The
purpose is so a future session reading the failure case can't miss
the deferral when scanning the document. Refer back to this
handoff's §"EUR-Lex CELEX-instance failure case — DEFERRED" for
the rationale.

### P4 (optional, time-permitting) — Operator usability of the new dialog

Once the build is green and the channel is live, a small UX pass
over the FLAGGED chip + flag button placement is worth a glance.
Session 19's screenshot showed the existing recipe panel quite
densely packed; the new chip + button add modest visual weight to
each card. Watch for: chip-id-button collision in narrow viewports,
button affordance vs. chip affordance ambiguity (right now the
chip is the edit affordance and the button is the create
affordance; users may expect the same control to do both).

---

## Hard rules (carry-over)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`.
- Bounds checking on every IPC string input via
  `check_user_text` / `check_string`.
- Tauri commands return `CommandError`; never panic on user input.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  ts-rs-owned. Hand-mirroring is acceptable in a patch; never edit
  the generated files between patches.
- Runes-using files end in `.svelte.ts`.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Migrations: read the prior migration's comment block before
  writing the next. ADD COLUMN traps in DuckDB are real; fresh
  CREATE TABLE sidesteps them. (Session 19's migration 0009 is a
  fresh table.)
- The xAI API key is never echoed, logged, or printed.
- **New for Session 19:** when a class of failure has been
  iterated on across multiple sessions and the prompt has hit
  diminishing returns, the right move is to defer until real
  recurrence data is available. The recipe feedback channel exists
  precisely to make per-(plan, source) corrections cheap, so the
  prompt doesn't have to absorb every edge case globally.

---

## Continuity note

The continuity note from Session 18 still applies. The operator is
rigorous about security, prefers honesty about uncertainty over
false confidence, and reacts well to direct disagreement when
warranted. Stick to the plan; if you need to deviate, say so and
explain why.

One specific carry-over from Session 19: **the empirical-first
discipline ran into a wall** — verification of v1.7 needs network
access the assistant didn't have, so Session 19 shipped P2 ahead of
P1 with the operator's explicit go-ahead. Session 20 runs the
verification *against v1.8* (which adds an orthogonal channel
without changing v1.7's content); a v1.8 verification *is* a v1.7
verification for everything except the operator-feedback section,
and that section is verified separately the first time an operator
flags a recipe and re-runs fetch.

End of handoff.
