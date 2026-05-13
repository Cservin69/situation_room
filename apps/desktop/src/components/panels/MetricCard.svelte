<!--
  MetricCard — one observation metric, rendered for the situation-room
  records dashboard (Session 58).

  ## What this component answers

  "The plan produced N observation records. Show me, at a glance,
  the metric these N records are about, the latest value, the unit,
  the period, and the source — and if there's more than one
  observation, the trend across them."

  Each MetricCard renders ONE metric (e.g. `production`,
  `reserves`, `price`). Multiple observations of the same metric
  across different `valid_at` get collapsed into one card with a
  sparkline; observations of different metrics get separate cards.
  The parent (`RecordsDashboard`) does the grouping; this component
  receives the pre-grouped slice.

  ## Single-point vs. multi-point rendering

  - **N = 1** — the "big number" mode. Latest value renders large
    (32px tabular-num) with the unit smaller and to the right of
    the value. No sparkline. The footer carries the year and
    source.
  - **N ≥ 2** — adds a sparkline above the big number, plus a
    delta chip (latest − previous) signed and color-coded
    (positive → `--signal-positive`; negative → `--signal-negative`;
    unchanged → muted).

  ## Why these fields specifically

  An observation card needs to answer four questions in the order
  the eye scans them:
    1. **What is this?** → `metric` (the title bar)
    2. **What's the number?** → `value` + `unit` (the big number)
    3. **When?** → `valid_at` year, plus the `period` chip for
       whether it's annual/quarterly/spot/etc.
    4. **From where?** → host extracted from `provenance.source_url`,
       with the full URL as the title-attribute hover.

  Confidence is intentionally NOT surfaced as a foreground field —
  ADR 0007 keeps confidence as an envelope-level discipline, not a
  user-facing "trust score." A future detail view can show it; on
  the dashboard it would compete for attention with the value
  itself, which is the wrong tradeoff.

  ## DTOs are opaque content

  `ObservationDto.content` is wire-typed as `unknown` (the same
  convention `RecordCard` uses). The shape lives in
  `crates/core/src/schema/content.rs` and is read informally via
  the `safeGet` helper, mirroring `recordSummary.ts`. When a future
  session promotes content to typed DTOs, replace `safeGet` with
  property access.
-->
<script lang="ts">
  import type { ObservationDto } from '$lib/api/types/ObservationDto';
  import MiniSparkline from '$components/charts/MiniSparkline.svelte';
  import CopyButton from '$components/common/CopyButton.svelte';

  interface Props {
    /**
     * The metric name shared by every record in `records`. Used
     * as the card title and as the implicit grouping key the
     * parent applied when slicing.
     */
    metric: string;
    /**
     * Observation records for this metric. The parent guarantees
     * every record's `content.metric` matches `metric`. May be
     * length 1 or longer.
     */
    records: ObservationDto[];
  }
  let { metric, records }: Props = $props();

  // -- safe shape reads (see header docstring) ----------------------

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
    // ObservationPeriod::Custom(String) serialises as an object;
    // surface "custom" as the chip label and put the full ISO 8601
    // value on hover via title (caller's responsibility).
    if (p && typeof p === 'object' && 'custom' in p) return 'custom';
    return '';
  }

  /**
   * Format a number for the big-number display. Uses
   * `Intl.NumberFormat` with thousands separators and a sensible
   * fraction-digit choice based on magnitude:
   *   - |v| ≥ 1000 → 0 fraction digits (production, reserves)
   *   - |v| ≥ 1   → up to 2 fraction digits (utilization, p/e)
   *   - |v| < 1   → up to 4 fraction digits (small ratios)
   * The locale is fixed to `en-US` so the visual layout is stable
   * across operator systems regardless of OS locale; numbers in
   * this product are tabular by convention (see `--font-mono`).
   */
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

  /**
   * Extract a date label from `valid_at` (preferred) or
   * `observed_at` (fallback). Returns the year-only for annual
   * observations, full ISO date for sub-annual, and empty string
   * if neither is parseable.
   */
  function dateLabel(r: ObservationDto): string {
    const raw = r.envelope.valid_at ?? r.envelope.observed_at;
    if (!raw) return '';
    const d = new Date(raw);
    if (Number.isNaN(d.valueOf())) return '';
    const period = obsPeriod(r);
    if (period === 'annual') return String(d.getUTCFullYear());
    // ISO date without time, in UTC. Localised "tonight at" copy
    // would be cute but wrong: the data is referenced by date, not
    // by the operator's wall clock.
    return d.toISOString().slice(0, 10);
  }

  /**
   * Best-effort host extraction from a source URL. Empty string
   * when the URL is null or unparseable. Strips a leading `www.`
   * so "www.usgs.gov" and "usgs.gov" read identically.
   */
  function hostOf(rawUrl: string | null): string {
    if (!rawUrl) return '';
    try {
      const u = new URL(rawUrl);
      const h = u.host;
      return h.startsWith('www.') ? h.slice(4) : h;
    } catch {
      return '';
    }
  }

  // -- derived state ------------------------------------------------

  /**
   * Records sorted by valid_at ascending (chronological). Records
   * with null valid_at sort last by observed_at — they're "stamped
   * now, no historical timestamp", which is the right place at the
   * end of the chronology. Stable sort because Array.prototype.sort
   * is spec-stable in modern JS.
   */
  let chronological = $derived.by(() => {
    const copy = [...records];
    copy.sort((a, b) => {
      const aKey = a.envelope.valid_at ?? a.envelope.observed_at;
      const bKey = b.envelope.valid_at ?? b.envelope.observed_at;
      return aKey < bKey ? -1 : aKey > bKey ? 1 : 0;
    });
    return copy;
  });

  let latest = $derived(chronological[chronological.length - 1]);
  let previous = $derived(
    chronological.length >= 2 ? chronological[chronological.length - 2] : null,
  );

  let latestValue = $derived(latest ? obsValue(latest) : null);
  let previousValue = $derived(previous ? obsValue(previous) : null);

  /**
   * Delta from previous to latest. `null` when either side is
   * missing or when there's only one record. Surfaced as a signed
   * chip; sign drives color.
   */
  let delta = $derived(
    latestValue !== null && previousValue !== null
      ? latestValue - previousValue
      : null,
  );

  let deltaSign = $derived(
    delta === null
      ? 'flat'
      : delta > 0
      ? 'positive'
      : delta < 0
      ? 'negative'
      : 'flat',
  );

  /**
   * Maximum sparkline points before uniform decimation kicks in.
   * Session 68 — chosen so the 80×24 viewBox can resolve every
   * point as a distinct vertex (~80 horizontal pixels gives ~80
   * polyline edges; over-sampling beyond ~2× pixel resolution
   * is just visual noise and CPU cost on the parse/render path).
   * The `latest` and `previous` derived values upstream operate
   * on the chronological full set, not the decimated points, so
   * the "big number" and delta chip are unaffected.
   */
  const SPARK_MAX_POINTS = 200;

  let sparkPoints = $derived.by(() => {
    const raw = chronological
      .map((r, i) => {
        const v = obsValue(r);
        if (v === null) return null;
        const stamp = r.envelope.valid_at ?? r.envelope.observed_at;
        const x = stamp ? new Date(stamp).valueOf() : i;
        return { x: Number.isFinite(x) ? x : i, y: v };
      })
      .filter((p): p is { x: number; y: number } => p !== null);

    if (raw.length <= SPARK_MAX_POINTS) return raw;

    // Uniform decimation. Always keep first + last; sample evenly
    // in between. This preserves the trend shape; per-point
    // anomalies smooth out, which is correct for a sparkline (a
    // "spike at index 437" is a detail-view question, not a
    // dashboard question).
    const stride = (raw.length - 1) / (SPARK_MAX_POINTS - 1);
    const out: { x: number; y: number }[] = [];
    for (let i = 0; i < SPARK_MAX_POINTS; i++) {
      const idx = Math.round(i * stride);
      out.push(raw[idx]);
    }
    return out;
  });

  let unitLabel = $derived(latest ? obsUnit(latest) : '');
  let periodLabel = $derived(latest ? obsPeriod(latest) : '');
  let whenLabel = $derived(latest ? dateLabel(latest) : '');
  let sourceHost = $derived(latest ? hostOf(latest.envelope.provenance.source_url) : '');
  let sourceUrl = $derived(latest ? latest.envelope.provenance.source_url ?? '' : '');
</script>

<article class="metric-card">
  <header class="head">
    <span class="metric-name" title={metric}>{metric}</span>
    {#if records.length > 1}
      <span class="count" title="{records.length} observations">×{records.length}</span>
    {/if}
  </header>

  {#if sparkPoints.length >= 2}
    <div class="spark" title="trend across {sparkPoints.length} observations">
      <MiniSparkline
        points={sparkPoints}
        width={120}
        height={28}
        color={deltaSign === 'negative'
          ? 'var(--signal-negative)'
          : deltaSign === 'positive'
            ? 'var(--signal-positive)'
            : 'var(--signal-info)'}
      />
    </div>
  {/if}

  <div class="big">
    {#if latestValue !== null}
      <span class="value">{fmtValue(latestValue)}</span>
      {#if unitLabel}
        <span class="unit">{unitLabel}</span>
      {/if}
    {:else}
      <span class="value missing">—</span>
    {/if}
    {#if delta !== null}
      <span class="delta" data-sign={deltaSign} title="change from previous observation">
        {delta > 0 ? '+' : ''}{fmtValue(delta)}
      </span>
    {/if}
  </div>

  <footer class="foot">
    {#if periodLabel}
      <span class="chip period" title="reporting period">{periodLabel}</span>
    {/if}
    {#if whenLabel}
      <span class="when" title={latest?.envelope.valid_at ?? latest?.envelope.observed_at ?? ''}>
        {whenLabel}
      </span>
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
  .metric-card {
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
  .metric-name {
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

  .spark {
    height: 28px;
    width: 100%;
  }

  .big {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex-wrap: wrap;
  }
  .value {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 28px;
    line-height: 1.1;
    color: var(--fg-primary);
  }
  .value.missing {
    color: var(--fg-tertiary);
    font-size: 18px;
  }
  .unit {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
  }
  .delta {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    padding: 1px 5px;
    border-radius: 2px;
    margin-left: auto;
  }
  .delta[data-sign='positive'] {
    color: var(--signal-positive);
    background: rgba(91, 198, 133, 0.08);
  }
  .delta[data-sign='negative'] {
    color: var(--signal-negative);
    background: rgba(229, 96, 74, 0.08);
  }
  .delta[data-sign='flat'] {
    color: var(--fg-tertiary);
    background: var(--bg-panel-alt);
  }

  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 10px;
    color: var(--fg-tertiary);
    flex-wrap: wrap;
  }
  .chip {
    font-family: var(--font-mono);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    padding: 0 4px;
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
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
  .metric-card:hover .source-wrap :global(.copy-btn),
  .source-wrap :global(.copy-btn:focus-visible),
  .source-wrap :global(.copy-btn.copied) {
    opacity: 1;
  }
</style>
