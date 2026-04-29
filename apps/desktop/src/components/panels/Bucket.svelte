<!--
  Bucket — a single record-type panel in the plan-review screen.

  One of six (Observation, Event, Entity, Relation, Document, Assertion).
  Each bucket shows:
    - the bucket name and a count
    - a list of expectation rows (one per metric / event type / entity
      kind / relation kind / source nomination)
    - rationales rendered inline at small size; empty buckets render the
      "(no expectations for this type — by design)" line per the handoff.

  Children compose via a snippet so the parent decides how each row
  renders (a metric row is different from an entity-kind row).
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  interface Props {
    title: string;
    count: number;
    /** Snippet that renders the rows. Receives no arguments. */
    children?: Snippet;
  }
  let { title, count, children }: Props = $props();
</script>

<section class="bucket">
  <header>
    <span class="title">{title}</span>
    <span class="count">{count}</span>
  </header>
  <div class="body">
    {#if count === 0}
      <p class="empty">(no expectations for this type — by design)</p>
    {:else if children}
      {@render children()}
    {/if}
  </div>
</section>

<style>
  .bucket {
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
    border-bottom: 1px solid var(--border-subtle);
    padding-bottom: 6px;
  }
  .count {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-tertiary);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }
  .empty {
    margin: 0;
    color: var(--fg-quaternary);
    font-style: italic;
    font-size: 11px;
  }
</style>
