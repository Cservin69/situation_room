<!--
  RecipeOutcomesHeatmap — Session 46.

  A horizontal strip showing per-(recipe-or-source, run) outcomes for
  the selected plan. One row per recipe (or per declined source);
  one cell per run. Cells are tinted by `outcome_kind` using the
  same closed vocabulary the live `FetchReport` panel uses, via the
  shared `outcomeTone` helper in `$lib/outcomes.ts`.

  ## Why this surface earns its weight

  The Session-45 live run surfaced 5 of 7 nominations declined with
  varied reasons. The fetch-report panel shows the *most recent*
  run's outcomes; before this surface the operator had no in-UI way
  to answer "is `pubs.usgs.gov` consistently the only winner across
  runs, or did it just happen this once?" The heatmap is exactly
  that question, answered at a glance.

  ## Data shape

  `plans.outcomesHistory` is an array of `RecipeOutcomesHistoryEntryDto`
  rows (one per recipe or declined source) carrying a `runs[]` of
  per-run cells ordered oldest-first by recording time. The component
  renders runs left-to-right, recipes top-to-bottom in the order the
  store delivered them (insertion order — which keeps the rows
  visually stable across renders even as new runs land).

  ## Empty states

  - `plans.outcomesHistory.length === 0` → no fetch outcomes recorded
    yet for this plan. Likely a freshly-classified plan or a plan
    that has only run once before the Session-46 migration landed.
    Render a small "no history yet" hint inline.
  - A row with empty `runs[]` is structurally impossible (the storage
    layer only emits entries for which at least one cell exists in
    the kept window).

  ## Tone vocabulary

  The cell colours mirror `FetchReport.svelte`'s row borders so a
  glance across both panels reads consistently:
    `succeeded`               → --signal-positive (green border)
    `failed` / `skipped`      → --signal-negative (red) /
                                --fg-quaternary (dim)
    `rate_limited`            → --signal-warning (amber)
    `declined` / `legacy_…`   → --fg-tertiary (neutral, "outcome
                                that needs attention but isn't a
                                runtime failure")
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { RecipeOutcomesHistoryEntryDto } from '$lib/api/types/RecipeOutcomesHistoryEntryDto';
  import type { RecipeOutcomesHistoryRunCellDto } from '$lib/api/types/RecipeOutcomesHistoryRunCellDto';

  function shortId(id: string): string {
    // UUIDv7 prefix — enough to disambiguate within a plan.
    return id.slice(0, 8);
  }

  function rowKey(entry: RecipeOutcomesHistoryEntryDto): string {
    // recipe_id is the natural key for authored recipes; for declined
    // / legacy rows there is no recipe_id, so we synthesize a key
    // from the source_id with a prefix so it can't collide with a
    // real UUID.
    return entry.recipe_id ?? `nocp:${entry.source_id}`;
  }

  function rowLabel(entry: RecipeOutcomesHistoryEntryDto): string {
    // `source_id` is the human-legible identifier in both cases; the
    // recipe_id (when present) is shown as a small prefix.
    return entry.source_id;
  }

  function cellTone(cell: RecipeOutcomesHistoryRunCellDto): string {
    // Matches the OutcomeTone closed set in $lib/outcomes.ts so the
    // CSS rules below align with the FetchReport panel's vocabulary
    // exactly.
    switch (cell.outcome_kind) {
      case 'succeeded':
        return 'ok';
      case 'skipped':
        return 'skip';
      case 'rate_limited':
        return 'limited';
      case 'declined':
        return 'declined';
      case 'legacy_plan_cannot_author':
        return 'legacy';
      case 'failed':
      default:
        return 'fail';
    }
  }

  function cellTitle(cell: RecipeOutcomesHistoryRunCellDto): string {
    // Hover detail: the kind, the run id, and any per-variant
    // payload. Keep terse — the heatmap is for at-a-glance scan; the
    // FetchReport panel is where the full reason lives.
    const at = new Date(cell.attempted_at);
    const when = isNaN(at.getTime())
      ? cell.attempted_at
      : at.toLocaleString();
    const lines = [`run ${cell.run_id.slice(0, 8)} · ${when}`, cell.outcome_kind];
    if (cell.records_produced !== null) {
      lines.push(`records: ${cell.records_produced}`);
    }
    if (cell.failure_stage) {
      lines.push(`stage: ${cell.failure_stage}`);
    }
    if (cell.retry_after_seconds !== null) {
      // ts-rs emits Rust `u64` as `bigint` since v8; coerce to
      // Number for the human-readable suffix. Values are bounded by
      // RFC 9110 §10.2.3's delta-seconds field (fits in a u32) so
      // the coercion is lossless in practice.
      const secs = Number(cell.retry_after_seconds);
      lines.push(`retry-after: ${secs}s`);
    }
    if (cell.message) {
      lines.push(cell.message);
    }
    return lines.join('\n');
  }
</script>

{#if plans.outcomesHistory.length > 0}
  <section class="heatmap">
    <header class="head">
      <span class="label">recipe history</span>
      <span class="hint">
        {plans.outcomesHistory.length} {plans.outcomesHistory.length === 1
          ? 'row'
          : 'rows'}
        · cells: oldest&nbsp;→&nbsp;newest
      </span>
    </header>
    <ul class="rows">
      {#each plans.outcomesHistory as entry (rowKey(entry))}
        <li class="row">
          <span class="row-label" title={rowLabel(entry)}>
            {#if entry.recipe_id}
              <span class="recipe-id">{shortId(entry.recipe_id)}</span>
            {:else}
              <span class="recipe-id decl-marker">decl·</span>
            {/if}
            <span class="source-id">{rowLabel(entry)}</span>
          </span>
          <span class="cells">
            {#each entry.runs as cell (cell.run_id)}
              <span
                class="cell"
                data-tone={cellTone(cell)}
                title={cellTitle(cell)}
                aria-label={cellTitle(cell)}
              ></span>
            {/each}
          </span>
        </li>
      {/each}
    </ul>
  </section>
{:else}
  <!--
    Pre-Session-46 plans land here: the `fetch_run_outcomes` table is
    fresh, so any plan whose runs predate migration 0016 has zero
    rows. Render a small hint rather than a blank panel so the
    operator sees the surface but understands why it's empty.
  -->
  <section class="heatmap empty">
    <header class="head">
      <span class="label">recipe history</span>
    </header>
    <p class="empty-explainer">
      No outcome history yet. Run a fetch to populate the heatmap.
    </p>
  </section>
{/if}

<style>
  .heatmap {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }

  .heatmap.empty {
    /* Empty state is dimmer — it's a placeholder, not the real
       surface. Same panel chrome so the slot keeps its shape across
       the populated/empty transition. */
    opacity: 0.7;
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
  .hint {
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
  }

  .empty-explainer {
    margin: 0;
    color: var(--fg-tertiary);
    font-size: 11px;
    font-style: italic;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .row {
    display: grid;
    grid-template-columns: minmax(120px, 240px) 1fr;
    column-gap: 10px;
    align-items: center;
    font-family: var(--font-mono);
    font-size: 11px;
  }

  .row-label {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .recipe-id {
    color: var(--fg-quaternary);
    font-size: 10px;
    flex: 0 0 auto;
  }
  .recipe-id.decl-marker {
    font-style: italic;
  }
  .source-id {
    color: var(--fg-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .cells {
    display: flex;
    gap: 2px;
    flex-wrap: nowrap;
    overflow-x: auto;
  }

  .cell {
    flex: 0 0 auto;
    width: 12px;
    height: 12px;
    border-radius: 2px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    cursor: help;
    transition: filter var(--duration-ui) var(--ease);
  }
  .cell:hover {
    filter: brightness(1.25);
  }
  /* Tone vocabulary mirrors $lib/outcomes.ts and FetchReport.svelte. */
  .cell[data-tone='ok'] {
    background: var(--signal-positive);
    border-color: var(--signal-positive);
  }
  .cell[data-tone='fail'] {
    background: var(--signal-negative);
    border-color: var(--signal-negative);
  }
  .cell[data-tone='limited'] {
    background: var(--signal-warning);
    border-color: var(--signal-warning);
  }
  .cell[data-tone='skip'] {
    background: var(--fg-quaternary);
    border-color: var(--fg-quaternary);
  }
  .cell[data-tone='declined'],
  .cell[data-tone='legacy'] {
    /* Same neutral-attention treatment FetchReport uses for
       declined / legacy rows: "outcome that needs attention but isn't
       a runtime failure." */
    background: var(--bg-panel-alt);
    border-color: var(--fg-tertiary);
  }
</style>
