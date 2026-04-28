<!--
  RecentPlansList — left pane.

  Renders one row per recent plan with topic, created_at, the
  bucket-count summary, and a status pill. Click a row to open it in
  the review pane. Active row gets a strong-border treatment (chrome
  change, not color).

  The PlanFilterStrip sits at the top of the listing and drives which
  status bucket is visible.
-->
<script lang="ts">
  import { plans, selectPlan, formatCreatedAt } from '$stores/plans.svelte';
  import type { PlanSummary } from '$lib/api/types/PlanSummary';
  import PlanFilterStrip from '$components/PlanFilterStrip.svelte';
  import StatusPill from '$components/common/StatusPill.svelte';

  function summaryLine(p: PlanSummary): string {
    return `${p.observation_count} obs · ${p.event_count} ev · ${p.entity_count} ent · ${p.relation_count} rel · ${p.document_source_count} src`;
  }
</script>

<aside class="list" aria-label="recent research plans">
  <header>
    <span class="title">recent</span>
    <span class="count">{plans.recent.length}</span>
  </header>
  <PlanFilterStrip />
  {#if plans.recent.length === 0}
    <p class="empty">
      {#if plans.loading}
        loading…
      {:else if plans.statusFilter === 'all'}
        no plans yet — classify a topic above
      {:else}
        no {plans.statusFilter} plans
      {/if}
    </p>
  {:else}
    <ul>
      {#each plans.recent as p (p.id)}
        <li>
          <button
            type="button"
            class="row"
            class:active={plans.selected?.id === p.id}
            onclick={() => selectPlan(p.id)}
          >
            <span class="topic-line">
              <span class="topic">{p.topic}</span>
              <StatusPill status={p.status} />
            </span>
            <span class="meta">{formatCreatedAt(p.created_at)}</span>
            <span class="summary">{summaryLine(p)}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</aside>

<style>
  .list {
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    height: 100%;
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: 10px 12px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
    border-bottom: 1px solid var(--border-subtle);
  }
  .count {
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
  }
  .empty {
    margin: 12px;
    color: var(--fg-tertiary);
    font-size: 11px;
    font-style: italic;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1 1 auto;
  }
  li + li {
    border-top: 1px solid var(--border-subtle);
  }
  .row {
    width: 100%;
    background: transparent;
    border: 0;
    border-left: 2px solid transparent;
    text-align: left;
    cursor: pointer;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--fg-primary);
    transition: background var(--duration-ui) var(--ease), border-color var(--duration-ui) var(--ease);
  }
  .row:hover { background: var(--bg-panel-alt); }
  .row:focus-visible { outline: 1px solid var(--border-accent); outline-offset: -1px; }
  .row.active {
    background: var(--bg-panel-alt);
    border-left-color: var(--border-strong);
  }
  .topic-line {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }
  .topic {
    font-size: 12px;
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
  }
  .meta {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
  }
  .summary {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
  }
</style>
