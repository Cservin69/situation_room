<!--
  ReauthorDialog — modal for triggering a manual re-author of a
  failed recipe. Track A, ADR 0012 amendment 1.

  Opens when the operator clicks the "re-author" button on a recipe
  card whose latest fetch outcome is `Failed @ apply`. Shows:

    - the prior recipe's source id and short id (so the operator
      confirms which recipe they're re-authoring),
    - the failure message verbatim (from the captured fetch attempt),
    - an excerpt of the fetched bytes the runtime saw (head 4 KiB,
      monospace, scrollable) — the same bytes the LLM will see at
      re-author time,
    - a textarea for the optional operator note (≤ 800 chars soft
      warn, ≤ 2 000 hard limit — same conventions as RejectDialog
      and RecipeFlagDialog),
    - Submit / Cancel buttons.

  ## Why a dialog, not an inline expansion

  Same rationale as RejectDialog and RecipeFlagDialog: the recipe
  card already carries an extraction block, a produces block, and
  for baked recipes a payload preview. Inlining a re-author panel
  would push the remaining recipes off-screen. A dialog gives the
  decision a deliberate-feeling moment of attention matching the
  architectural intent — the re-author spends real LLM budget; the
  operator should look at the failure context before triggering it.

  ## Validation policy

  Frontend bounds (length only, advisory) keep the Submit button
  disabled when the user types past the hard limit. Hard validation
  (control characters, zero-width chars, bidi overrides, NFC) lives
  in the backend's `check_user_text` validator against
  `Bounds::RECIPE_FEEDBACK` — the frontend can only catch length;
  bytes-level character-class checks belong on the trust boundary.

  ## What the operator sees in the bytes excerpt

  The capture in `crates/storage/src/recipe_fetch_attempts.rs` is
  truncated at MAX_EXCERPT_BYTES (64 KiB) on the way *into* storage.
  The dialog shows the head of that excerpt (we don't truncate
  again on render — the wire payload is already bounded). The
  byte-for-byte fidelity is the point: the operator and the LLM see
  the same bytes the runtime saw.
-->
<script lang="ts">
  import { untrack } from 'svelte';

  interface Props {
    /**
     * Stable id of the source the prior recipe targets. Shown in the
     * dialog header so the operator confirms which recipe they're
     * re-authoring when multiple are visible.
     */
    sourceId: string;
    /**
     * Short id of the prior recipe (first 8 chars of UUID). Shown
     * alongside the source id so the operator distinguishes versions
     * of the same source's recipes.
     */
    priorRecipeShortId: string;
    /**
     * The verbatim failure message from the prior recipe's last
     * fetch attempt (`recipe_fetch_attempts.failure_message`). The
     * dialog renders it as a structural panel — distinct from the
     * operator's diagnosis textarea below — because this is the
     * load-bearing evidence for re-authoring.
     */
    failureMessage: string;
    /**
     * Head of the bytes the runtime fetched at the failed apply.
     * Rendered as monospace + scrollable. Empty string → the dialog
     * shows an explanatory placeholder ("no bytes captured for this
     * recipe — re-authoring may guess at the response shape").
     */
    bytesExcerpt: string;
    /**
     * True while the parent's submit handler is in flight. Drives
     * the Submit button's disabled state and the spinner.
     */
    submitting?: boolean;
    /**
     * Called with the (possibly empty) note when the user clicks
     * Submit. Empty / whitespace-only is allowed: the failure message
     * alone may be rich enough.
     */
    onSubmit: (note: string | null) => void;
    /** Called when the user dismisses the dialog without submitting. */
    onCancel: () => void;
  }

  let {
    sourceId,
    priorRecipeShortId,
    failureMessage,
    bytesExcerpt,
    submitting = false,
    onSubmit,
    onCancel,
  }: Props = $props();

  // One-time init — same pattern as RejectDialog / RecipeFlagDialog.
  // Each open mounts a fresh component instance.
  let note = $state(untrack(() => ''));

  let textareaEl: HTMLTextAreaElement | undefined = $state();
  $effect(() => {
    textareaEl?.focus();
  });

  // Soft cap mirrors `Bounds::RECIPE_FEEDBACK` (2 000) on the
  // backend; the soft warn threshold matches RejectDialog at 800
  // chars per the same UX rationale.
  const SOFT_WARN_AT = 800;
  const HARD_LIMIT = 2_000;

  const charsTyped = $derived(note.length);
  const overSoftWarn = $derived(charsTyped > SOFT_WARN_AT);
  const overHardLimit = $derived(charsTyped > HARD_LIMIT);
  const hasBytes = $derived(bytesExcerpt.length > 0);

  /*
    Session 68 — diagnosis hints. Pattern-match common apply-failure
    predicates and surface a one-click suggestion that prefills the
    operator note.

    Each hint is keyed by a regex that matches the failure_message
    substring. The `note` is the prose the LLM will see if the
    operator clicks "use hint" — written in the diagnosis voice the
    re-author prompt is tuned for ("the recipe assumed X; the source
    actually emits Y; try Z").

    Patterns are intentionally conservative: only well-understood
    apply-failure shapes appear here. Unknown failure shapes get the
    blank textarea and the operator's own diagnosis. Hints are
    closed-vocabulary-safe — they pattern-match runtime failure
    predicate strings, never source-host strings.
  */
  type DiagnosisHint = {
    pattern: RegExp;
    /** Short label shown on the hint button. */
    label: string;
    /** The note prose injected into the textarea on click. */
    note: string;
  };
  const HINTS: DiagnosisHint[] = [
    {
      pattern: /matched \d+ elements; cap is \d+/i,
      label: 'cap-exceeded — narrow the iterator',
      note:
        "The iterator path matched more elements than the runtime cap. " +
        "Session 68's URL rewriter auto-caps OData-shaped URLs " +
        "($select/$filter/$orderby keys, or /api/open/vN/ paths) at fetch " +
        "time; if you're still seeing this, the URL didn't match those " +
        "shapes. Re-author the recipe with one of: (a) a JsonPath filter " +
        "like $.items[?(@.year==2025)]; (b) a more specific URL with a " +
        "narrower scope; (c) a CSS selector that targets distinct cards " +
        "rather than every link.",
    },
    {
      pattern: /matched no elements/i,
      label: 'css selector matched nothing — check shape',
      note:
        "The CSS iterator selector didn't fire on the fetched bytes. " +
        "Likely causes: the page is JS-rendered (no static HTML for the " +
        "selector to match — see ADR 0009 for browser-UA-only paths); " +
        "the markup changed shape since authoring; or the selector is " +
        "too specific (requires a class that's only present on some " +
        "states of the page). Re-author against the bytes excerpt below.",
    },
    {
      pattern: /matched no nodes|no row matched/i,
      label: 'json/csv path matched nothing — check shape',
      note:
        "The JsonPath / CSV row selector didn't fire. The response " +
        "shape may have changed (an extra wrapping object, a renamed " +
        "field, a non-array value where an array was expected) or the " +
        "path is wrong. Compare the path in the recipe against the " +
        "bytes excerpt below and re-author with the corrected shape.",
    },
    {
      pattern: /bytes did not parse as JSON/i,
      label: 'wrong content type — wrong extraction mode',
      note:
        "The recipe's extraction mode is JsonPath but the response " +
        "isn't JSON. Check the response's Content-Type chip on the " +
        "recipe row: if it's text/html or text/csv, re-author with the " +
        "matching extraction mode (CssSelect / CsvCell). If the bytes " +
        "are JSON-shaped but invalid, the source is broken — flag the " +
        "recipe instead.",
    },
  ];

  const matchedHints = $derived(
    HINTS.filter((h) => h.pattern.test(failureMessage))
  );

  function applyHint(h: DiagnosisHint) {
    note = h.note;
    textareaEl?.focus();
  }

  function handleSubmit() {
    if (overHardLimit || submitting) return;
    const trimmed = note.trim();
    onSubmit(trimmed.length === 0 ? null : trimmed);
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
  aria-labelledby="reauthor-dialog-title"
  tabindex="-1"
  onkeydown={handleKey}
>
  <div class="modal">
    <header>
      <h3 id="reauthor-dialog-title">Re-author this recipe</h3>
      <p class="prior" title="{sourceId} — recipe {priorRecipeShortId}">
        <span class="source-label">{sourceId}</span>
        <span class="prior-id">recipe {priorRecipeShortId}</span>
      </p>
    </header>

    <section class="body">
      <!--
        Failure-message panel — the load-bearing evidence the LLM
        will use. Distinct chrome (left border + warning hue) from
        the operator's note textarea below so the operator reads
        them as different kinds of input.
      -->
      <div class="failure-panel" role="note">
        <span class="failure-label">failure</span>
        <pre class="failure-message">{failureMessage}</pre>
      </div>

      <!--
        Diagnosis hints — Session 68. Pattern-matches the failure
        message against the small set of well-understood apply-failure
        predicates and offers a one-click prefill of the operator
        note. Surfaces only when at least one pattern hits; absent
        otherwise so unknown shapes get the blank-textarea + own-
        diagnosis path. The buttons set the textarea content, the
        operator can edit before submit.
      -->
      {#if matchedHints.length > 0}
        <div class="hints-panel" role="note">
          <span class="hints-label">diagnosis hints</span>
          <ul>
            {#each matchedHints as hint (hint.label)}
              <li>
                <button
                  type="button"
                  class="hint-btn"
                  title="Pre-fill the operator note with this diagnosis. Edit before submit."
                  onclick={() => applyHint(hint)}
                  disabled={submitting}
                >use: {hint.label}</button>
              </li>
            {/each}
          </ul>
        </div>
      {/if}

      <!--
        Bytes excerpt — the source's actual response at the failed
        apply. Monospace + scrollable. The empty-state copy below
        is honest: re-authoring without ground-truth bytes is
        guessing, and the operator should know that before
        spending an LLM call.
      -->
      <details class="bytes-panel" open>
        <summary>fetched bytes (what the runtime saw)</summary>
        {#if hasBytes}
          <pre class="bytes-excerpt">{bytesExcerpt}</pre>
        {:else}
          <p class="bytes-empty">
            No bytes captured for this recipe. Re-authoring will rely
            on the failure message alone — the LLM may guess at the
            response shape rather than match it. Run fetch again to
            capture fresh bytes if the source is reachable.
          </p>
        {/if}
      </details>

      <label for="reauthor-note">
        Optional operator note (your diagnosis)
        <span class="hint">
          Free-form. The LLM sees the failure above either way; this
          is for "the source emits unwrapped &lt;title&gt;, not
          CDATA-wrapped" or anything else specific you noticed.
          Cmd/Ctrl+Enter submits.
        </span>
      </label>
      <textarea
        id="reauthor-note"
        bind:this={textareaEl}
        bind:value={note}
        rows="5"
        maxlength={HARD_LIMIT + 200}
        placeholder="What did the LLM get wrong? (optional)"
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
          authoring…
        {:else}
          re-author
        {/if}
      </button>
    </footer>
  </div>
</div>

<style>
  /* Layout and visuals mirror RejectDialog and RecipeFlagDialog so
     the three dialogs in the same app feel native and consistent.
     The semantic hue is `--signal-warning` for the failure-message
     panel (warranted attention; the recipe is broken) and
     `--signal-info` for the primary action (re-authoring is
     constructive — we're trying to fix the recipe, not destroy it).
     Per ADR 0006: color is meaning, not decoration. */

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
    width: min(680px, 94vw);
    max-height: 90vh;
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
  header .prior {
    margin: 4px 0 0;
    display: flex;
    gap: 12px;
    font-size: 12px;
    font-family: var(--font-mono);
    color: var(--fg-secondary);
    overflow: hidden;
    white-space: nowrap;
  }
  header .source-label {
    color: var(--fg-primary);
    font-weight: 600;
  }
  header .prior-id {
    color: var(--fg-tertiary);
  }

  .body {
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    min-height: 0;
  }

  /* Failure panel — the load-bearing evidence chrome. Same
     border-left + warning hue as the stub-hint banner in
     RecipeFlagDialog, so the visual rhyme reads as "structural
     context the operator must absorb." */
  .failure-panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--bg-inset);
    border-left: 3px solid var(--signal-warning, var(--border-accent));
    border-radius: 3px;
  }
  .failure-label {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--signal-warning, var(--fg-secondary));
  }
  .failure-message {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.45;
    color: var(--fg-primary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* Diagnosis hints — Session 68. Sits between the failure panel
     (warning hue) and the bytes panel (canvas hue). Uses --signal-info
     for the border-left to read as "constructive suggestion, not an
     additional warning"; the action is "click to apply", not "read
     this carefully". */
  .hints-panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--bg-inset);
    border-left: 3px solid var(--signal-info, var(--border-accent));
    border-radius: 3px;
  }
  .hints-label {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--signal-info, var(--fg-secondary));
  }
  .hints-panel ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .hint-btn {
    background: transparent;
    border: 1px solid var(--border-subtle);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 4px 8px;
    border-radius: 3px;
    cursor: pointer;
    text-align: left;
  }
  .hint-btn:hover:not(:disabled) {
    background: var(--bg-canvas);
    border-color: var(--signal-info, var(--border-accent));
  }
  .hint-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Bytes excerpt — collapsible, scrollable, monospace. The
     `details`/`summary` shape mirrors the existing extraction /
     produces blocks on the recipe card so the dialog's interaction
     grammar is consistent with the rest of the inspection panel. */
  .bytes-panel {
    background: var(--bg-canvas);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 8px 12px;
  }
  .bytes-panel summary {
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .bytes-excerpt {
    margin: 8px 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-secondary);
    background: var(--bg-inset);
    padding: 8px;
    border-radius: 2px;
    max-height: 200px;
    overflow-y: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }
  .bytes-empty {
    margin: 8px 0 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-tertiary);
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
    min-height: 80px;
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
    /* --signal-info: re-authoring is constructive (try to fix the
       recipe), not destructive. Matches the flag-dialog primary's
       hue choice; the visual rhyme between the two re-author-adjacent
       actions is intentional. */
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
