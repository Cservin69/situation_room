<!--
  RecipeFlagDialog — modal for attaching an operator-feedback note
  to a (plan, source) pair. ADR 0013.

  Opens when the user clicks the flag button on a recipe card in the
  inspection panel. Captures a free-text note explaining what was
  wrong with the recipe; the note is sent to the backend alongside
  the `set_recipe_feedback` command and persisted per (plan, source).
  The next time recipe-authoring runs for the same pair (manually
  via `runFetch`, since automated re-author is gated by ADR 0012),
  the note feeds back into the LLM prompt as `{{RECIPE_FEEDBACK}}`.

  ## Why a dialog, not an inline textarea

  Same rationale as RejectDialog: the recipe card already carries an
  expanded extraction block, a produces block, and (for baked
  recipes) a payload preview. Inlining a textarea would push the
  remaining recipes off-screen and dwarf the card's native rhythm.
  A dialog gives the note a deliberate-feeling moment of attention
  matching the architectural intent — the note is the signal the
  next authoring run consumes, not a throwaway.

  ## Editing vs. fresh

  When the recipe already carries a note (the indicator chip is
  showing), the dialog opens pre-filled with the stored text via
  the `initial` prop. Submitting an empty / whitespace-only string
  clears the note (the backend collapses empty → clear, mirroring
  `reject_plan`'s `reason: Option<String>` shape). That keeps "edit"
  and "remove" as one action shape rather than two.

  ## Validation policy (ADR 0013)

  Frontend bounds (length only, advisory) keep the Submit button
  disabled when the user types past the hard limit. Hard validation
  (control characters, zero-width chars, bidi overrides, NFC) lives
  in the backend's `check_user_text` validator against
  `Bounds::RECIPE_FEEDBACK` — the frontend can only catch length;
  bytes-level character-class checks belong on the trust boundary.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import type { AuthoredFromDto } from '$lib/api/types/AuthoredFromDto';

  interface Props {
    /**
     * Stable id of the source the flag applies to. Shown in the
     * dialog header so the operator confirms which recipe they're
     * flagging when multiple are visible.
     */
    sourceId: string;
    /**
     * Initial note value. When the user opens the dialog on an
     * already-flagged recipe to edit, the caller pre-fills with
     * the stored note; for a fresh flag this is the empty string.
     */
    initial?: string;
    /**
     * Authoring provenance of the recipe being flagged. ADR 0014.
     *
     * When `'stub_excerpt'`, the dialog renders an informational
     * hint banner above the textarea — context the operator
     * should see *before* typing a feedback note. The hint
     * suggests re-running fetch first if the source might be
     * reachable now, since a successful pre-fetch on the next
     * authoring run produces a recipe authored from real bytes
     * (which may obviate the operator's correction entirely).
     *
     * For `'fetched_bytes'` and `'unknown'` the dialog renders
     * no banner — `'fetched_bytes'` because the recipe is
     * grounded and the gap doesn't apply, `'unknown'` because
     * the legacy state is not actionable advice the operator
     * needs at the moment of flagging.
     *
     * Defaults to `'unknown'` so callers that haven't been
     * updated to thread the prop through still mount cleanly
     * (no banner, no behavior change). New callers should always
     * pass the recipe's actual `authored_from` value.
     */
    authoredFrom?: AuthoredFromDto;
    /**
     * True while the parent's submit handler is in flight. Drives
     * the Submit button's disabled state and the spinner.
     */
    submitting?: boolean;
    /**
     * Called with the (possibly empty) note when the user clicks
     * Submit. Empty / whitespace-only is the clear path — the
     * parent forwards it to `flagRecipe`, which routes through
     * `clearRecipeFeedback` automatically.
     */
    onSubmit: (note: string) => void;
    /** Called when the user dismisses the dialog without submitting. */
    onCancel: () => void;
  }
  let {
    sourceId,
    initial = '',
    authoredFrom = 'unknown',
    submitting = false,
    onSubmit,
    onCancel,
  }: Props = $props();

  // One-time init from the `initial` prop — same pattern as
  // RejectDialog. Each open mounts a fresh component instance.
  let note = $state(untrack(() => initial));

  let textareaEl: HTMLTextAreaElement | undefined = $state();
  $effect(() => {
    textareaEl?.focus();
  });

  // Soft cap mirrors `Bounds::RECIPE_FEEDBACK` (2 000) on the
  // backend; the soft warn threshold matches RejectDialog at 800
  // chars per the same UX rationale: the fence + nonce + "treat as
  // data" framing buys structural safety, but a sprawling note
  // dilutes the LLM's ability to act on the feedback.
  const SOFT_WARN_AT = 800;
  const HARD_LIMIT = 2_000;

  const charsTyped = $derived(note.length);
  const overSoftWarn = $derived(charsTyped > SOFT_WARN_AT);
  const overHardLimit = $derived(charsTyped > HARD_LIMIT);
  const willClear = $derived(note.trim().length === 0);
  // Session 21 P4 fix — derived, not eager-captured. The original
  // line `const wasNonEmpty = initial.trim().length > 0;` triggered
  // the Svelte 5 `state_referenced_locally` warning because
  // `initial` is a reactive `$props()` value and the eager read
  // captured only the snapshot at component-mount. In practice the
  // dialog is mounted fresh on each open (so the snapshot read
  // happened to be correct), but the warning is still load-bearing:
  // any future contributor changing the call site to keep the
  // dialog alive across opens would silently get a stale
  // `wasNonEmpty`. `$derived` is the correct shape and removes the
  // warning at the source.
  const wasNonEmpty = $derived(initial.trim().length > 0);
  // ADR 0014: the hint banner above the textarea renders iff the
  // flagged recipe was stub-authored. `fetched_bytes` and
  // `unknown` both render no banner — `fetched_bytes` because the
  // gap doesn't apply; `unknown` because legacy rows aren't
  // actionable in this dialog.
  const showStubHint = $derived(authoredFrom === 'stub_excerpt');

  function handleSubmit() {
    if (overHardLimit || submitting) return;
    onSubmit(note);
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onCancel();
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault();
      handleSubmit();
    }
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="flag-dialog-title"
  tabindex="-1"
  onkeydown={handleKey}
>
  <div class="modal">
    <header>
      <h3 id="flag-dialog-title">
        {wasNonEmpty ? 'Edit recipe feedback' : 'Flag this recipe'}
      </h3>
      <p class="source" title={sourceId}>{sourceId}</p>
    </header>

    <section class="body">
      {#if showStubHint}
        <!--
          ADR 0014 hint banner — visible iff the recipe was
          stub-authored. Distinct from the freeform note field
          below: it's deliberate context the operator should see
          *before* typing. The banner does not gate submission.

          Copy is informational, not directive — the operator may
          still want to flag the recipe (e.g. "the stub guess
          happened to be wrong even where the source is reachable"
          is a legitimate flag). The hint just surfaces the
          alternative recovery path so the operator chooses
          deliberately rather than typing without knowing the
          recipe wasn't grounded in real bytes.
        -->
        <aside class="stub-hint" role="note">
          <span class="stub-hint-label">heads up</span>
          <p class="stub-hint-body">
            This recipe was authored without the source's actual
            response — the LLM saw a fallback description, not the
            real bytes. Flagging is still useful, but if the source
            is reachable now, running fetch again will let the next
            authoring run see real bytes and may obviate this
            correction entirely.
          </p>
        </aside>
      {/if}
      <label for="flag-note">
        What is wrong with this recipe?
        <span class="hint">
          Optional, but specific. The note feeds into the next
          authoring attempt for this source — "matched the channel
          &lt;title&gt;, not the article titles" is more useful
          than "wrong". Submit empty to remove an existing note.
        </span>
      </label>
      <textarea
        id="flag-note"
        bind:this={textareaEl}
        bind:value={note}
        rows="6"
        maxlength={HARD_LIMIT + 200}
        placeholder="What did the LLM get wrong?"
        disabled={submitting}
      ></textarea>
      <div class="counter" class:warn={overSoftWarn} class:over={overHardLimit}>
        {charsTyped} / {HARD_LIMIT}
        {#if overSoftWarn && !overHardLimit}
          — long notes dilute author focus
        {/if}
        {#if overHardLimit}
          — too long; the backend will refuse this note
        {/if}
        {#if willClear && wasNonEmpty}
          — empty: will remove the existing note
        {/if}
      </div>
    </section>

    <footer>
      <button type="button" class="btn-secondary" disabled={submitting} onclick={onCancel}>
        cancel
      </button>
      <button
        type="button"
        class="btn-primary"
        disabled={overHardLimit || submitting}
        onclick={handleSubmit}
      >
        {#if submitting}
          saving…
        {:else if willClear && wasNonEmpty}
          clear note
        {:else if wasNonEmpty}
          update
        {:else}
          flag
        {/if}
      </button>
    </footer>
  </div>
</div>

<style>
  /* Layout and visuals mirror RejectDialog so two dialogs in the same
     app feel native and consistent. The semantic hue is different:
     RejectDialog uses --signal-warning (rejecting a plan is destructive-
     adjacent); flagging a recipe is informational, so we use
     --signal-info for the primary action.

     Per ADR 0006: color is meaning, not decoration. Flag = info chip
     (the recipe is annotated, not discarded); reject plan = warning
     button (the plan is being moved out of the active set). */

  .backdrop {
    position: fixed;
    inset: 0;
    background: var(--bg-overlay);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal {
    background: var(--bg-panel);
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    width: min(560px, 92vw);
    max-height: 88vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.6);
  }

  header {
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--border-subtle);
  }
  header h3 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--fg-primary);
  }
  header .source {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--fg-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
  }

  .body {
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .body label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .hint {
    text-transform: none;
    letter-spacing: normal;
    font-size: 12px;
    color: var(--fg-tertiary);
    line-height: 1.5;
  }

  /*
   * ADR 0014 stub-hint banner. Sits above the textarea label;
   * a left border + warning hue mark it as "structural context"
   * rather than the chrome label/hint/counter triad. Same
   * `--signal-warning` color the chip uses, for visual rhyme:
   * the chip on the recipe card and the banner here are
   * announcing the same fact in two surfaces.
   *
   * `align-items: baseline` aligns the inline "heads up" tag
   * with the first line of the body paragraph, even when the
   * paragraph wraps over multiple lines.
   */
  .stub-hint {
    display: flex;
    gap: 10px;
    align-items: baseline;
    padding: 10px 12px;
    background: var(--bg-inset);
    border-left: 3px solid var(--signal-warning, var(--border-accent));
    border-radius: 3px;
    margin-bottom: 4px;
  }
  .stub-hint-label {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--signal-warning, var(--fg-secondary));
  }
  .stub-hint-body {
    margin: 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-secondary);
  }

  textarea {
    background: var(--bg-inset);
    color: var(--fg-primary);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    font-family: var(--font-sans);
    font-size: 13px;
    padding: 10px 12px;
    resize: vertical;
    min-height: 96px;
    transition: border-color var(--duration-ui) var(--ease);
  }
  textarea:focus {
    outline: none;
    border-color: var(--border-accent);
  }
  textarea:disabled {
    opacity: 0.6;
  }

  .counter {
    align-self: flex-end;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
  }
  .counter.warn {
    color: var(--signal-warning);
  }
  .counter.over {
    color: var(--signal-negative);
  }

  footer {
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border-subtle);
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .btn-primary,
  .btn-secondary {
    font-family: var(--font-mono);
    font-size: 11px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 6px 14px;
    border-radius: 3px;
    cursor: pointer;
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease);
  }
  .btn-primary {
    /* --signal-info: the flag is informational, not destructive.
       The recipe stays in the inspection panel; a note is attached. */
    background: var(--signal-info);
    color: var(--fg-inverse);
    border: 1px solid var(--signal-info);
  }
  .btn-primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }
  .btn-primary:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .btn-secondary {
    background: transparent;
    color: var(--fg-secondary);
    border: 1px solid var(--border-strong);
  }
  .btn-secondary:hover:not(:disabled) {
    border-color: var(--border-accent);
    color: var(--fg-primary);
  }
</style>
