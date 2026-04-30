<!--
  SatisfactionPanel — plan ↔ record satisfaction view (Session 14, P2).

  Shows how many records of each type have landed in storage for the
  selected plan. The join is topic-based: any record tagged with any
  of the plan's topic tags is counted.

  This is the coarse satisfaction signal described in ADR 0012:
  "did *anything* land for this expectation?" not "did the *right*
  thing land?" — the Class-D semantic gap (right type, wrong value)
  is deliberately deferred. What this panel closes is the headline
  blind spot: a user who runs a fetch and sees "1 record" in the
  FetchReport can now also see *which* bucket that record landed in,
  without opening DuckDB directly.

  ## Rendering contract

  - `null` counts  → loading shimmer (counts are fetched async).
  - all-zero counts AND plan has been fetched at least once → "no
    records yet" message with a hint. We check `hasRuns` to avoid
    showing this on a freshly-classified plan that hasn't run yet
    (in that case zero is expected, not diagnostic).
  - all-zero counts AND no runs → nothing rendered. The FetchReport
    component already tells the user no fetch has run; we don't need
    to duplicate that.
  - any non-zero → render the count row. Zero-count types still show
    their pill so the user can see the shape at a glance (three types
    tried, one landed).

  ## Why it lives above the buckets (in PlanReview.svelte)

  The bucket panels show what the *plan expects*. This panel shows
  what *actually landed*. They answer different questions; keeping
  them visually adjacent lets the user compare expectations vs.
  reality without scrolling.
-->
<script lang="ts">
  import type { RecordCountsDto } from '$lib/api/types/RecordCountsDto';

  interface Props {
    counts: RecordCountsDto | null;
    /** True when at least one fetch run has completed for this plan. */
    hasRuns: boolean;
  }
  let { counts, hasRuns }: Props = $props();

  /** The six types in display order — matches the bucket grid order. */
  const TYPES: Array<{ key: keyof Omit<RecordCountsDto, 'total'>; label: string }> = [
    { key: 'observations', label: 'obs' },
    { key: 'events',       label: 'evt' },
    { key: 'entities',     label: 'ent' },
    { key: 'relations',    label: 'rel' },
    { key: 'documents',    label: 'doc' },
    { key: 'assertions',   label: 'asr' },
  ];

  /** True if we have counts and at least one record landed. */
  const anyLanded = $derived(counts !== null && counts.total > 0);
  /** True if we have counts, all zeros, and a fetch has run. */
  const zeroAfterFetch = $derived(counts !== null && counts.total === 0 && hasRuns);
</script>

{#if counts === null}
  <!-- Loading: counts are fetched async; show a subtle shimmer row
       so the panel doesn't snap in. Only shown while a plan is
       selected and the request is in flight. -->
  <section class="satisfaction satisfaction--loading" aria-busy="true">
    <span class="label">records</span>
    <span class="shimmer" aria-hidden="true"></span>
  </section>
{:else if anyLanded}
  <section class="satisfaction" aria-label="Record satisfaction">
    <span class="label">records landed</span>
    <div class="pills">
      {#each TYPES as t (t.key)}
        {@const n = counts[t.key]}
        <span
          class="pill"
          class:pill--positive={n > 0}
          class:pill--zero={n === 0}
          title="{t.key}: {n}"
        >
          <span class="pill-label">{t.label}</span>
          <span class="pill-count">{n}</span>
        </span>
      {/each}
      <span class="total" title="total records across all types">
        {counts.total} total
      </span>
    </div>
  </section>
{:else if zeroAfterFetch}
  <!-- Fetch ran, but nothing landed. This is the Class-D diagnostic
       signal: the plan expected records but extraction produced zero.
       Surface it clearly so the user knows to check the fetch report. -->
  <section class="satisfaction satisfaction--empty">
    <span class="label">records landed</span>
    <span class="empty-hint">
      0 — check the fetch report for recipe failures
    </span>
  </section>
{/if}
<!-- If counts.total === 0 and no runs: render nothing. FetchReport
     already surfaces the "no fetch yet" state. -->

<style>
  .satisfaction {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    font-size: 11px;
  }

  .satisfaction--loading {
    min-height: 32px;
  }

  .label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    flex: 0 0 auto;
  }

  /* Loading shimmer */
  .shimmer {
    flex: 1;
    height: 10px;
    border-radius: 2px;
    background: linear-gradient(
      90deg,
      var(--bg-panel-alt) 25%,
      var(--border-subtle) 50%,
      var(--bg-panel-alt) 75%
    );
    background-size: 200% 100%;
    animation: shimmer var(--duration-ambient) ease infinite;
  }

  @keyframes shimmer {
    0%   { background-position: 200% 0; }
    100% { background-position: -200% 0; }
  }

  /* Pills row */
  .pills {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .pill {
    display: inline-flex;
    align-items: baseline;
    gap: 3px;
    padding: 2px 6px;
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 10px;
    border: 1px solid transparent;
    transition: border-color var(--duration-ui) var(--ease);
  }

  .pill--positive {
    background: rgba(91, 198, 133, 0.1);
    border-color: rgba(91, 198, 133, 0.3);
    color: var(--signal-positive);
  }

  .pill--zero {
    background: transparent;
    border-color: var(--border-subtle);
    color: var(--fg-quaternary);
  }

  .pill-label {
    text-transform: uppercase;
    letter-spacing: 0.04em;
    opacity: 0.8;
  }

  .pill-count {
    font-variant-numeric: tabular-nums;
  }

  .total {
    margin-left: 4px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-secondary);
    border-left: 1px solid var(--border-subtle);
    padding-left: 8px;
  }

  /* Zero-after-fetch variant */
  .satisfaction--empty .empty-hint {
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
    font-size: 11px;
  }
</style>
