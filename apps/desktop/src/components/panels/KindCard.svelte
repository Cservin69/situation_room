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
  }
  let {
    kind,
    count,
    sample,
    when = '',
    sourceHost = '',
    sourceUrl = '',
    chartSeries = null,
  }: Props = $props();

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

<article class="kind-card">
  <header class="head">
    <span class="kind-name" title={kind}>{kind}</span>
    <span class="count" title="{count} record{count === 1 ? '' : 's'}">×{count}</span>
  </header>

  {#if chartSeries && chartSeries.points.length > 0}
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
