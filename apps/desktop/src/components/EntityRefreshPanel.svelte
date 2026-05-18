<!--
  EntityRefreshPanel — per-Entity refresh-event timeline (Session 99 #4).

  ## What this answers

  Sibling to CostTimelinePanel. The cost panels show LLM accounting.
  This one shows the storage layer's tier-aware refresh activity:
  every in-place name/tier change Sn-98 #5's `upsert_entity_with_tier`
  fires lands here, so operators can see *what changed and when*
  without having to diff two `entities` snapshots or grep INFO logs.

  ## Layout

  One row per refresh event, newest-first. Each row:
    - left  — time (HH:MM:SS local)
    - mid   — entity_id (business key, monospace)
    - right — `prev → new` canonical name, tier badges underneath

  Tier-only refreshes (name unchanged, tier elevated — rare but
  legal when two pipelines independently land on the same display
  name) render with a `tier-only` hint instead of the `→` arrow so
  the operator's eye isn't drawn to a no-op-looking row.

  ## What this panel does NOT do

  - **No persistence.** Same as CostTimelinePanel: ring buffer is in
    binary memory; restart-clean. The operator-visible value is
    "what just happened" — that's the right scope.
  - **No dollar/cost language.** Refreshes are storage-layer
    mutations, not LLM calls; cost language would muddle the panel's
    distinct shape.
  - **No write actions.** Refreshes are produced by the executor's
    Entity pipelines; the panel is read-only.
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { entityRefreshLog } from '$lib/api/client';
  import type { EntityRefreshEventDto } from '$lib/api/types/EntityRefreshEventDto';
  import type { EntityProvenanceTierDto } from '$lib/api/types/EntityProvenanceTierDto';

  let entries = $state<EntityRefreshEventDto[]>([]);
  let lastUpdated = $state<Date | null>(null);
  let error = $state<string | null>(null);

  /**
   * 10s poll cadence. Matches CostTimelinePanel — refreshes are
   * produced by the executor's pipelines on the same kind of
   * timeline as LLM calls (one per Document, batched per fetch
   * run), so the same poll bound is appropriate.
   */
  const POLL_INTERVAL_MS = 10_000;
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      entries = await entityRefreshLog();
      lastUpdated = new Date();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    refresh();
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS);
  });

  onDestroy(() => {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  });

  // Newest-first: the Rust ring buffer surfaces oldest-first
  // (insertion order); the operator's eye lands on the freshest
  // refresh without scrolling.
  let displayEntries = $derived([...entries].reverse());

  function formatTime(iso: string): string {
    try {
      const d = new Date(iso);
      if (Number.isNaN(d.valueOf())) return '?';
      const hh = String(d.getHours()).padStart(2, '0');
      const mm = String(d.getMinutes()).padStart(2, '0');
      const ss = String(d.getSeconds()).padStart(2, '0');
      return `${hh}:${mm}:${ss}`;
    } catch {
      return '?';
    }
  }

  /**
   * Map the closed-vocab tier enum to a short label for the badge.
   * Stays in lockstep with the Rust EntityProvenanceTier ordering
   * comment (DocumentExtracted > SlugHumanised > RecipeIterator > Unknown).
   */
  function tierLabel(t: EntityProvenanceTierDto): string {
    switch (t) {
      case 'DocumentExtracted':
        return 'doc';
      case 'SlugHumanised':
        return 'slug';
      case 'RecipeIterator':
        return 'iter';
      case 'Unknown':
        return '?';
    }
  }
</script>

<section class="refresh-panel" aria-label="Entity refresh log">
  <header class="head">
    <span class="title">entity refreshes</span>
    <span class="meta">
      {#if lastUpdated}
        <span title="last updated">{formatTime(lastUpdated.toISOString())}</span>
      {/if}
      <span class="count">· {entries.length} refresh{entries.length === 1 ? '' : 'es'}</span>
    </span>
  </header>

  {#if error !== null}
    <p class="error" role="alert">entity refresh log failed to refresh: {error}</p>
  {/if}

  {#if entries.length === 0}
    <p class="empty">no Entity tier-elevating refreshes yet this session.</p>
  {:else}
    <ul class="rows">
      {#each displayEntries as e (e.at + e.entity_id)}
        <li class="row">
          <span class="ts" title={e.at}>{formatTime(e.at)}</span>
          <span class="bid" title={e.entity_id}>{e.entity_id}</span>
          {#if e.name_changed}
            <span class="change">
              <span class="prev" title="previous canonical_name">{e.previous_canonical_name}</span>
              <span class="arrow">→</span>
              <span class="next" title="new canonical_name">{e.new_canonical_name}</span>
            </span>
          {:else}
            <span class="change tier-only" title="tier elevated; canonical_name unchanged">
              {e.new_canonical_name}
              <span class="hint">(tier-only)</span>
            </span>
          {/if}
          <span class="tiers" title="tier elevation">
            <span class="prev-tier">{tierLabel(e.previous_tier)}</span>
            <span class="arrow">→</span>
            <span class="next-tier">{tierLabel(e.new_tier)}</span>
          </span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .refresh-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
  }
  .title {
    font-weight: 500;
  }
  .meta {
    font-family: var(--font-mono);
    color: var(--fg-quaternary);
    text-transform: none;
    letter-spacing: 0;
    font-size: 10px;
  }
  .count {
    margin-left: 4px;
  }
  .error {
    margin: 0;
    padding: 8px;
    font-size: 11px;
    color: var(--fg-tertiary);
    background: var(--bg-inset);
    border-radius: 3px;
  }
  .empty {
    margin: 0;
    padding: 12px;
    font-size: 11px;
    color: var(--fg-tertiary);
    background: var(--bg-inset);
    border: 1px dashed var(--border-subtle);
    border-radius: 3px;
    text-align: center;
  }
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 280px;
    overflow-y: auto;
  }
  .row {
    display: grid;
    grid-template-columns: 70px 18ch 1fr auto;
    gap: 10px;
    padding: 2px 6px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-primary);
    border-bottom: 1px solid var(--border-subtle);
  }
  .row:last-child {
    border-bottom: none;
  }
  .ts {
    color: var(--fg-secondary);
  }
  .bid {
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .change {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .change .prev {
    color: var(--fg-tertiary);
    text-decoration: line-through;
    text-decoration-color: var(--fg-quaternary);
  }
  .change .next {
    color: var(--fg-primary);
  }
  .change.tier-only {
    color: var(--fg-secondary);
  }
  .change .hint {
    color: var(--fg-quaternary);
    font-size: 10px;
  }
  .arrow {
    color: var(--fg-quaternary);
  }
  .tiers {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    color: var(--fg-tertiary);
    white-space: nowrap;
    font-size: 10px;
  }
  .tiers .prev-tier {
    color: var(--fg-quaternary);
  }
  .tiers .next-tier {
    color: var(--fg-secondary);
  }
</style>
