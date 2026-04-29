<!--
  FetchReport — renders the result of a single `runFetch` call.

  Three sections:

    - Summary line: "X/Y recipes succeeded, Z records produced."
    - Outcomes list: one row per recipe, with per-stage iconography
      for failures.
    - History strip (below): the `plans.fetchRuns` array — at-a-
      glance "we ran this plan N times before, here's how each
      went." Empty until at least one run has been completed.

  The component reads from the runes store directly; no props. Mounts
  as a child of PlanReview when there's a plan selected.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { RecipeOutcomeDto } from '$lib/api/types/RecipeOutcomeDto';
  import type { FetchRunSummaryDto } from '$lib/api/types/FetchRunSummaryDto';

  function shortId(id: string): string {
    // UUIDv7s are too long for inline display; first 8 chars are
    // unique enough for a single plan's recipe list.
    return id.slice(0, 8);
  }

  function outcomeTone(o: RecipeOutcomeDto): 'ok' | 'skip' | 'fail' {
    if (o.kind === 'succeeded') return 'ok';
    if (o.kind === 'skipped') return 'skip';
    return 'fail';
  }

  function outcomeLabel(o: RecipeOutcomeDto): string {
    if (o.kind === 'succeeded') {
      return `${o.records_produced} record${o.records_produced === 1 ? '' : 's'}`;
    }
    if (o.kind === 'skipped') return 'skipped';
    return `failed @ ${o.stage}`;
  }

  function outcomeDetail(o: RecipeOutcomeDto): string {
    if (o.kind === 'skipped') return o.reason;
    if (o.kind === 'failed') return o.message;
    return '';
  }

  function formatRunStarted(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const hh = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    const ss = String(d.getSeconds()).padStart(2, '0');
    return `${hh}:${min}:${ss}`;
  }

  function runStatusTone(r: FetchRunSummaryDto): 'ok' | 'partial' | 'fail' | 'pending' {
    if (!r.finished_at) return 'pending';
    if (r.error_summary) return 'fail';
    if (r.recipes_succeeded === r.recipes_attempted && r.recipes_attempted > 0) return 'ok';
    if (r.recipes_succeeded > 0) return 'partial';
    return 'fail';
  }
</script>

<section class="fetch-report">
  {#if plans.fetchReport}
    {@const report = plans.fetchReport}
    <header class="head">
      <span class="label">last run</span>
      <span class="summary">
        <span class="kv"><span class="k">attempted</span><span class="v">{report.recipes_attempted}</span></span>
        <span class="kv"><span class="k">succeeded</span><span class="v">{report.recipes_succeeded}</span></span>
        <span class="kv"><span class="k">records</span><span class="v">{report.records_produced}</span></span>
      </span>
    </header>

    {#if report.error_summary}
      <p class="top-error">{report.error_summary}</p>
    {/if}

    {#if report.outcomes.length === 0}
      <p class="empty">no recipes were authored or applied — check the plan's bound sources.</p>
    {:else}
      <ul class="outcomes">
        {#each report.outcomes as o (o.recipe_id)}
          <li class="outcome" data-tone={outcomeTone(o)}>
            <span class="recipe-id">{shortId(o.recipe_id)}</span>
            <span class="source-id">{o.source_id}</span>
            <span class="status">{outcomeLabel(o)}</span>
            {#if outcomeDetail(o)}
              <span class="detail">{outcomeDetail(o)}</span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if plans.fetchRuns.length > 0}
    <div class="history">
      <span class="label">history</span>
      <ul class="runs">
        {#each plans.fetchRuns as r (r.id)}
          <li class="run" data-tone={runStatusTone(r)}>
            <span class="time">{formatRunStarted(r.started_at)}</span>
            <span class="counts">
              {r.recipes_succeeded}/{r.recipes_attempted}
              <span class="dot">·</span>
              {r.records_produced}r
            </span>
            {#if r.error_summary}
              <span class="run-error" title={r.error_summary}>!</span>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</section>

<style>
  .fetch-report {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .summary {
    display: flex;
    gap: 14px;
    font-family: var(--font-mono);
    font-size: 11px;
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .k  { color: var(--fg-quaternary); text-transform: uppercase; letter-spacing: 0.06em; }
  .v  { color: var(--fg-primary); }

  .top-error {
    margin: 0;
    padding: 6px 8px;
    background: rgba(200, 60, 60, 0.08);
    border-left: 2px solid var(--signal-error, #c83c3c);
    color: var(--fg-primary);
    font-size: 12px;
    font-family: var(--font-mono);
  }

  .empty {
    margin: 0;
    color: var(--fg-tertiary);
    font-size: 12px;
  }

  .outcomes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .outcome {
    display: grid;
    grid-template-columns: 80px 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 10px;
    row-gap: 2px;
    align-items: baseline;
    padding: 4px 6px;
    border-left: 2px solid transparent;
    font-size: 12px;
    font-family: var(--font-mono);
  }
  .outcome[data-tone="ok"]   { border-left-color: var(--signal-ok, #5b9c5e); }
  .outcome[data-tone="skip"] { border-left-color: var(--fg-quaternary); }
  .outcome[data-tone="fail"] { border-left-color: var(--signal-error, #c83c3c); background: rgba(200, 60, 60, 0.04); }

  .recipe-id { color: var(--fg-quaternary); }
  .source-id { color: var(--fg-secondary); }
  .status    { color: var(--fg-primary); justify-self: end; }
  .detail {
    grid-column: 1 / -1;
    font-size: 11px;
    color: var(--fg-tertiary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .history {
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-top: 1px solid var(--border-subtle);
    padding-top: 8px;
  }
  .runs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .run {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    padding: 2px 6px;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-secondary);
  }
  .run[data-tone="ok"]      { border-color: var(--signal-ok, #5b9c5e); }
  .run[data-tone="partial"] { border-color: var(--signal-warning); }
  .run[data-tone="fail"]    { border-color: var(--signal-error, #c83c3c); }
  .run[data-tone="pending"] { border-style: dashed; }
  .time   { color: var(--fg-tertiary); }
  .counts { color: var(--fg-primary); }
  .dot    { color: var(--fg-quaternary); margin: 0 2px; }
  .run-error {
    color: var(--signal-error, #c83c3c);
    font-weight: 600;
    cursor: help;
  }
</style>
