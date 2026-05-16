<!--
  SamplesModal — full-list inspection modal for a KindCard group
  (Session 80).

  ## What this component answers

  Session 79's KindCard stacks up to 8 distinct sample lines per group
  (companies of kind `company`: rusal + alcoa + …; relations of kind
  `supplies_to`: the from→to pairs; etc.) and surfaces overflow as a
  single `+ N more` row. That row was inert: there was no way to see
  what was hidden once a panel grew past the cap. On a global supply-
  chain plan with 30 companies, the operator saw the first 8 and a
  number — useful at a glance, frustrating when they wanted to
  actually find a name in the long tail.

  This modal makes the `+ N more` row a click destination: clicking it
  opens a full-height list of every distinct sample in the group
  (deduped the same way the card body dedupes), so the operator can
  scroll through all 30 companies without expanding the card layout
  or opening a separate route.

  ## Why an in-app modal rather than expanding the card inline

  - Cards live in a CSS grid (`repeat(auto-fill, minmax(220px, 1fr))`);
    expanding one card to fit 30 names would either stretch the row it
    sits in or break the grid. A modal preserves the dashboard's
    reading order — close it and you're back where you were.
  - The Documents panel already uses `DocumentDrawer` for the same
    "click a card → see more" affordance. Re-using the modal posture
    keeps both feels consistent.

  ## What this component does NOT do

  - **No filtering / search.** A future session can wire a search
    input if 30-name lists become common; today the dedupe + scroll is
    enough.
  - **No drill-into-record.** The samples are strings, not records —
    clicking a name does nothing today. The dashboard's per-record
    inspection lives on `DocumentDrawer` (Documents) and on the
    Plan/Recipes panel (other types) instead.
-->
<script lang="ts">
  import { onMount } from 'svelte';

  interface Props {
    /** The kind label this modal is listing samples for. Shown in the
     * header so the operator remembers what they clicked into. */
    kind: string;
    /** Count of records sharing this kind. Mirrors KindCard's header. */
    count: number;
    /** Full sample list — deduped + filtered by the caller; this
     * component renders what it receives without re-deduping. */
    samples: string[];
    /** Close-the-drawer callback. Wired to backdrop click, Escape, and
     * the explicit close button. */
    onClose: () => void;
  }
  let { kind, count, samples, onClose }: Props = $props();

  // Escape closes from anywhere inside the modal.
  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onClose();
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-label={`samples for ${kind}`}
  tabindex={-1}
  onclick={onBackdropClick}
>
  <article class="drawer">
    <header class="head">
      <div class="head-main">
        <span class="kind">{kind}</span>
        <span class="count" title="{count} record{count === 1 ? '' : 's'}">×{count}</span>
        <span class="meta">{samples.length} distinct</span>
      </div>
      <button
        class="close"
        type="button"
        aria-label="close samples"
        onclick={onClose}
      >
        ×
      </button>
    </header>

    <div class="body-wrap">
      {#if samples.length === 0}
        <p class="empty">— no samples to show</p>
      {:else}
        <ul class="samples">
          {#each samples as s (s)}
            <li class="sample-line" title={s}>{s}</li>
          {/each}
        </ul>
      {/if}
    </div>
  </article>
</div>

<style>
  /* Backdrop + drawer styles mirror DocumentDrawer (Session 70) so the
     two modals feel like the same family. */
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }

  .drawer {
    display: flex;
    flex-direction: column;
    gap: 12px;
    width: min(560px, 100%);
    max-height: 80vh;
    background: var(--bg-panel);
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    padding: 16px 20px 20px;
    overflow: hidden;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding-bottom: 10px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-main {
    display: flex;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .kind {
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-primary);
  }
  .count {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .meta {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0;
  }
  .close {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
    border-radius: 3px;
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .close:hover,
  .close:focus-visible {
    background: var(--bg-elevated, var(--bg-panel));
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  .body-wrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
  }
  .samples {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-primary);
  }
  .sample-line {
    padding: 2px 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border-bottom: 1px dashed transparent;
  }
  .sample-line:hover {
    border-bottom-color: var(--border-subtle);
  }
  .empty {
    margin: 0;
    padding: 16px 0;
    font-size: 12px;
    font-style: italic;
    color: var(--fg-tertiary);
    text-align: center;
  }
</style>
