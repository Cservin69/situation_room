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
    - per-recipe outcome badge from the most recent fetch run
      (Session 13 P2 — see below)
    - source_url shown as monospace + click-to-copy
    - extraction spec pretty-printed as JSON
    - production bindings pretty-printed as JSON
    - authored_at and authored_by in a footer strip

  Per the handoff, the structured fields render as JSON rather than
  per-mode bespoke components — that's a future polish session if
  this view ever ships beyond the developer audience.

  Per-recipe outcome badge (Session 13 P2)
  -----------------------------------------

  The badge below the recipe header shows the outcome of the *most
  recent* fetch run for this recipe (matched by `recipe_id` against
  `plans.fetchReport.outcomes`). This is the "show at a glance which
  recipes are working and which aren't" affordance from the Session
  13 handoff. Four states:

    - ok    → "N records" badge in positive green
    - skip  → "skipped" in chrome grey, with the reason expandable
    - fail  → "failed @ stage" in negative red, with the message
              expandable
    - none  → "no fetch run yet" in tertiary grey, no expand

  Tone semantics + label/detail strings are shared with FetchReport
  via `$lib/outcomes.ts` so the two panels can't drift in their
  rendering of the same wire shape.

  The badge reflects only the most recent fetch run's outcomes —
  earlier runs aren't surfaced here (the run history strip in
  FetchReport covers that). When the user clicks "Run fetch" again,
  the badge updates with the new run's outcome for this recipe.

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
  import {
    outcomeTone,
    outcomeLabel,
    outcomeDetail,
    outcomeForRecipe,
  } from '$lib/outcomes';

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
  {@const outcome = outcomeForRecipe(recipe.id, plans.fetchReport?.outcomes)}
  {@const tone = outcomeTone(outcome)}
  {@const label = outcomeLabel(outcome)}
  {@const detail = outcomeDetail(outcome)}
  <article class="recipe">
    <header class="recipe-head">
      <span class="source-id">{recipe.source_id}</span>
      <span class="recipe-id">{shortId(recipe.id)}</span>
    </header>

    <!--
      Outcome strip. Always present (even in 'none' state) so the
      vertical rhythm of the cards stays consistent across plans
      with and without fetch runs. The expandable detail only
      renders when there's something to show.
    -->
    <div class="outcome-strip" data-tone={tone}>
      <span class="outcome-dot" aria-hidden="true"></span>
      <span class="outcome-label">{label}</span>
      {#if detail}
        <details class="outcome-detail">
          <summary>details</summary>
          <pre>{detail}</pre>
        </details>
      {/if}
    </div>

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

  /*
   * Outcome strip — small status row sitting between the recipe
   * head and the URL. Tone-driven: the dot color and the label
   * color shift with the wire's outcome kind. Border-left accent
   * mirrors the FetchReport outcome rows for visual consistency
   * across the two panels — same data shape, same chrome.
   *
   * Only canonical signal vars from global.css are used. ADR 0006
   * §"color is a meaning, not decoration" — and the no-hardcoded-hex
   * rule from the Session 13 handoff hard rules.
   */
  .outcome-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    background: var(--bg-panel);
    border-left: 2px solid var(--border-subtle);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 11px;
    flex-wrap: wrap;
  }
  .outcome-strip[data-tone="ok"]   { border-left-color: var(--signal-positive); }
  .outcome-strip[data-tone="skip"] { border-left-color: var(--fg-quaternary); }
  .outcome-strip[data-tone="fail"] {
    border-left-color: var(--signal-negative);
    background: var(--bg-panel-alt);
  }
  .outcome-strip[data-tone="none"] { border-left-color: var(--border-subtle); }

  .outcome-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-quaternary);
    flex-shrink: 0;
  }
  .outcome-strip[data-tone="ok"]   .outcome-dot { background: var(--signal-positive); }
  .outcome-strip[data-tone="skip"] .outcome-dot { background: var(--fg-tertiary); }
  .outcome-strip[data-tone="fail"] .outcome-dot { background: var(--signal-negative); }
  .outcome-strip[data-tone="none"] .outcome-dot { background: var(--fg-quaternary); }

  .outcome-label {
    color: var(--fg-primary);
    flex: 1;
    min-width: 0;
  }
  .outcome-strip[data-tone="none"] .outcome-label {
    color: var(--fg-tertiary);
  }
  .outcome-strip[data-tone="fail"] .outcome-label {
    color: var(--signal-negative);
  }

  .outcome-detail {
    /* Inline expandable; the summary is the click target, the pre
       is the message body. Same disclosure shape as the extraction
       and produces blocks below — keeps the recipe card's
       interaction grammar consistent. */
    flex-basis: 100%;
    margin-top: 4px;
  }
  .outcome-detail summary {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    cursor: pointer;
    user-select: none;
  }
  .outcome-detail summary:hover {
    color: var(--fg-secondary);
  }
  .outcome-detail pre {
    margin: 4px 0 0 0;
    padding: 6px 8px;
    background: var(--bg-inset);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-secondary);
    white-space: pre-wrap;
    word-break: break-word;
    /* Long failure messages (e.g. JSONPath syntax errors with the
       full expression echoed back) shouldn't blow out the card. */
    max-height: 160px;
    overflow-y: auto;
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
