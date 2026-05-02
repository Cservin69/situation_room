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

  BAKED badge (Session 18, ADR 0007 Amendment 3)
  -----------------------------------------------

  When a recipe carries a `static_payload`, the runtime serves the
  baked bytes to extraction in place of an HTTP fetch. The recipe
  produces the same records on every fetch until re-authored — there
  is no live freshness path. The freshness model is materially
  different from the common HTML-addressable case, so the UI shows
  it explicitly:

    - **BAKED chip** in the recipe head, next to the source_id.
      Tooltip explains the bake-time-frozen freshness contract.
    - **Collapsible payload preview** below the produces block,
      showing the raw baked bytes the runtime feeds to extraction.
      Defaults closed (the payload can be large; users open it when
      diagnosing).

  The badge is shown iff `recipe.static_payload != null`. Empty-
  string would not occur on the wire — the validator at
  `build_validated_recipe` collapses empty / whitespace-only strings
  to None before storage, and the executor's short-circuit treats
  None as "fetch normally."

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
  import { plans, flagRecipe } from '$stores/plans.svelte';
  import type { RecipeDto } from '$lib/api/types/RecipeDto';
  import {
    outcomeTone,
    outcomeLabel,
    outcomeDetail,
    outcomeForRecipe,
  } from '$lib/outcomes';
  import RecipeFlagDialog from '$components/dialogs/RecipeFlagDialog.svelte';

  // ADR 0013: which recipe (if any) currently has its flag dialog
  // open. We key by `source_id` rather than `recipe.id` because the
  // feedback channel itself is keyed per (plan, source) — opening
  // the dialog on the same source's later recipe version should land
  // on the same persisted note. Null = no dialog open.
  let flagDialogSourceId: string | null = $state(null);
  let flagSubmitting = $state(false);

  function openFlagDialog(sourceId: string) {
    flagDialogSourceId = sourceId;
  }
  function closeFlagDialog() {
    flagDialogSourceId = null;
  }
  async function onFlagSubmit(note: string) {
    if (!flagDialogSourceId) return;
    flagSubmitting = true;
    try {
      const ok = await flagRecipe(flagDialogSourceId, note);
      if (ok) flagDialogSourceId = null;
      // On failure: dialog stays open, plans.error renders in the
      // parent banner, user can edit + resubmit.
    } finally {
      flagSubmitting = false;
    }
  }

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
  {@const feedback = plans.recipeFeedback[recipe.source_id]}
  <article class="recipe">
    <header class="recipe-head">
      <span class="source-id">{recipe.source_id}</span>
      {#if recipe.static_payload !== null}
        <!--
          BAKED badge — visible when the recipe carries a
          static_payload. The tooltip explains the bake-time-frozen
          freshness contract so users understand why the recipe
          will produce identical records every fetch.

          ADR 0007 Amendment 3 §"freshness model is explicit in
          the UI" — same data shape, different freshness, made
          visible.
        -->
        <span
          class="baked-badge"
          title="Bake-time-frozen: the runtime serves baked bytes to extraction in place of an HTTP fetch. This recipe will produce the same records on every fetch until re-authored. ADR 0007 Amendment 3."
        >BAKED</span>
      {/if}
      {#if feedback}
        <!--
          FLAGGED chip — ADR 0013. Visible when the operator has
          attached a feedback note for this (plan, source) pair.
          Clicking opens the dialog to edit; the note's full text
          shows on hover via the title attribute. The chip uses
          --signal-info because the recipe is annotated, not
          discarded — the freshness/lifecycle isn't broken; the
          author just has additional context for the next attempt.
        -->
        <button
          type="button"
          class="flagged-chip"
          title={feedback.note}
          onclick={() => openFlagDialog(recipe.source_id)}
        >FLAGGED</button>
      {/if}
      <span class="recipe-id">{shortId(recipe.id)}</span>
      <!--
        Flag button — always present so the operator can attach a
        note even on a recipe that's never failed (e.g. a wrong-
        shape extraction the user noticed by reading the records).
        When already flagged, the FLAGGED chip is the primary edit
        affordance; this button is a secondary entry point for the
        unflagged state.
      -->
      {#if !feedback}
        <button
          type="button"
          class="flag-button"
          title="Attach a note about what's wrong with this recipe — fed into the next authoring attempt for this source."
          onclick={() => openFlagDialog(recipe.source_id)}
        >flag</button>
      {/if}
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

    {#if recipe.static_payload !== null}
      <!--
        Baked-payload preview — the raw bytes the runtime serves to
        extraction in place of an HTTP fetch. Closed by default
        (payloads may be large); users open it when diagnosing why
        a baked recipe produced unexpected records.

        Rendered as a plain <pre> rather than parsed-and-pretty —
        the runtime hands these bytes to apply() exactly as written,
        and any reformatting here would mislead the user about what
        the runtime actually saw.
      -->
      <details class="block baked-block">
        <summary>baked payload</summary>
        <pre>{recipe.static_payload}</pre>
      </details>
    {/if}

    <footer class="recipe-foot">
      <span>authored {formatAuthoredAt(recipe.authored_at)}</span>
      <span>by {recipe.authored_by}</span>
      <span>v{recipe.version}</span>
    </footer>
  </article>
{/snippet}

{#if flagDialogSourceId !== null}
  <!--
    Flag dialog mount. Keyed implicitly by the {#if} block — the
    dialog mounts fresh whenever flagDialogSourceId becomes
    non-null, so the `initial` prop captured at construction time
    is the right value (matches RejectDialog's
    `untrack(() => initial)` pattern).

    `initial` is the existing note text for this source if there
    is one, else empty. Submitting empty routes through
    `flagRecipe` → `clearRecipeFeedback`.
  -->
  <RecipeFlagDialog
    sourceId={flagDialogSourceId}
    initial={plans.recipeFeedback[flagDialogSourceId]?.note ?? ''}
    submitting={flagSubmitting}
    onSubmit={onFlagSubmit}
    onCancel={closeFlagDialog}
  />
{/if}

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
    /*
     * No `justify-content: space-between` — that distributes space
     * equally between every item, which breaks down when the head
     * gains a variable trailing slot (the flag button or the
     * FLAGGED chip). Instead, we push the right-side cluster
     * (recipe-id + optional flag button) via `margin-left: auto`
     * on .recipe-id; items before .recipe-id (source-id, BAKED,
     * FLAGGED chips) flow left-to-right with the gap.
     */
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
    /* push to the right; trailing items follow with the gap */
    margin-left: auto;
  }

  /*
   * BAKED badge — Session 18, ADR 0007 Amendment 3.
   *
   * A visible chip in the recipe head announcing the bake-time-frozen
   * freshness model. Sized to sit between source_id (left) and
   * recipe-id (right) without disrupting the existing baseline
   * alignment. The hover tooltip carries the freshness explanation.
   *
   * Color discipline (ADR 0006 §"color is meaning, not decoration"):
   * `--signal-warning` is the right semantic — baked recipes
   * deserve the user's attention because their freshness contract
   * differs from the default. Not negative (the recipe isn't broken)
   * and not positive (the recipe isn't healthier than a live one) —
   * warning, because it's information the user must hold in mind.
   *
   * If global.css doesn't expose `--signal-warning` yet, the
   * fallback chain via var() is `--signal-warning, --fg-secondary` —
   * the badge degrades to a neutral chip rather than vanishing.
   */
  .baked-badge {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-warning, var(--fg-secondary));
    border: 1px solid var(--signal-warning, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: help;
    /* sit on the same baseline as source-id and recipe-id */
    align-self: center;
  }

  /*
   * FLAGGED chip — ADR 0013. Shown when the operator has attached a
   * feedback note for the recipe's (plan, source) pair. Same baseline
   * + sizing as .baked-badge, different hue: --signal-info because
   * the flag is informational (the recipe is annotated, not in a
   * degraded freshness state). Clickable: opens the dialog to edit.
   */
  .flagged-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    border: 1px solid var(--signal-info, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: pointer;
    align-self: center;
    transition: filter var(--duration-ui) var(--ease);
  }
  .flagged-chip:hover {
    filter: brightness(1.15);
  }

  /*
   * Flag button — secondary entry point on unflagged recipes. Sits
   * to the right of the recipe-id so it doesn't displace the head's
   * primary identifiers, and uses --fg-tertiary so it reads as
   * subordinate to the source-id and recipe-id. The FLAGGED chip
   * (when present) is the primary edit affordance; this button is
   * the "add a note" entry point.
   */
  .flag-button {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 2px;
    color: var(--fg-tertiary);
    background: transparent;
    border: 1px solid var(--border-subtle);
    cursor: pointer;
    align-self: center;
    transition: border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .flag-button:hover {
    border-color: var(--signal-info, var(--border-accent));
    color: var(--fg-primary);
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

  /*
   * Baked-payload block — same shape as the extraction/produces
   * blocks but with a left-edge accent in the warning tone, so the
   * user's eye picks up the bake-time-frozen distinction even when
   * the BAKED badge in the head has scrolled out of view.
   */
  .baked-block {
    border-left: 2px solid var(--signal-warning, var(--border-subtle));
  }
  .baked-block summary {
    color: var(--signal-warning, var(--fg-secondary));
  }
  .baked-block summary:hover {
    color: var(--fg-primary);
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
