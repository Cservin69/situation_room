<!--
  RejectDialog — modal for rejecting a plan with an optional reason.

  Opens when the user clicks "reject" on a Pending plan. Captures a
  free-text note explaining why; the note is sent to the backend
  alongside the reject command and persisted on the plan row. On
  re-classification, the note is fed back to the classifier as
  fenced feedback (see `crates/pipeline/src/research_classifier.rs`).

  ## Why a dialog rather than an inline textarea

  The reject button on the review pane sits next to the accept
  button. Inlining a textarea between them would push the accept
  button down on every plan and dwarf the trust-paragraph layout.
  A dialog keeps the review pane's visual rhythm intact and gives
  the rejection note a deliberate-feeling moment of attention,
  matching the architectural intent: the rejection reason is the
  signal the next classification consumes, not a throwaway field.

  ## Validation policy (Session 15)

  Frontend bounds (length only, advisory) keep the Submit button
  disabled when the note is empty AND when the user has typed past
  the soft warning threshold. Hard validation (control characters,
  zero-width chars, bidi overrides, NFC) lives in the backend's
  `check_user_text` validator — the frontend can only catch length;
  bytes-level character-class checks belong on the trust boundary.

  An empty note is allowed (the user clicks Submit without typing):
  the backend records the rejection with NULL `rejection_reason`.
  The dialog's helper copy makes that explicit.
-->
<script lang="ts">
  import { untrack } from 'svelte';

  interface Props {
    /**
     * The plan's topic, shown in the dialog header so the user
     * remembers which plan they're rejecting if multiple windows
     * are open.
     */
    topic: string;
    /**
     * Initial note value. When the user opens the dialog on a
     * previously-rejected plan to edit the existing note, the
     * caller pre-fills with the stored reason; for a fresh
     * rejection this is the empty string.
     */
    initial?: string;
    /**
     * True while the parent's submit handler is in flight. Drives
     * the Submit button's disabled state and the spinner.
     */
    submitting?: boolean;
    /**
     * Called with the (possibly empty) note when the user clicks
     * Submit. The parent persists, then closes the dialog by
     * setting its own open state to false.
     */
    onSubmit: (reason: string) => void;
    /** Called when the user dismisses the dialog without submitting. */
    onCancel: () => void;
  }
  let {
    topic,
    initial = '',
    submitting = false,
    onSubmit,
    onCancel,
  }: Props = $props();

  // One-time init from the `initial` prop. This dialog is wrapped in
  // a parent `{#if open}` block, so each open mounts a fresh
  // component instance — meaning the prop's value at construction
  // time is what we want, not a live binding. `untrack` makes that
  // intentional choice explicit and silences Svelte's
  // `state_referenced_locally` warning.
  let note = $state(untrack(() => initial));

  // Bound to the textarea via `bind:this`. Used by the focus effect
  // below to give the textarea focus when the dialog mounts —
  // replacing the deprecated `autofocus` attribute, which Svelte's
  // a11y lint rightly flags because it can yank focus unexpectedly
  // for keyboard and screen-reader users. Inside an explicit modal
  // dialog, focusing the primary input is the right behaviour; doing
  // it via JS makes the intent inspectable and confines the side
  // effect to this component.
  let textareaEl: HTMLTextAreaElement | undefined = $state();
  $effect(() => {
    textareaEl?.focus();
  });

  // Soft cap mirrors `Bounds::REJECTION_REASON` (2,000) on the
  // backend, but we warn earlier (~800 chars) per the minority
  // report's UX recommendation: the prompt-injection nonce buys
  // structural safety, but a sprawling note dilutes the LLM's
  // ability to act on the feedback even when sanitized.
  const SOFT_WARN_AT = 800;
  const HARD_LIMIT = 2_000;

  const charsTyped = $derived(note.length);
  const overSoftWarn = $derived(charsTyped > SOFT_WARN_AT);
  const overHardLimit = $derived(charsTyped > HARD_LIMIT);

  function handleSubmit() {
    if (overHardLimit || submitting) return;
    onSubmit(note);
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onCancel();
    }
    // Cmd/Ctrl+Enter submits — common modal convention.
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
  aria-labelledby="reject-dialog-title"
  tabindex="-1"
  onkeydown={handleKey}
>
  <div class="modal">
    <header>
      <h3 id="reject-dialog-title">Reject this classification?</h3>
      <p class="topic" title={topic}>{topic}</p>
    </header>

    <section class="body">
      <label for="reject-reason">
        Why is this classification wrong?
        <span class="hint">
          Optional, but the note feeds back into the next attempt
          if you re-classify. Be specific about what was wrong —
          "you confused the EUDR's UDB with the AI Act's UDB" is
          more useful than "wrong topic".
        </span>
      </label>
      <textarea
        id="reject-reason"
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
          — long notes dilute classifier focus
        {/if}
        {#if overHardLimit}
          — too long; the backend will refuse this note
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
        {submitting ? 'rejecting…' : 'reject'}
      </button>
    </footer>
  </div>
</div>

<style>
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
  header .topic {
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
    background: var(--signal-warning);
    color: var(--fg-inverse);
    border: 1px solid var(--signal-warning);
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
