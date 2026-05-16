<!--
  DrawerChart — interactive line chart for DocumentDrawer (Session 83).

  ## What this component answers

  The Session 69 KindCard tile shows one MiniSparkline of the
  highest-ranked numeric series (typically `close` for Yahoo-shaped
  feeds). When the operator clicks the tile and the Document drawer
  opens, they want two things the tile couldn't give them:

    1. **Pick a different metric.** Yahoo data carries close, open,
       high, low, volume, adjclose; FRED carries value; NHC carries
       wind / pressure. The detector already finds all of them — the
       drawer needs to surface the catalog so the operator can switch.

    2. **Read values at specific dates.** A sparkline shows trend
       shape; the drawer is the place to answer "what was TSLA on
       2025-09-14?". Hover crosshair + tooltip.

  ## Why a button-popover instead of a native <select>

  The options carry secondary metadata — last value, min, max — we
  want visible at-a-glance before the operator switches. A native
  `<select>` would only show the key; a popover renders the full
  preview row per option. The trigger is keyboard-focusable and
  closes on outside click / Escape so the affordance still meets
  accessibility expectations.

  ## Closed-vocabulary discipline

  The series keys passed in are whatever the JSON body declared.
  This component does no source routing — every key renders
  identically. The detector's `rankKey` is what places `close` first
  in the dropdown; switching to a different feed family changes the
  ordering organically.
-->
<script lang="ts">
  import type { ChartCatalog, ChartSeries } from '$lib/dashboard/document_chart';
  import {
    nearestIndex,
    formatChartTimestamp,
    formatChartValue,
  } from '$lib/dashboard/document_chart';
  import { onMount } from 'svelte';

  interface Props {
    /** Catalog from `detectChartCatalog`. Must have ≥ 1 series; the
     *  caller is responsible for the empty-catalog guard. */
    catalog: ChartCatalog;
    /** Initial series key. Defaults to the catalog's first
     *  (highest-ranked) series. Invalid keys fall back to index 0. */
    initialKey?: string;
  }
  let { catalog, initialKey }: Props = $props();

  // ---- Active series ----------------------------------------------

  function indexOfKey(key: string | undefined): number {
    if (!key) return 0;
    const i = catalog.series.findIndex((s) => s.key === key);
    return i >= 0 ? i : 0;
  }
  // `initialKey` is intentionally captured at component creation —
  // the prop name signals "seed me with this once". After mount,
  // `activeIndex` is operator-driven via the popover; later changes
  // to the parent's prop should NOT clobber that. The directive on
  // the following line marks the capture-once read as deliberate.
  // svelte-ignore state_referenced_locally
  let activeIndex = $state(indexOfKey(initialKey));
  let active = $derived<ChartSeries>(
    catalog.series[activeIndex] ?? catalog.series[0],
  );

  // ---- Per-series summary (for dropdown options) ------------------

  interface SeriesMeta { key: string; last: number; min: number; max: number; }
  let seriesMeta = $derived<SeriesMeta[]>(
    catalog.series.map((s) => {
      let min = Infinity;
      let max = -Infinity;
      for (const v of s.values) {
        if (v < min) min = v;
        if (v > max) max = v;
      }
      const last = s.values.length > 0 ? s.values[s.values.length - 1] : 0;
      return { key: s.key, last, min, max };
    }),
  );

  // ---- Chart geometry ---------------------------------------------

  // Session 83.1 — raw JSON now hides behind a toggle in
  // DocumentDrawer, so the chart can claim more vertical real
  // estate. 320px reads as a primary surface rather than a
  // preview strip while still leaving room for the toggle row
  // and a sensible scroll region below in 90vh-bounded drawers.
  const CHART_HEIGHT = 320;
  const PADDING = { top: 14, right: 60, bottom: 28, left: 14 };
  // Container width is bound via `bind:clientWidth` so the chart
  // re-flows with the drawer. SSR / first-render gets 700 as a
  // sensible default; the bind kicks in on mount.
  let containerW = $state(700);
  let plotW = $derived(Math.max(20, containerW - PADDING.left - PADDING.right));
  let plotH = CHART_HEIGHT - PADDING.top - PADDING.bottom;

  let domain = $derived.by(() => {
    const ts = catalog.timestamps;
    const vs = active.values;
    let xMin = Infinity;
    let xMax = -Infinity;
    let yMin = Infinity;
    let yMax = -Infinity;
    const n = Math.min(ts.length, vs.length);
    for (let i = 0; i < n; i++) {
      if (ts[i] < xMin) xMin = ts[i];
      if (ts[i] > xMax) xMax = ts[i];
      if (vs[i] < yMin) yMin = vs[i];
      if (vs[i] > yMax) yMax = vs[i];
    }
    // 5% y-padding so the line doesn't touch the top / bottom edges.
    const yPad = yMax === yMin ? Math.abs(yMax) * 0.05 + 1 : (yMax - yMin) * 0.05;
    return { xMin, xMax, yMin: yMin - yPad, yMax: yMax + yPad };
  });

  function xScale(x: number): number {
    const { xMin, xMax } = domain;
    if (xMax === xMin) return PADDING.left + plotW / 2;
    return PADDING.left + ((x - xMin) / (xMax - xMin)) * plotW;
  }
  function yScale(y: number): number {
    const { yMin, yMax } = domain;
    if (yMax === yMin) return PADDING.top + plotH / 2;
    return PADDING.top + plotH - ((y - yMin) / (yMax - yMin)) * plotH;
  }

  let pathPoints = $derived.by(() => {
    const ts = catalog.timestamps;
    const vs = active.values;
    const n = Math.min(ts.length, vs.length);
    const out: string[] = [];
    for (let i = 0; i < n; i++) {
      out.push(`${xScale(ts[i]).toFixed(2)},${yScale(vs[i]).toFixed(2)}`);
    }
    return out.join(' ');
  });

  // y-axis ticks: 5 evenly-spaced labels (min, q1, mid, q3, max).
  let yTicks = $derived.by(() => {
    const { yMin, yMax } = domain;
    if (!Number.isFinite(yMin) || !Number.isFinite(yMax)) return [];
    if (yMin === yMax) return [yMin];
    const steps = 4;
    const out: number[] = [];
    for (let i = 0; i <= steps; i++) {
      out.push(yMin + ((yMax - yMin) * i) / steps);
    }
    return out;
  });

  // ---- Hover crosshair --------------------------------------------

  let hoverIdx = $state<number | null>(null);

  function onMove(e: MouseEvent) {
    const target = e.currentTarget as SVGElement;
    const rect = target.getBoundingClientRect();
    const px = e.clientX - rect.left;
    if (px < PADDING.left || px > PADDING.left + plotW) {
      hoverIdx = null;
      return;
    }
    const { xMin, xMax } = domain;
    if (xMax === xMin) {
      hoverIdx = catalog.timestamps.length > 0 ? 0 : null;
      return;
    }
    const dataX = xMin + ((px - PADDING.left) / plotW) * (xMax - xMin);
    hoverIdx = nearestIndex(catalog.timestamps, dataX);
  }
  function onLeave() { hoverIdx = null; }

  // Tooltip layout: tries to render to the right of the crosshair;
  // flips to the left when it would clip the right edge.
  let tooltipBox = $derived.by(() => {
    if (hoverIdx === null) return null;
    const ts = catalog.timestamps[hoverIdx];
    const vs = active.values[hoverIdx];
    if (ts === undefined || vs === undefined) return null;
    const hx = xScale(ts);
    const hy = yScale(vs);
    const w = 108;
    const h = 34;
    const flipLeft = hx + 14 + w > containerW - PADDING.right + 2;
    const tx = flipLeft ? hx - 8 - w : hx + 8;
    const ty = Math.max(PADDING.top + 2, Math.min(hy - h / 2, PADDING.top + plotH - h));
    return { hx, hy, tx, ty, w, h, ts, vs };
  });

  // ---- Dropdown popover -------------------------------------------

  let popoverOpen = $state(false);
  let popoverEl: HTMLDivElement | undefined = $state();
  let triggerEl: HTMLButtonElement | undefined = $state();
  function togglePopover() { popoverOpen = !popoverOpen; }
  function selectIdx(i: number) {
    activeIndex = i;
    popoverOpen = false;
    // Reset hover when switching metric so the tooltip doesn't show
    // a stale y-value during the re-render.
    hoverIdx = null;
  }
  function onPopoverKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      popoverOpen = false;
      triggerEl?.focus();
    }
  }

  onMount(() => {
    function onDocClick(e: MouseEvent) {
      if (!popoverOpen) return;
      const t = e.target as Node;
      if (popoverEl && popoverEl.contains(t)) return;
      if (triggerEl && triggerEl.contains(t)) return;
      popoverOpen = false;
    }
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  });

  // For the x-axis label format we need the span — same units as
  // the timestamps themselves (seconds for Yahoo, ms for JS Date
  // feeds). The formatter's heuristic handles both.
  let xSpan = $derived(domain.xMax - domain.xMin);
</script>

<div class="dchart" bind:clientWidth={containerW}>
  <div class="header">
    <button
      bind:this={triggerEl}
      class="metric-trigger"
      type="button"
      aria-haspopup="listbox"
      aria-expanded={popoverOpen}
      onclick={togglePopover}
    >
      <span class="metric-key">{active.key || '(unnamed)'}</span>
      <span class="metric-meta">last {formatChartValue(seriesMeta[activeIndex]?.last ?? 0)}</span>
      <span class="caret" aria-hidden="true">▾</span>
    </button>
    {#if catalog.label}
      <span class="series-label">{catalog.label}</span>
    {/if}
    <span class="series-count">{catalog.timestamps.length} pt{catalog.timestamps.length === 1 ? '' : 's'}</span>
  </div>

  {#if popoverOpen}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      bind:this={popoverEl}
      class="popover"
      role="listbox"
      tabindex={-1}
      onkeydown={onPopoverKey}
    >
      {#each seriesMeta as meta, i}
        <button
          type="button"
          class="popover-item"
          class:active={i === activeIndex}
          role="option"
          aria-selected={i === activeIndex}
          onclick={() => selectIdx(i)}
        >
          <span class="popover-key">{meta.key || '(unnamed)'}</span>
          <span class="popover-stats">
            <span class="stat-last">last {formatChartValue(meta.last)}</span>
            <span class="stat-range">{formatChartValue(meta.min)}–{formatChartValue(meta.max)}</span>
          </span>
        </button>
      {/each}
    </div>
  {/if}

  <svg
    class="canvas"
    width={containerW}
    height={CHART_HEIGHT}
    onmousemove={onMove}
    onmouseleave={onLeave}
    role="img"
    aria-label="{active.key || 'series'} time series"
  >
    <!-- y gridlines + tick labels -->
    {#each yTicks as t}
      <line
        x1={PADDING.left}
        x2={containerW - PADDING.right}
        y1={yScale(t)}
        y2={yScale(t)}
        class="grid"
      />
      <text
        x={containerW - PADDING.right + 4}
        y={yScale(t)}
        class="ytick"
        dominant-baseline="middle"
      >
        {formatChartValue(t)}
      </text>
    {/each}

    <!-- x-axis labels: first and last timestamp -->
    {#if Number.isFinite(domain.xMin) && Number.isFinite(domain.xMax)}
      <text
        x={PADDING.left}
        y={CHART_HEIGHT - 6}
        class="xtick"
        text-anchor="start"
      >
        {formatChartTimestamp(domain.xMin, xSpan)}
      </text>
      <text
        x={containerW - PADDING.right}
        y={CHART_HEIGHT - 6}
        class="xtick"
        text-anchor="end"
      >
        {formatChartTimestamp(domain.xMax, xSpan)}
      </text>
    {/if}

    <!-- value polyline -->
    <polyline points={pathPoints} class="line" fill="none" />

    <!-- crosshair + tooltip on hover -->
    {#if tooltipBox}
      <line
        x1={tooltipBox.hx}
        x2={tooltipBox.hx}
        y1={PADDING.top}
        y2={PADDING.top + plotH}
        class="crosshair"
      />
      <circle cx={tooltipBox.hx} cy={tooltipBox.hy} r="3" class="dot" />
      <g class="tooltip" transform="translate({tooltipBox.tx}, {tooltipBox.ty})">
        <rect width={tooltipBox.w} height={tooltipBox.h} rx="2" class="tooltip-bg" />
        <text x="6" y="13" class="tooltip-date">{formatChartTimestamp(tooltipBox.ts, xSpan)}</text>
        <text x="6" y="27" class="tooltip-value">{formatChartValue(tooltipBox.vs)}</text>
      </g>
    {/if}
  </svg>
</div>

<style>
  .dchart {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  /* Header row — metric trigger, label, count. Wraps on narrow
     drawers so the dropdown doesn't push the count out of view. */
  .header {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .metric-trigger {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    padding: 5px 10px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    cursor: pointer;
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease);
  }
  .metric-trigger:hover,
  .metric-trigger:focus-visible,
  .metric-trigger[aria-expanded='true'] {
    background: var(--bg-elevated, var(--bg-panel));
    border-color: var(--border-strong);
  }
  .metric-key {
    font-weight: 500;
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .metric-meta {
    color: var(--fg-tertiary);
    font-size: 10px;
  }
  .caret {
    color: var(--fg-tertiary);
    font-size: 9px;
  }
  .series-label {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
    text-transform: lowercase;
  }
  .series-count {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
  }

  /* Popover floats below the trigger. Absolute positioning anchors
     to the .dchart wrap; z-index sits above the chart canvas. */
  .popover {
    position: absolute;
    top: 34px;
    left: 0;
    min-width: 240px;
    max-height: 280px;
    overflow-y: auto;
    background: var(--bg-panel);
    border: 1px solid var(--border-strong);
    border-radius: 3px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.38);
    z-index: 5;
    padding: 4px;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .popover-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 16px;
    padding: 6px 10px;
    background: transparent;
    border: none;
    border-left: 2px solid transparent;
    border-radius: 2px;
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    cursor: pointer;
    text-align: left;
    transition: background var(--duration-ui) var(--ease);
  }
  .popover-item:hover,
  .popover-item:focus-visible {
    background: var(--bg-panel-alt);
    outline: none;
  }
  .popover-item.active {
    background: var(--bg-panel-alt);
    border-left-color: var(--signal-info);
  }
  .popover-key {
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .popover-stats {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    font-size: 10px;
  }
  .stat-last { color: var(--fg-secondary); }
  .stat-range { color: var(--fg-tertiary); }

  /* SVG canvas — height fixed; width bound via JS so we can compute
     pixel-space layout (no preserveAspectRatio distortion). */
  .canvas {
    display: block;
    width: 100%;
    height: 320px;
    cursor: crosshair;
  }
  .grid {
    stroke: var(--border-subtle);
    stroke-width: 0.5;
    stroke-dasharray: 2 3;
  }
  .ytick,
  .xtick {
    font-family: var(--font-mono);
    font-size: 9px;
    fill: var(--fg-tertiary);
  }
  .line {
    stroke: var(--signal-info);
    stroke-width: 1.4;
    stroke-linejoin: round;
    stroke-linecap: round;
    vector-effect: non-scaling-stroke;
  }
  .crosshair {
    stroke: var(--fg-tertiary);
    stroke-width: 0.6;
    stroke-dasharray: 3 3;
    pointer-events: none;
  }
  .dot {
    fill: var(--signal-info);
    stroke: var(--bg-panel);
    stroke-width: 1.5;
    pointer-events: none;
  }
  .tooltip { pointer-events: none; }
  .tooltip-bg {
    fill: var(--bg-elevated, var(--bg-panel-alt));
    stroke: var(--border-strong);
    stroke-width: 0.75;
    opacity: 0.97;
  }
  .tooltip-date {
    font-family: var(--font-mono);
    font-size: 10px;
    fill: var(--fg-secondary);
  }
  .tooltip-value {
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 500;
    fill: var(--fg-primary);
  }
</style>
