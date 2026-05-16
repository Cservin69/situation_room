<!--
  KindCard — one (kind, count, sample) tile for the cross-plan
  dashboard's non-Observation panels (Session 63).

  ## What this component answers

  "For this record type, I have N records of kind X. Show me, at a
  glance, the kind, the count, a representative sample line, and the
  source." The same scan order the operator wants from
  `MetricCard` for observations: title → big number (count, not
  value) → sample → source.

  ## Why a uniform card across Event/Entity/Relation/Document/Assertion

  Each non-Observation type has its own grouping key (event_type,
  entity_kind, relation_kind, doc_kind, stance) and its own "sample
  line" surface (headline, canonical_name, from/to, title, claimant).
  Five bespoke cards would be five bespoke maintenance surfaces; one
  card that takes (kind, count, sample, when, sourceHost) keeps the
  per-type renderer in `RecordsDashboard` instead of fanning out new
  files. The price is that the sample line is a single string —
  per-type renderers do the formatting once at the call site.

  Confidence and provenance details aren't surfaced — same posture as
  MetricCard. A future detail-view session can add them; on the
  dashboard they compete for attention with the kind label and the
  sample, which are the load-bearing fields.
-->
<script lang="ts">
  import CopyButton from '$components/common/CopyButton.svelte';
  import MiniSparkline from '$components/charts/MiniSparkline.svelte';

  interface Props {
    /** Grouping key — event_type / entity kind / relation_kind / doc_kind / stance. */
    kind: string;
    /** Number of records sharing this kind. ≥ 1. */
    count: number;
    /** One-line representative sample (headline, canonical_name, etc.). May be empty. */
    sample: string;
    /**
     * Session 79 — optional multi-sample list. When the underlying
     * group contains multiple records that each have a distinct
     * sample line (entities of kind `company`: rusal, alcoa, …;
     * relations of kind `supplies_to`: rio_tinto → tsla,
     * panasonic → tsla, …), pass the full list here so all rows
     * surface, not just the first.
     *
     * The card deduplicates and caps to `SAMPLES_RENDER_CAP`; a
     * count of distinct sample strings beyond the cap is surfaced
     * as `+ N more`. Empty / null / single-entry arrays fall through
     * to the single-string `sample` rendering, which keeps existing
     * call sites unchanged.
     */
    samples?: string[] | null;
    /**
     * Short timestamp label for the latest record in this group.
     * Year-only for annual, ISO date otherwise. Empty when no
     * usable timestamp is available.
     */
    when?: string;
    /** Bare host (e.g. `noaa.gov`) for the latest record's source. Empty when unknown. */
    sourceHost?: string;
    /** Full source URL for hover. Empty when unknown. */
    sourceUrl?: string;
    /**
     * Session 69 — optional time-series for the tile preview.
     * When non-null, the card replaces its text sample with an
     * inline `MiniSparkline` and a small label strip showing the
     * detected value-key and entity (e.g. `close · TSLA`). When
     * null (the common case), the card renders the text sample as
     * before. The decision to render a chart vs. text is the
     * caller's; KindCard just honours whichever shape it received.
     *
     * Keeping this as an additive optional prop means every other
     * panel that uses KindCard (events / entities / relations /
     * assertions) is byte-for-byte unchanged.
     */
    chartSeries?: {
      points: Array<{ x: number; y: number }>;
      label?: string;
      valueKey?: string;
    } | null;
    /**
     * Session 70 — optional open-drawer callback. When non-null, the
     * card becomes clickable: a click anywhere on the card surface
     * (other than child controls like the source-copy button) fires
     * `onOpen`. The dashboard wires this only on the Documents panel
     * today — Event/Entity/Relation/Assertion cards stay
     * presentational because their underlying records don't carry a
     * preview-able body. Adding a click handler to those cards
     * without a destination would be a noop affordance.
     */
    onOpen?: (() => void) | null;
    /**
     * Session 80 — optional callback fired when the operator clicks
     * the `+ N more` overflow row at the bottom of the multi-sample
     * list. Receives the full deduped sample list (the same list the
     * card itself deduped from `samples`), so the parent doesn't have
     * to re-run the in-card dedup logic to populate its modal.
     *
     * Independent of `onOpen`: the modal is the discoverability
     * affordance for the long-tail samples, whereas `onOpen` opens a
     * type-specific drawer (Documents today). A future panel might
     * wire both — clicking the card body opens a drawer, clicking the
     * overflow opens the sample list.
     */
    onSamplesExpand?: ((all: string[]) => void) | null;
  }
  let {
    kind,
    count,
    sample,
    samples = null,
    when = '',
    sourceHost = '',
    sourceUrl = '',
    chartSeries = null,
    onOpen = null,
    onSamplesExpand = null,
  }: Props = $props();

  /*
    Session 79 — render-cap on the multi-sample list. Plans that
    extracted dozens of entities of the same kind (a global supply-
    chain plan with many companies, say) shouldn't blow up the card
    height; we cap at 8 visible lines and surface the overflow as a
    single `+ N more` row. The cap is intentionally above 5 so plans
    with realistic kind-cardinality (5-7 companies, 3-5 mines) show
    in full without an overflow indicator at all — the indicator only
    appears when the cap actually bites.
  */
  const SAMPLES_RENDER_CAP = 8;

  let dedupedSamples = $derived.by(() => {
    if (!samples) return [] as string[];
    const seen = new Set<string>();
    const out: string[] = [];
    for (const s of samples) {
      const t = (s ?? '').trim();
      if (t.length === 0) continue;
      if (seen.has(t)) continue;
      seen.add(t);
      out.push(t);
    }
    return out;
  });
  let visibleSamples = $derived(dedupedSamples.slice(0, SAMPLES_RENDER_CAP));
  let samplesOverflow = $derived(
    Math.max(0, dedupedSamples.length - visibleSamples.length),
  );
  let useSamplesList = $derived(dedupedSamples.length > 1);

  // Click handler that ignores clicks on the source-copy button and
  // any links/buttons nested inside the card. Without this guard,
  // clicking the copy button would also open the drawer — annoying
  // and slightly confusing.
  function onCardClick(e: MouseEvent) {
    if (!onOpen) return;
    const t = e.target as HTMLElement;
    if (t.closest('button, a')) return;
    onOpen();
  }

  function onCardKeydown(e: KeyboardEvent) {
    if (!onOpen) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onOpen();
    }
  }

  // Session 80 — overflow-row click. Surfaces the full deduped sample
  // list to the parent so it can open a modal listing every row in the
  // group. Stops propagation so the click doesn't also bubble up to
  // `onCardClick` and fire `onOpen` on a card that has both wired.
  function onSamplesOverflowClick(e: MouseEvent) {
    if (!onSamplesExpand) return;
    e.stopPropagation();
    onSamplesExpand(dedupedSamples);
  }

  function onSamplesOverflowKeydown(e: KeyboardEvent) {
    if (!onSamplesExpand) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      onSamplesExpand(dedupedSamples);
    }
  }

  // Strip and format the chart sub-caption — the small text shown
  // beside or below the sparkline. Empty when neither valueKey nor
  // label is meaningful (we never want the sparkline floating with
  // no context).
  let chartCaption = $derived.by(() => {
    if (!chartSeries) return '';
    const parts: string[] = [];
    if (chartSeries.valueKey) parts.push(chartSeries.valueKey);
    if (chartSeries.label) parts.push(chartSeries.label);
    const n = chartSeries.points.length;
    parts.push(`${n} pt${n === 1 ? '' : 's'}`);
    return parts.join(' · ');
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- Session 70: `<article>` is intrinsically noninteractive per the
     a11y rule, but `role="button"` + `tabindex={0}` makes it
     focusable + clickable at the a11y-tree level. The semantic
     `<article>` element is preserved (it's a self-contained record
     summary) rather than switching to `<div>` purely to silence the
     check. Keyboard activation is wired via `onCardKeydown`. -->
<article
  class="kind-card"
  class:clickable={onOpen !== null}
  role={onOpen ? 'button' : undefined}
  tabindex={onOpen ? 0 : undefined}
  onclick={onCardClick}
  onkeydown={onCardKeydown}
>
  <header class="head">
    <span class="kind-name" title={kind}>{kind}</span>
    <span class="count" title="{count} record{count === 1 ? '' : 's'}">×{count}</span>
  </header>

  {#if useSamplesList}
    <!-- Session 79 — multi-sample list. The card surfaces every
         distinct sample line in the group (deduped, capped at
         SAMPLES_RENDER_CAP), so a `COMPANY ×4` group shows all four
         company names rather than just the first. Overflow beyond the
         cap is surfaced as a single `+ N more` row. The chart-preview
         branch takes precedence for Documents (where the chart is the
         load-bearing first-doc affordance); panels that don't pass
         `samples` fall through to the original single-`sample`
         render. -->
    <ul class="samples">
      {#each visibleSamples as s (s)}
        <li class="sample-line" title={s}>{s}</li>
      {/each}
      {#if samplesOverflow > 0}
        {#if onSamplesExpand}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
          <li
            class="sample-overflow clickable"
            role="button"
            tabindex={0}
            onclick={onSamplesOverflowClick}
            onkeydown={onSamplesOverflowKeydown}
            title="show all {dedupedSamples.length} samples"
          >+ {samplesOverflow} more</li>
        {:else}
          <li class="sample-overflow">+ {samplesOverflow} more</li>
        {/if}
      {/if}
    </ul>
  {:else if chartSeries && chartSeries.points.length > 0}
    <!-- Session 69 — chart preview (Path B). Renders when the
         caller detected a time-series shape in the underlying
         record's body. The sparkline scales freely inside the
         fixed-height container; the caption strip carries the
         what-am-I-looking-at signal. -->
    <div class="chart" title={sample || chartCaption}>
      <MiniSparkline points={chartSeries.points} />
    </div>
    {#if chartCaption}
      <p class="chart-caption">{chartCaption}</p>
    {/if}
  {:else if sample}
    <p class="sample" title={sample}>{sample}</p>
  {:else}
    <p class="sample missing">— no preview available</p>
  {/if}

  <footer class="foot">
    {#if when}
      <span class="when">{when}</span>
    {/if}
    {#if sourceHost}
      <span class="source-wrap">
        <span class="source" title={sourceUrl || sourceHost}>{sourceHost}</span>
        {#if sourceUrl}
          <CopyButton value={sourceUrl} />
        {/if}
      </span>
    {/if}
  </footer>
</article>

<style>
  .kind-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    min-width: 160px;
  }
  /* Session 70 — clickable cards (Documents panel today) get a
     cursor, a subtle hover lift, and a visible focus ring. The
     non-clickable variant is byte-for-byte unchanged from
     Session 69 because the `.clickable` class only attaches when
     `onOpen` is non-null. */
  .kind-card.clickable {
    cursor: pointer;
    transition: border-color var(--duration-ui) var(--ease),
                background var(--duration-ui) var(--ease);
  }
  .kind-card.clickable:hover {
    border-color: var(--border-strong);
    background: var(--bg-elevated, var(--bg-panel-alt));
  }
  .kind-card.clickable:focus-visible {
    outline: 2px solid var(--fg-secondary);
    outline-offset: 1px;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
  }
  .kind-name {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
    text-transform: none;
    letter-spacing: 0;
  }

  .sample {
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--fg-primary);
    /* Clamp to 3 lines so a long headline doesn't blow up the
       card height. The hover title carries the full string. The
       standard `line-clamp` property is included alongside its
       `-webkit-` prefix for forward-compat with browsers shipping
       the unprefixed form. */
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .sample.missing {
    color: var(--fg-tertiary);
    font-style: italic;
  }

  /* Session 79 — multi-sample list. Stacked, no bullets, monospace
     for symbols-and-IDs readability (entity slugs and relation
     `from → to` pairs both look better in mono). Each line ellipses
     individually so a long canonical_name doesn't force a wider card.
     The overflow row uses the same tertiary-color treatment as
     `.sample.missing` so it reads as metadata, not as another row. */
  .samples {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    line-height: 1.45;
    color: var(--fg-primary);
    font-family: var(--font-mono);
  }
  .sample-line {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sample-overflow {
    color: var(--fg-tertiary);
    font-style: italic;
    font-family: var(--font-sans, inherit);
  }
  /* Session 80 — interactive overflow row when `onSamplesExpand` is
     wired. Visually nudges that it's clickable without forcing a full
     button affordance (the overflow row reads as inline meta, not as a
     primary action — the dotted underline matches the rest of the
     dashboard's "this is a destination" cue). */
  .sample-overflow.clickable {
    cursor: pointer;
    text-decoration: underline dotted;
    text-underline-offset: 2px;
    transition: color var(--duration-ui) var(--ease);
  }
  .sample-overflow.clickable:hover,
  .sample-overflow.clickable:focus-visible {
    color: var(--fg-primary);
    outline: none;
  }

  /* Session 69 — chart preview (Path B). Sits where the .sample
     paragraph would be when the caller passes a chartSeries. The
     fixed height matches roughly the 3-line clamp so the tile
     keeps the same overall vertical rhythm whether it's showing
     text or a sparkline. */
  .chart {
    height: 48px;
    width: 100%;
    /* MiniSparkline renders its svg at 100% × 100% with
       preserveAspectRatio="none" so the container's dimensions
       decide the visible aspect ratio. */
  }
  .chart-caption {
    margin: 0;
    font-size: 10px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.02em;
    /* Single-line — if the caption gets long, ellipsise. The full
       value-key + label combo is short by construction (we cap at
       three pieces) so this is paranoia, not load-bearing. */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 10px;
    color: var(--fg-tertiary);
    flex-wrap: wrap;
  }
  .when {
    font-family: var(--font-mono);
    color: var(--fg-secondary);
  }
  /* Source group — host text + copy affordance. The copy button
     fades in on card-hover (and on its own focus-visible for
     keyboard users); the host text stays visible at all times so
     the operator sees *where* the URL points before deciding to
     copy. */
  .source-wrap {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .source {
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 14ch;
  }
  .source-wrap :global(.copy-btn) {
    opacity: 0;
    transition: opacity var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease),
                background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease);
  }
  .kind-card:hover .source-wrap :global(.copy-btn),
  .source-wrap :global(.copy-btn:focus-visible),
  .source-wrap :global(.copy-btn.copied) {
    opacity: 1;
  }
</style>
