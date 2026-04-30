<!--
  RecipesPanel — shows the recipes Level-2 authored for the selected
  plan, with each recipe's URL and extraction spec rendered for human
  inspection.

  Why this exists (Session 11 P2.5)
  ----------------------------------

  Up to Session 10, recipes were authored, persisted, and executed —
  but invisible in the UI. When a recipe failed (Session 9's
  `example.invalid` echo-back, or Session 11's first real run with
  Swiss-debt fetching `CH` instead of `CHE`), the only way to see
  what the LLM actually produced was a DuckDB query against the
  `recipes` table. That worked but punished the developer for using
  the desktop app at all.

  This panel makes the recipes legible inline. For each recipe:

    - source_id and dedup_key as a header strip
    - source_url shown as monospace + click-to-copy
    - extraction spec pretty-printed as JSON
    - production bindings pretty-printed as JSON
    - authored_at and authored_by in a footer strip

  Per the handoff, the structured fields render as JSON rather than
  per-mode bespoke components — that's a future polish session if
  this view ever ships beyond the developer audience.

  Empty state
  -----------

  The panel renders nothing when the selected plan has no recipes
  yet. That's the legitimate state for an accepted-but-not-fetched
  plan; the user sees a "run fetch to author them" hint via the
  RunFetchButton elsewhere on the screen. No need to add a "no
  recipes" message that would clutter the panel for the much more
  common case (just-classified plan).

  When the parent plan has no selection at all, this component also
  renders nothing — it never appears as a leaked artefact across
  selection boundaries because plans.recipes is reset on selectPlan.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { RecipeDto } from '$lib/api/types/RecipeDto';

  function shortId(id: string): string {
    // UUIDv7s are too long for inline display; first 8 chars are
    // unique enough within a single plan's recipe list.
    return id.slice(0, 8);
  }

  function formatAuthoredAt(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const yyyy = d.getFullYear();
    const mm = String(d.getMonth() + 1).padStart(2, '0');
    const dd = String(d.getDate()).padStart(2, '0');
    const hh = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    return `${yyyy}-${mm}-${dd} ${hh}:${min}`;
  }

  function prettyJson(value: unknown): string {
    // The wire shape carries `extraction` and `produces` as `unknown`
    // (raw `serde_json::Value` on the Rust side). The frontend
    // renders them as 2-space-indented JSON for readability.
    //
    // If serialization fails for any reason — circular refs are the
    // only realistic case, and they shouldn't appear in DTO data —
    // the panel surfaces the error rather than crashing the render.
    try {
      return JSON.stringify(value, null, 2);
    } catch (e) {
      return `// could not stringify: ${e instanceof Error ? e.message : String(e)}`;
    }
  }
</script>

{#if plans.recipes.length > 0}
  <section class="recipes-panel" aria-label="Authored recipes">
    <header class="head">
      <span class="label">recipes</span>
      <span class="count">{plans.recipes.length}</span>
    </header>

    <div class="list">
      {#each plans.recipes as recipe (recipe.id)}
        {@render recipeCard(recipe)}
      {/each}
    </div>
  </section>
{/if}

{#snippet recipeCard(recipe: RecipeDto)}
  <article class="recipe">
    <header class="recipe-head">
      <span class="source-id">{recipe.source_id}</span>
      <span class="recipe-id">{shortId(recipe.id)}</span>
    </header>

    <div class="kv-row">
      <span class="k">URL</span>
      <code class="url">{recipe.source_url}</code>
    </div>

    <details class="block" open>
      <summary>extraction</summary>
      <pre>{prettyJson(recipe.extraction)}</pre>
    </details>

    <details class="block">
      <summary>produces</summary>
      <pre>{prettyJson(recipe.produces)}</pre>
    </details>

    <footer class="recipe-foot">
      <span>authored {formatAuthoredAt(recipe.authored_at)}</span>
      <span>by {recipe.authored_by}</span>
      <span>v{recipe.version}</span>
    </footer>
  </article>
{/snippet}

<style>
  .recipes-panel {
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }

  .head {
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

  .list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .recipe {
    background: var(--bg-canvas);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }

  .recipe-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }

  .source-id {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg-primary);
    font-weight: 600;
  }

  .recipe-id {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
    font-variant-numeric: tabular-nums;
  }

  .kv-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .k {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    min-width: 32px;
  }

  .url {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
    word-break: break-all;
    background: var(--bg-panel);
    padding: 2px 4px;
    border-radius: 2px;
    flex: 1;
    /* Long URLs (especially World Bank's indicator paths) wrap
       gracefully rather than overflowing the card. */
  }

  .block {
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 6px 8px;
    background: var(--bg-panel);
  }

  .block summary {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    cursor: pointer;
    user-select: none;
  }

  .block summary:hover {
    color: var(--fg-secondary);
  }

  .block pre {
    margin: 6px 0 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-primary);
    white-space: pre-wrap;
    word-break: break-word;
    /* Constrains very deep production bindings from blowing out the
       review pane; the user can scroll within the block. */
    max-height: 320px;
    overflow-y: auto;
  }

  .recipe-foot {
    display: flex;
    gap: 12px;
    font-size: 10px;
    color: var(--fg-quaternary);
    padding-top: 4px;
    border-top: 1px solid var(--border-subtle);
  }
</style>
