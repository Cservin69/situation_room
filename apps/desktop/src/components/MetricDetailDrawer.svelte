<!--
  MetricDetailDrawer — per-Observation breakdown for one metric group
  (Session 86, parse-volatility visibility).

  ## What this component answers

  "MetricCard shows the latest value (e.g. 613.99) and a delta
  against the previous Observation. But across N fetches my dashboard
  has seen wildly different values for the same metric — sometimes
  613.99, sometimes 1.56, sometimes 7,408 — and the delta chip just
  blinks negative-positive. Which fetch produced which value, from
  what source URL, with what recipe?"

  This drawer lists every Observation in the metric group, newest-
  first:

      value · unit  ·  when  ·  host  ·  recipe ref  ·  source URL

  The recipe ref is parsed out of `provenance.source_id`, which the
  pipeline stamps as `"{source_id}#recipe:{recipe_id}@v{version}"`
  at apply-time (see `crates/pipeline/src/recipe_apply.rs::build_record`).
  Two records carrying the same recipe ref but different values came
  from the same recipe applied to different fetched bytes; two
  records with different recipe refs were authored by separate
  recipes (collisions across recipes are the most common volatility
  surface today — multiple selectors matching the same DOM scalar).

  ## Why this is the parse-volatility surface

  Session 86 diagnosis: the screenshot showed three metric_kinds
  (trading_volume / market_cap / closing_price) all rendering 613.99
  from cnbc.com — that's three recipes' latest fetches collapsing to
  the same scalar. The MetricCard hid this because it only renders
  one value at a time per metric. This drawer exposes:

    - cross-fetch values for the SAME metric (does it drift?)
    - which recipe authored which value (collisions across recipes
      become visible when you open all three metric drawers)
    - which source URL the recipe pulled from (CNBC quote page vs.
      a generic news article)

  Deeper provenance (selector path that matched, raw bytes excerpt)
  still requires a schema change — left to a future session.

  ## What this component does NOT do

  - **No edit / re-fetch.** Recipes are edited from RecipesPanel.
  - **No diff view across fetches.** The list is plain rows; the
    operator's eye does the diff. A future "values changed by > Nσ
    since the previous fetch" highlight is plausible follow-on work.
  - **No per-record action.** The source URL is hyperlinked; the
    rest of the row is read-only.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import type { ObservationDto } from '$lib/api/types/ObservationDto';
  import CopyButton from '$components/common/CopyButton.svelte';

  interface Props {
    /** The metric name shared by every record in `records`. */
    metric: string;
    /** All observations for this metric, in any order. The drawer
     * sorts internally so the parent can pass the same array it
     * passed to MetricCard. */
    records: ObservationDto[];
    /** Close-the-drawer callback. */
    onClose: () => void;
  }
  let { metric, records, onClose }: Props = $props();

  // ---- Shape readers (mirror MetricCard's safeGet pattern) --------

  function safeGet(obj: unknown, key: string): unknown {
    if (obj && typeof obj === 'object' && key in obj) {
      return (obj as Record<string, unknown>)[key];
    }
    return undefined;
  }
  function obsValue(r: ObservationDto): number | null {
    const v = safeGet(r.content, 'value');
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
  }
  function obsUnit(r: ObservationDto): string {
    const u = safeGet(r.content, 'unit');
    return typeof u === 'string' ? u : '';
  }
  function obsPeriod(r: ObservationDto): string {
    const p = safeGet(r.content, 'period');
    if (typeof p === 'string') return p;
    if (p && typeof p === 'object' && 'custom' in p) return 'custom';
    return '';
  }
  function fmtValue(v: number): string {
    const abs = Math.abs(v);
    const opts: Intl.NumberFormatOptions =
      abs >= 1000
        ? { maximumFractionDigits: 0 }
        : abs >= 1
          ? { maximumFractionDigits: 2 }
          : { maximumFractionDigits: 4 };
    return new Intl.NumberFormat('en-US', opts).format(v);
  }
  function hostOf(rawUrl: string | null | undefined): string {
    if (!rawUrl) return '';
    try {
      const u = new URL(rawUrl);
      const h = u.host;
      return h.startsWith('www.') ? h.slice(4) : h;
    } catch {
      return '';
    }
  }
  function whenOf(r: ObservationDto): { full: string; concise: string } {
    const raw = r.envelope.valid_at ?? r.envelope.observed_at;
    if (!raw) return { full: '', concise: '—' };
    const d = new Date(raw);
    if (Number.isNaN(d.valueOf())) return { full: raw, concise: raw };
    const period = obsPeriod(r);
    const concise =
      period === 'annual' ? String(d.getUTCFullYear()) : d.toISOString().slice(0, 10);
    return { full: d.toISOString(), concise };
  }

  /**
   * Parse the recipe reference out of `provenance.source_id`. The
   * pipeline stamps source_id as `"{source_id}#recipe:{uuid}@v{version}"`
   * at apply-time (recipe_apply.rs::build_record). When the stamp is
   * present we return both the bare source_id and the (uuid, version);
   * when it isn't (older promoted records, manually-derived rows)
   * `recipe` is `null` and we fall back to the raw source_id.
   */
  function parseRecipeRef(raw: string): {
    sourceId: string;
    recipeId: string | null;
    version: string | null;
  } {
    const i = raw.indexOf('#recipe:');
    if (i < 0) {
      return { sourceId: raw, recipeId: null, version: null };
    }
    const sourceId = raw.slice(0, i);
    const tail = raw.slice(i + '#recipe:'.length); // "<uuid>@v<version>"
    const at = tail.indexOf('@v');
    if (at < 0) {
      return { sourceId, recipeId: tail, version: null };
    }
    return {
      sourceId,
      recipeId: tail.slice(0, at),
      version: tail.slice(at + 2),
    };
  }

  /** Short-form recipe id for the row label: first 8 chars of the
   *  UUID is enough to distinguish across the typical N≤20 fetch
   *  history we're showing here. */
  function shortRecipeId(id: string): string {
    return id.length >= 8 ? id.slice(0, 8) : id;
  }

  // ---- Derived row shape ------------------------------------------

  type Row = {
    id: string;
    value: number | null;
    valueLabel: string;
    unit: string;
    period: string;
    whenConcise: string;
    whenFull: string;
    sourceUrl: string;
    sourceHost: string;
    recipeId: string | null;
    recipeShort: string | null;
    recipeVersion: string | null;
    sourceId: string;
  };

  let rows: Row[] = $derived.by(() => {
    // Sort newest-first by valid_at preferred, else observed_at.
    const copy = [...records];
    copy.sort((a, b) => {
      const ak = a.envelope.valid_at ?? a.envelope.observed_at;
      const bk = b.envelope.valid_at ?? b.envelope.observed_at;
      return ak < bk ? 1 : ak > bk ? -1 : 0;
    });
    return copy.map((r): Row => {
      const v = obsValue(r);
      const w = whenOf(r);
      const url = r.envelope.provenance.source_url ?? '';
      const ref = parseRecipeRef(r.envelope.provenance.source_id);
      return {
        id: r.id,
        value: v,
        valueLabel: v === null ? '—' : fmtValue(v),
        unit: obsUnit(r),
        period: obsPeriod(r),
        whenConcise: w.concise,
        whenFull: w.full,
        sourceUrl: url,
        sourceHost: hostOf(url),
        recipeId: ref.recipeId,
        recipeShort: ref.recipeId === null ? null : shortRecipeId(ref.recipeId),
        recipeVersion: ref.version,
        sourceId: ref.sourceId,
      };
    });
  });

  /** Distinct (recipeId, sourceId) pairs across the rows — this is
   *  the operator-facing answer to "how many sources/recipes
   *  contributed to this metric's volatility?" Stamped at the top of
   *  the drawer as a one-line summary. */
  let distinctRecipes: number = $derived.by(() => {
    const seen = new Set<string>();
    for (const r of rows) {
      seen.add(`${r.recipeId ?? '∅'}|${r.sourceId}`);
    }
    return seen.size;
  });

  /** Distinct value count — when 1, every fetch produced the same
   *  scalar (suspicious if N >> 1: the recipe might be matching a
   *  static element). When equal to rows.length, every fetch
   *  produced a unique value (high turbulence). Both extremes are
   *  worth surfacing. */
  let distinctValues: number = $derived.by(() => {
    const seen = new Set<string>();
    for (const r of rows) {
      seen.add(r.value === null ? '∅' : String(r.value));
    }
    return seen.size;
  });

  // ---- Close-on-Escape --------------------------------------------

  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onClose();
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-label="metric detail"
  tabindex={-1}
  onclick={onBackdropClick}
>
  <article class="drawer">
    <header class="head">
      <div class="head-main">
        <span class="title">metric</span>
        <span class="metric-name" title={metric}>{metric}</span>
        <span class="counts" title="rows · distinct values · distinct recipe-sources">
          {rows.length} row{rows.length === 1 ? '' : 's'} ·
          {distinctValues} value{distinctValues === 1 ? '' : 's'} ·
          {distinctRecipes} recipe{distinctRecipes === 1 ? '' : 's'}
        </span>
      </div>
      <button
        class="close"
        type="button"
        aria-label="close metric detail"
        onclick={onClose}
      >
        ×
      </button>
    </header>

    {#if rows.length > 1 && distinctValues === 1}
      <p class="warn-banner">
        every observation in this metric carries the same value
        ({rows[0].valueLabel}). This often means the recipe is matching a
        static DOM element rather than the live metric — open the recipe
        from RecipesPanel to inspect.
      </p>
    {/if}

    <div class="rows-wrap">
      <table class="rows">
        <thead>
          <tr>
            <th scope="col">value</th>
            <th scope="col">when</th>
            <th scope="col">source</th>
            <th scope="col">recipe</th>
            <th scope="col" class="url-col">url</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.id)}
            <tr>
              <td class="value-cell" title="observation value">
                <span class="value-num">{r.valueLabel}</span>
                {#if r.unit}
                  <span class="value-unit">{r.unit}</span>
                {/if}
              </td>
              <td class="when-cell" title={r.whenFull}>
                <span class="when-concise">{r.whenConcise}</span>
                {#if r.period}
                  <span class="period-chip">{r.period}</span>
                {/if}
              </td>
              <td class="source-cell" title={r.sourceId}>
                <span class="host">{r.sourceHost || r.sourceId || '—'}</span>
              </td>
              <td class="recipe-cell">
                {#if r.recipeShort}
                  <span class="recipe-short" title={r.recipeId}>{r.recipeShort}</span>
                  {#if r.recipeVersion}
                    <span class="recipe-version">v{r.recipeVersion}</span>
                  {/if}
                {:else}
                  <span class="recipe-none" title="no recipe stamp on this record (older promotion or derived path)">—</span>
                {/if}
              </td>
              <td class="url-cell">
                {#if r.sourceUrl}
                  <a
                    href={r.sourceUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    title={r.sourceUrl}
                    class="url-link"
                  >
                    {r.sourceUrl}
                  </a>
                  <CopyButton value={r.sourceUrl} />
                {:else}
                  <span class="url-none">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </article>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }

  .drawer {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(960px, 100%);
    max-height: 90vh;
    background: var(--bg-panel);
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    padding: 16px 20px 20px;
    overflow: hidden;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-main {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .title {
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .metric-name {
    font-family: var(--font-mono);
    font-size: 14px;
    color: var(--fg-primary);
  }
  .counts {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    color: var(--fg-quaternary);
  }
  .close {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
    border-radius: 3px;
  }
  .close:hover,
  .close:focus-visible {
    background: var(--bg-elevated, var(--bg-panel));
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  .warn-banner {
    margin: 0;
    padding: 6px 8px;
    font-size: 11px;
    color: var(--signal-warning, var(--signal-info));
    background: rgba(220, 180, 90, 0.07);
    border: 1px solid rgba(220, 180, 90, 0.25);
    border-radius: 2px;
  }

  .rows-wrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
  }
  .rows {
    width: 100%;
    border-collapse: collapse;
    font-size: 11px;
    font-family: var(--font-mono);
  }
  .rows thead th {
    text-align: left;
    font-weight: 500;
    color: var(--fg-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 9px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border-subtle);
    position: sticky;
    top: 0;
    background: var(--bg-panel);
  }
  .rows tbody td {
    padding: 4px 8px;
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-secondary);
    vertical-align: baseline;
  }
  .value-cell {
    white-space: nowrap;
  }
  .value-num {
    color: var(--fg-primary);
    font-variant-numeric: tabular-nums;
  }
  .value-unit {
    color: var(--fg-tertiary);
    margin-left: 4px;
  }
  .when-cell {
    white-space: nowrap;
  }
  .when-concise {
    color: var(--fg-primary);
  }
  .period-chip {
    margin-left: 6px;
    color: var(--fg-quaternary);
    font-size: 10px;
  }
  .source-cell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 200px;
  }
  .host {
    color: var(--fg-secondary);
  }
  .recipe-cell {
    white-space: nowrap;
  }
  .recipe-short {
    color: var(--fg-secondary);
  }
  .recipe-version {
    color: var(--fg-quaternary);
    margin-left: 4px;
    font-size: 10px;
  }
  .recipe-none {
    color: var(--fg-quaternary);
  }
  .url-col,
  .url-cell {
    max-width: 320px;
  }
  .url-cell {
    display: flex;
    align-items: center;
    gap: 4px;
    overflow: hidden;
  }
  .url-link {
    color: var(--fg-secondary);
    text-decoration: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
  }
  .url-link:hover,
  .url-link:focus-visible {
    color: var(--fg-primary);
    text-decoration: underline;
  }
  .url-none {
    color: var(--fg-quaternary);
  }
</style>
