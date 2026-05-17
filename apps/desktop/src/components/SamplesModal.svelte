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

  - **No drill-into-record.** The samples are strings, not records —
    clicking a name does nothing today. The dashboard's per-record
    inspection lives on `DocumentDrawer` (Documents) and on the
    Plan/Recipes panel (other types) instead.

  ## What this component DOES (Session 91)

  - **Substring filter.** Icon-first search affordance in the
    modal's header strip that expands into a text input on focus.
    Case-insensitive `contains` against each sample line.
    "Showing M of N" caption sits next to the existing "N distinct"
    when a filter is active. Sister surface: DocumentTable's filter
    uses the same `$lib/dashboard/text_filter.ts::matchesQuery`
    predicate — drift between the two would surprise operators
    using both in one session.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { matchesQuery } from '$lib/dashboard/text_filter';

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

  // -- filter state (Session 91) -----------------------------------
  //
  // Mirrors DocumentTable's pattern: icon-first when collapsed,
  // expanded input when focused or non-empty. Auto-collapses on
  // blur when empty.
  let filterQuery = $state('');
  let filterExpanded = $state(false);
  // `$state` wrap so `bind:this` writes are picked up by the
  // reactivity system — see DocumentTable.svelte for the rationale.
  let filterInput = $state<HTMLInputElement | undefined>(undefined);

  function onFilterIconClick() {
    filterExpanded = true;
    queueMicrotask(() => filterInput?.focus());
  }
  function onFilterBlur() {
    if (filterQuery.trim().length === 0) {
      filterExpanded = false;
    }
  }
  function onFilterClear() {
    filterQuery = '';
    filterExpanded = false;
  }

  let filteredSamples = $derived.by(() => {
    const q = filterQuery.trim();
    if (q.length === 0) return samples;
    return samples.filter((s) => matchesQuery(s, q));
  });
  let filterIsActive = $derived(filterQuery.trim().length > 0);

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
        {#if filterIsActive}
          <span class="meta filter-caption">
            · showing {filteredSamples.length} of {samples.length}
          </span>
        {/if}
      </div>
      <div class="head-actions">
        <!-- Session 91 filter — same icon-first → expanded posture as
             DocumentTable. The "×" close stays as the explicit
             exit; the filter clear-button (when present) uses a
             smaller chevron-style "×" so the two don't compete. -->
        <div class="filter" class:expanded={filterExpanded}>
          {#if filterExpanded || filterIsActive}
            <input
              bind:this={filterInput}
              bind:value={filterQuery}
              class="filter-input"
              type="text"
              placeholder="filter samples…"
              aria-label="filter samples"
              onblur={onFilterBlur}
            />
            {#if filterIsActive}
              <button
                class="filter-clear"
                type="button"
                aria-label="clear filter"
                onclick={onFilterClear}
                title="clear filter"
              >
                ×
              </button>
            {/if}
          {:else}
            <button
              type="button"
              class="filter-icon"
              onclick={onFilterIconClick}
              aria-label="filter samples"
              title="filter samples"
            >
              ⌕
            </button>
          {/if}
        </div>
        <button
          class="close"
          type="button"
          aria-label="close samples"
          onclick={onClose}
        >
          ×
        </button>
      </div>
    </header>

    <div class="body-wrap">
      {#if samples.length === 0}
        <p class="empty">— no samples to show</p>
      {:else if filteredSamples.length === 0}
        <p class="empty">— no samples match the filter</p>
      {:else}
        <ul class="samples">
          {#each filteredSamples as s (s)}
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
  .head-actions {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex: 0 0 auto;
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

  /* Session 91 — filter affordance. Sibling to DocumentTable's
     pattern: icon-only when collapsed, inline text input when
     focused or active. Sized for the modal header strip; smaller
     than the close button so the close stays the dominant exit. */
  .filter {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .filter-icon {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1;
    padding: 4px 8px;
    cursor: pointer;
  }
  .filter-icon:hover,
  .filter-icon:focus-visible {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .filter-input {
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 3px 6px;
    width: 160px;
    outline: none;
  }
  .filter-input:focus {
    border-color: var(--border-strong);
  }
  .filter-clear {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1;
    padding: 2px 6px;
    cursor: pointer;
  }
  .filter-clear:hover,
  .filter-clear:focus-visible {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .filter-caption {
    color: var(--fg-tertiary);
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
