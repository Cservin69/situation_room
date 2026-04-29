<!--
  ExpectationRow — one line inside a Bucket.

  Shows a primary label (the metric name / event type / entity kind /
  relation kind / source description) and an expandable rationale.
  The rationale is collapsed by default to keep information density
  high (ADR 0006); clicking the row toggles it.

  Optional `aside` slot renders to the right of the label — used for
  metric unit hints, exemplar counts, preferred-source-id badges.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  interface Props {
    label: string;
    rationale: string;
    aside?: Snippet;
  }
  let { label, rationale, aside }: Props = $props();
  let expanded = $state(false);
</script>

<div class="row" class:expanded>
  <button class="head" type="button" onclick={() => (expanded = !expanded)} aria-expanded={expanded}>
    <span class="label">{label}</span>
    {#if aside}<span class="aside">{@render aside()}</span>{/if}
    <span class="caret" aria-hidden="true">{expanded ? '−' : '+'}</span>
  </button>
  {#if expanded}
    <p class="rationale">{rationale}</p>
  {/if}
</div>

<style>
  .row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .head {
    /* It's a button to be keyboard-accessible, but renders as a row. */
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    padding: 4px 6px;
    background: transparent;
    border: 0;
    text-align: left;
    cursor: pointer;
    color: var(--fg-primary);
    font-family: var(--font-sans);
    font-size: 12px;
    border-radius: 2px;
    transition: background var(--duration-ui) var(--ease);
  }
  .head:hover {
    background: var(--bg-panel-alt);
  }
  .head:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .label {
    font-family: var(--font-mono);
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .aside {
    flex: 0 0 auto;
    display: inline-flex;
    gap: 4px;
    color: var(--fg-tertiary);
    font-size: 11px;
    font-family: var(--font-mono);
  }
  .caret {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
    width: 10px;
    text-align: center;
  }
  .rationale {
    margin: 0;
    padding: 0 6px 6px 6px;
    color: var(--fg-secondary);
    font-size: 11px;
    line-height: 1.5;
  }
</style>
