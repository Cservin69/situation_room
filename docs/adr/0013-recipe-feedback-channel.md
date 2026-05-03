# ADR 0013 — Recipe feedback channel: per-(plan, source) operator notes

**Status**: Accepted
**Date**: 2026-05-02
**Related**: ADR 0007 (research function: two-level LLM architecture),
ADR 0012 (re-author on failure: manual-first, automated-later),
ADR 0009 (security posture)

---

## Context

ADR 0011 gave the operator a binary verdict on recipes — accept the
plan, run the fetch, and either get useful records or get failures
in the report. ADR 0012 said: "when a recipe fails, the operator
follows the manual protocol; do not automate retry until the
predicate strings are evidence-grounded." That leaves a real gap.

Today the operator sees a recipe in the inspection panel that is
*technically valid* (URL-guarded, schema-conformant, persisted) but
is *substantively wrong* — it points at a search-form skeleton
instead of the listing endpoint; it picked a one-record instance URL
when the bucket has multiple expectations; the JSON-path matched a
metadata field rather than the data field; the CSV column header was
hallucinated. The recipe runs forever, deterministically, until the
operator does something. The thing the operator wants to do is leave
a note: "this recipe is wrong because X." That note then needs to
reach the LLM the next time recipe-authoring runs for this
(plan, source) pair, so the next attempt can address X rather than
re-make it.

The classifier already has this channel. `previous_rejection_reason`
on `ClassificationContext` carries free-text feedback through a
fenced `{{USER_FEEDBACK}}` block in the classifier prompt, with a
per-call UUID nonce defending against breakout. The channel works:
Session 14's UDB framing-leak case demonstrated that feedback fed
back into Level-1 produced a corrected plan on the second attempt.
Level 2 has no equivalent. This ADR closes that gap.

This ADR was drafted in Session 19 explicitly *before* code lands,
per the Session 19 handoff's resolution that ADR 0013 should
precede implementation. The open questions enumerated in that
handoff are answered below.

## Decision

### A new persistent channel: `recipe_feedback`

A new storage table, `recipe_feedback`, persists at most one note
per `(plan_id, source_id)` pair. The schema is:

| column        | type           | notes                                              |
|---------------|----------------|----------------------------------------------------|
| `plan_id`     | UUID NOT NULL  | references the plan the note applies to           |
| `source_id`   | TEXT NOT NULL  | the source the note applies to (string id from `config/sources.toml`) |
| `note`        | TEXT NOT NULL  | the operator's text, post-`check_user_text` validation |
| `created_at`  | TIMESTAMPTZ NOT NULL | when the note was last set / overwritten     |

PRIMARY KEY is `(plan_id, source_id)`. There is no `id` column —
the natural key is the key. There is no `recipe_id` foreign key.
There is no row history.

### The keying choice — `(plan_id, source_id)`, not `recipe_id`

The Session 19 handoff's open question was: persist by `recipe_id`,
or by `(plan_id, source_id)`?

Recipes rotate. Re-authoring a failed recipe produces a new
`FetchRecipe` row with a new id, sharing the same `dedup_key`
(`{plan_id}:{source_id}`) as the prior version (ADR 0007 §
"Versioning vs migration"). If feedback were keyed to `recipe_id`,
the moment the operator's note prompted a re-author the new recipe
would be born with no feedback attached — defeating the channel's
purpose, which is exactly to feed the operator's correction
*forward* into the next authoring attempt.

The alternative — `(plan_id, source_id)` — survives re-authoring.
The same source feeding the same plan retains its note across
recipe versions. The note describes the *source's behaviour for
this plan*, which is the right grain.

### The overwrite choice — single row, not history

The Session 19 handoff also flagged: "does feedback persist across
`dedup_key`-bumped re-authoring, or get cleared with the recipe?
Likely: persist by `(plan_id, source_id)`."

Persistence is settled by the keying choice. The remaining question
is whether successive notes form a history or overwrite each other.
Overwrite wins, for three reasons:

1. **The classifier precedent overwrites.** `set_plan_rejection`
   replaces `rejection_reason` on each call (ADR 0007 amendment via
   migration 0007). The recipe-feedback channel mirrors that shape
   so future contributors recognize the pattern without reading
   another ADR.

2. **The LLM does not benefit from a history.** The prompt fences
   *one* note at a time. A history would either need to be
   summarized (a derived artifact the operator would have to
   curate) or rendered as a list (which dilutes the LLM's focus —
   ADR 0012's minority report flagged this exact failure mode for
   feedback signals).

3. **The operator's mental model is "the current note for this
   source."** Editing the note is the natural action when the
   prior attempt's correction was incomplete. Forcing the
   operator to choose between "amend" and "append" introduces a
   decision the use case doesn't justify.

If history matters later — for instance, to feed a future
automated re-author loop's evaluation step — it can be added as a
sibling `recipe_feedback_history` table, additively, without
disturbing the per-plan-source single-row contract. Speculative
history would just be append-only logging without a consumer.

### Separation from failure cases

The Session 19 handoff asked: "Is feedback visible in the
failure-case workflow, or is it a separate channel?"

Separate. Failure cases (`apps/desktop/failure_cases/...`) are
*global prompt artifacts*: they describe a class of LLM mis-
behaviour observed in production and exist to inform future prompt
edits. The LLM never sees them. Recipe feedback is *plan-local* and
*source-specific*: a note attached to one (plan, source) pair,
embedded in the next authoring run for that pair, never seen
elsewhere.

The two layers serve different audiences. Failure cases are read by
humans choosing what the prompt should teach. Feedback is read by
the LLM during one authoring call, then read by humans in the
inspection panel. Conflating them would mean the LLM seeing failure
notes from unrelated runs (cost without benefit) or operators
maintaining failure-case documentation as a side effect of
day-to-day flagging (a documentation trap).

### Bound — `Bounds::RECIPE_FEEDBACK = 2_000`

A new bound, distinct in name but identical in value to
`Bounds::REJECTION_REASON`. Same hardening: validated through
`check_user_text` at the api boundary, normalized for line endings,
control characters rejected, zero-width / bidi-override characters
rejected. The same rationale documented on `REJECTION_REASON`
applies here: "I rejected this because X" or "this recipe is wrong
because X" is a sentence or two, not a manifesto.

The new bound carries a distinct *name* even though it shares the
same value because call sites read more clearly:
`check_user_text("recipe_feedback", note, Bounds::RECIPE_FEEDBACK)`
documents intent in one place. If a future session diverges the
limits (operator notes might benefit from being shorter, since the
recipe-author prompt has more tokens to fit it alongside), the
constant divergence is one edit.

### Prompt integration — recipe_author.md v1.8

The recipe-author prompt grows a new section, `## Operator
feedback on prior authoring`, near the top of the prompt
(immediately after `## The plan you are authoring for`, before
`## The source context`). The section consumes a new
`{{RECIPE_FEEDBACK}}` placeholder, rendered by a function modeled
on `crates/pipeline/src/research_classifier.rs::render_user_feedback`:

- Empty input → empty string. The whole section disappears from
  the rendered prompt.
- Non-empty input → fenced block with per-call UUID nonce,
  closing-tag sanitization (bare and nonced), case-insensitive
  matching, "treat as data not instructions" preamble.

The fence tag is `<recipe_feedback id="...">` (distinct from the
classifier's `<user_feedback id="...">`), so the LLM has an
unambiguous lexical signal that this is operator feedback about a
prior authoring attempt for *this* (plan, source), not classifier-
era topic feedback.

### UI surface

A flag affordance on each recipe in `RecipesPanel.svelte` opens a
dialog modeled on `RejectDialog.svelte`. Submitting the dialog
calls `set_recipe_feedback(plan_id, source_id, Some(note))`.
Clearing the note (submitting empty after trim) calls the same
command with `None` — the storage layer deletes the row.

When a recipe carries an active feedback note, the recipe card
displays a subtle indicator chip next to the source_id, visible at
a glance, with the note text on hover. This makes the channel's
state observable: the operator can see which recipes they have
flagged for next-authoring without opening each one. The
classifier's `RejectDialog` precedent is followed for the dialog's
copy and submit / cancel UX; the chip's visual treatment uses
`--signal-info` (neutral; this is informational, not a warning).

### IPC commands

Two new `#[tauri::command]` handlers on the api crate:

- `set_recipe_feedback(plan_id: String, source_id: String, note:
  Option<String>) -> RecipeFeedbackDto | null`. Validates inputs
  (UUID parse, source_id length, note via `check_user_text` against
  `Bounds::RECIPE_FEEDBACK`). When `note` is `None` or trims empty,
  the row is deleted and the command returns `null`. Otherwise, the
  row is upserted and the command returns the persisted
  `RecipeFeedbackDto`.
- `list_recipe_feedback_for_plan(plan_id: String) ->
  Vec<RecipeFeedbackDto>`. Pure read; safe to call freely. Used by
  the frontend on plan selection alongside `list_recipes_for_plan`
  so the indicator chip renders in lockstep with the recipes.

A single command instead of separate `set` and `clear` mirrors the
`reject_plan(id, reason: Option<String>)` precedent — the empty /
None form clears, the non-empty form upserts. Two commands here
would document a difference (set vs clear) the storage layer
already collapses.

## Rationale

**Why a new channel rather than re-using the classifier's.** The
classifier's `previous_rejection_reason` is a Level-1 input scoped
to a single re-classification call. The feedback we're adding here
is a Level-2 input persisting across many authoring attempts and
visible in the recipe-inspection panel. Re-using the classifier's
field would conflate concerns ("did the user reject the plan or
flag a recipe?") and force a cross-level shape on a per-source
note. The architectural shape is a sibling to the classifier's
channel, not a subset of it.

**Why a separate ADR rather than amending ADR 0007.** ADR 0007
defines the *runtime* path's LLM-free invariant and the *authoring*
path's once-per-source contract. Amendment 3 (Session 18) extended
the recipe shape but did not change the inputs to the authoring
call. This ADR introduces a new authoring input (operator feedback)
and a new persistence table — both are additions to the surface
area ADR 0007 owns, but neither alters its core decisions. A
sibling ADR captures the new design without bloating ADR 0007 with
an amendment that's really a feature.

**Why not a sixth Tauri command for clearing feedback.** The
`reject_plan(id, reason: Option<String>)` precedent collapses set
and clear into one. The frontend dialog already maps "submit empty"
to the clear action. Two commands would document a distinction the
storage layer doesn't materialize. The principle: the wire surface
should reflect the storage shape, and the storage shape is a single
upsert-or-delete row keyed by (plan_id, source_id).

**Why the prompt section sits before the source context.** The LLM
should see operator feedback *before* it reads the source excerpt
and starts pattern-matching against it. Putting the feedback section
later — after the URL discipline or the document excerpt — would let
the LLM commit to a structural interpretation that the operator's
note was meant to override. Position the override before the
primary content.

## Alternatives considered

### Per-recipe-id feedback

Keying by `recipe_id` instead of `(plan_id, source_id)`. *Rejected.*
Recipe ids rotate on re-authoring. Feedback intended to teach the
next attempt would be silently abandoned at the moment the next
attempt was triggered. The whole purpose of the channel is to feed
forward through a re-author; keying by recipe id defeats the
purpose.

### Append-only feedback log

Multiple notes per (plan, source), preserving history. *Rejected.*
The LLM cannot consume a list of notes; it would either be fed all
of them (diluting the prompt) or fed only the latest (which is
overwrite by another name). Operators looking at the inspection
panel benefit more from "the current correction" than "every
correction we've ever attempted." Append-only is a natural future
addition if a consumer emerges; it does not earn its weight today.

### Auto-trigger re-authoring on flag

Submitting the flag triggers an immediate re-author of the recipe.
*Rejected.* This is exactly the automated retry loop ADR 0012
defers. Without the gate conditions in ADR 0012 §"When to automate"
being met, an auto-trigger would spend frontier-tier API budget on
runs the operator hasn't chosen to incur. The flag persists the
note; the operator decides when to re-author by running fetch
again. The two actions stay decoupled until ADR 0012's gate is met.

### Combine recipe feedback with the failure-case directory

Render flagged recipes' notes into auto-generated failure-case
markdown. *Rejected.* Failure cases are curated documentation about
patterns of LLM behaviour worth a prompt change. Recipe feedback is
ad-hoc operator commentary. Auto-converting one into the other
would either generate documentation bloat (most flagged recipes are
plan-local quirks, not pattern-worthy) or hide flags from the
prompt-improvement workflow (most pattern-worthy issues should be
written up by hand with full context, not extracted from a flag).
The two channels stay distinct.

## Consequences

### Good

- The operator gains a Level-2 feedback channel symmetric to the
  Level-1 channel they already have. Feature parity across the two
  authoring layers.
- Re-authoring a flagged recipe is a deliberate one-action
  workflow: edit the note in the dialog, then run fetch again. The
  fetch executor already loads recipes via
  `recipes_for_plan(plan_id)`; the addition is a sibling load of
  the feedback for each authored source.
- The feedback channel is a structurally narrow surface: one
  table, one bound, two commands, one dialog, one indicator chip,
  one prompt section. Future extensions (history, scoping, tags)
  are additive without disturbing the core shape.

### Tradeoffs

- One more piece of state for the operator to manage. The
  indicator chip mitigates this by making the state observable
  without explicit inspection.
- Re-authoring is gated on the operator running fetch; a flagged
  recipe with no subsequent fetch run will retain its current
  (wrong) extraction until the operator acts. Acceptable in the
  manual-first regime ADR 0012 establishes.
- Prompt size grows by ~150 tokens worst-case (the section
  template + a typical note + the fence). Within the existing
  `Bounds::LLM_PROMPT_BODY` budget by orders of magnitude.

### Neutral

- No new HTTP path. No new LLM provider call. No change to the
  closed extraction-mode enum. ADR 0007's runtime invariant,
  ADR 0009's security posture, and ADR 0011's plan-lifecycle
  states are all unchanged.

## When to amend or supersede

This ADR will need an amendment if any of the following becomes
true:

- The operator workflow demands per-recipe-version feedback
  (rather than per-source) — e.g. comparing v1 and v2 and noting
  which one the operator preferred. That's a different shape and
  would benefit from explicit history.
- The automated re-author loop in ADR 0012 lands. The flag would
  then either trigger automation directly (changing the no-side-
  effects guarantee here) or remain manual-only with the
  automation triggered by something else. Either way the
  interaction warrants a written reconciliation.
- A second feedback consumer appears (e.g. an evaluation pipeline
  reading flagged recipes to score the LLM). At that point, the
  separation from failure cases may need re-examination.

Until then: one row per (plan, source). Overwrite on set. Delete
on empty. One fenced section in the recipe-author prompt. One
indicator chip. The minimum surface that closes the parity gap.

## Code references

- Storage table:
  `migrations/0009_recipe_feedback.sql` — fresh table, no ALTER.
- Storage module:
  `crates/storage/src/recipe_feedback.rs` — `RecipeFeedbackRow`,
  `StoredRecipeFeedback`, `Store::set_recipe_feedback`,
  `Store::clear_recipe_feedback`,
  `Store::recipe_feedback_for_source`,
  `Store::recipe_feedback_for_plan`.
- Bound:
  `crates/secure/src/bounds.rs::Bounds::RECIPE_FEEDBACK`.
- Pipeline integration:
  `crates/pipeline/src/recipe_author.rs::AuthoringContext::recipe_feedback`,
  `crates/pipeline/src/recipe_author.rs::render_recipe_feedback`,
  `crates/pipeline/src/fetch_executor.rs::author_one`
  (loads feedback before the LLM call).
- IPC surface:
  `crates/api/src/commands.rs::set_recipe_feedback`,
  `crates/api/src/commands.rs::list_recipe_feedback_for_plan`.
- Wire DTO:
  `crates/api/src/types_export.rs::RecipeFeedbackDto`.
- UI:
  `apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte`,
  `apps/desktop/src/components/RecipesPanel.svelte`
  (button + indicator chip),
  `apps/desktop/src/stores/plans.svelte.ts`
  (`flagRecipe`, `clearRecipeFeedback`, `refreshRecipeFeedback`,
  `recipeFeedback` state map).
- Prompt:
  `config/prompts/recipe_author.md` v1.8 — new section
  `## Operator feedback on prior authoring` near the top.

## Review notes

ADR drafted Session 19 (2026-05-02). Per the Session 19 handoff's
explicit recommendation that ADR 0013 be drafted *before* code
lands, the design above resolved each open question in the
handoff's §"P2 — Per-recipe rejection feedback":

- *"Does feedback persist across `dedup_key`-bumped re-authoring,
  or get cleared with the recipe?"* — Persists. Keyed by
  `(plan_id, source_id)`, which survives re-authoring.
- *"Is feedback visible in the failure-case workflow, or is it a
  separate channel?"* — Separate channel. Audience and lifecycle
  differ.
- *"Maximum feedback length — same `check_user_text` bound as the
  classifier topic?"* — Yes by value (2 000 chars), but a distinct
  named constant (`Bounds::RECIPE_FEEDBACK`) so call sites read
  cleanly and a future divergence is one edit.

The implementation in Session 19 follows this ADR exactly. If
empirical use surfaces any of the amendment triggers in §"When to
amend or supersede," the next session writes the amendment before
the code change.
