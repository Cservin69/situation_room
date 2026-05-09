<!--
  ExpectationCoverage — Session 46.

  Surfaces the recipe-author prompt's "narrow honest coverage"
  discipline (recipe_author.md v1.14 §"Coverage discipline — bindings
  vs expectations"). For each plan expectation across the four
  binding-addressable buckets — observation_metric, event_type,
  entity_kind, relation_kind — list the recipes that bind to it, or
  surface "uncovered" explicitly when none do.

  ## Why this surface earns its weight

  The Session-45 live run produced a USGS recipe that targets
  `observation_metric[0]` (production) only, despite the plan
  declaring 4 obs metrics (production, reserves, refining_capacity,
  spot_price). That narrow coverage is **intentional** per the
  prompt's discipline (one scalar per fetch → one binding per
  scalar → no padded bindings). Before this surface the operator had
  to read recipe JSON to see which expectations were covered. The
  matrix makes the narrow coverage legible at a glance.

  ## Rendering vocabulary

  Each row is a (bucket, index) pair plus a chip per binding recipe.
  Uncovered rows render an "uncovered" pill in a dim attention tone
  — same neutral-attention treatment FetchReport.svelte uses for
  declined outcomes ("outcome that needs attention but isn't a
  runtime failure").

  Recipes can bind to multiple expectations (rare but valid: a
  CSV-cell extraction over a multi-column row could populate a
  metric and a flag in the same record_type bucket). The same
  recipe_id may therefore appear in multiple rows; the chip's
  source_id and short id keep that legible.

  ## Empty states

  - `plans.expectationCoverage === null` → we haven't asked yet
    (pending plan, or pre-acceptance render). The component renders
    nothing — the panel's surrounding bucket panels already cover
    this state.
  - `plans.expectationCoverage.length === 0` → the plan declares no
    binding-addressable expectations. Rare but legitimate (a plan
    whose bucket counts are all zero). Render a small empty hint.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { ExpectationCoverageRowDto } from '$lib/api/types/ExpectationCoverageRowDto';
  import type { ExpectationCoverageRecipeDto } from '$lib/api/types/ExpectationCoverageRecipeDto';

  // Reactive view over the nullable store property. Pulling the
  // value into a derived once keeps the template free of the
  // `T[] | null` narrowing dance svelte-check otherwise complains
  // about (proxy-property accesses inside `{#if}` blocks don't
  // always carry the narrowed type to subsequent expressions).
  let coverage = $derived(plans.expectationCoverage);
  let coveredCount = $derived(
    coverage === null ? 0 : coverage.filter((r) => r.recipes.length > 0).length,
  );

  function shortId(id: string): string {
    return id.slice(0, 8);
  }

  function bucketLabel(bucket: string): string {
    // Match the bucket-panel headers in PlanReview.svelte: lowercase
    // singular forms. The `bucket` value on the wire is already the
    // recipe-author prompt's vocabulary; this just maps to the panel
    // copy.
    switch (bucket) {
      case 'observation_metric':
        return 'observation';
      case 'event_type':
        return 'event';
      case 'entity_kind':
        return 'entity';
      case 'relation_kind':
        return 'relation';
      default:
        return bucket;
    }
  }

  function rowKey(row: ExpectationCoverageRowDto): string {
    // (bucket, index) is unique per plan.
    return `${row.bucket}:${row.index}`;
  }

  function chipKey(
    row: ExpectationCoverageRowDto,
    chip: ExpectationCoverageRecipeDto,
  ): string {
    // recipe_id is per-recipe; pair with row to disambiguate when
    // the same recipe binds to multiple expectations.
    return `${rowKey(row)}:${chip.recipe_id}`;
  }
</script>

{#if coverage !== null && coverage.length > 0}
  <section class="coverage">
    <header class="head">
      <span class="label">expectation coverage</span>
      <span class="hint">
        {coveredCount}
        of {coverage.length} covered
      </span>
    </header>
    <ul class="rows">
      {#each coverage as row (rowKey(row))}
        <li class="row" data-covered={row.recipes.length > 0}>
          <span class="bucket">{bucketLabel(row.bucket)}</span>
          <span class="index">[{row.index}]</span>
          <span class="ex-label" title={row.label}>
            {#if row.label.length > 0}
              {row.label}
            {:else}
              <em class="orphan-marker">
                orphan binding — recipe targets index {row.index} but the
                plan no longer declares it
              </em>
            {/if}
          </span>
          <span class="chips">
            {#if row.recipes.length === 0}
              <span class="uncovered" title="No recipe binds to this expectation. Narrow honest coverage is the prompt's discipline (one scalar per fetch); each uncovered expectation is a candidate for an additional source or a re-classification.">
                uncovered
              </span>
            {:else}
              {#each row.recipes as chip (chipKey(row, chip))}
                <span
                  class="chip"
                  title={`recipe ${chip.recipe_id} · record_type ${chip.record_type}`}
                >
                  <span class="chip-id">{shortId(chip.recipe_id)}</span>
                  <span class="chip-source">{chip.source_id}</span>
                </span>
              {/each}
            {/if}
          </span>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .coverage {
    display: flex;
    flex-direction: column;
    gap: 8px;
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
  .hint {
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
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
    grid-template-columns: 80px 32px minmax(120px, 1fr) auto;
    align-items: baseline;
    column-gap: 8px;
    padding: 4px 6px;
    border-left: 2px solid transparent;
    font-family: var(--font-mono);
    font-size: 11px;
  }
  /* Covered rows get the same positive border treatment FetchReport
     uses for `succeeded` outcomes; uncovered rows get the dim
     neutral-attention treatment from `declined`. */
  .row[data-covered='true'] {
    border-left-color: var(--signal-positive);
  }
  .row[data-covered='false'] {
    border-left-color: var(--fg-tertiary);
    background: var(--bg-panel-alt);
  }

  .bucket {
    color: var(--fg-tertiary);
    text-transform: lowercase;
  }
  .index {
    color: var(--fg-quaternary);
  }
  .ex-label {
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .orphan-marker {
    color: var(--signal-warning);
    font-style: italic;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    justify-self: end;
  }

  .chip {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    padding: 2px 6px;
    background: var(--bg-canvas);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    font-size: 10px;
    cursor: help;
  }
  .chip-id {
    color: var(--fg-quaternary);
  }
  .chip-source {
    color: var(--fg-secondary);
  }

  .uncovered {
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    padding: 2px 6px;
    color: var(--fg-tertiary);
    border: 1px solid var(--fg-tertiary);
    border-radius: 2px;
    background: transparent;
    cursor: help;
  }
</style>
