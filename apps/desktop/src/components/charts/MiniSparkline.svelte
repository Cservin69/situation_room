<!--
  MiniSparkline — inline-SVG sparkline for the situation-room records
  dashboard (Session 58).

  ## Why inline SVG, not uplot / Plot / d3

  A sparkline is two lines and a dot. uplot and Observable Plot are
  excellent for full-featured charts (interactive axes, tooltips,
  legends) but each one adds ~50–200 KB to the rendered surface and
  introduces a setup-and-teardown lifecycle the component has to
  manage (canvas allocation, ResizeObserver, destroy on unmount).
  For a 80×24 rendering of N≤30 points with no interactivity, that
  cost is wholly disproportionate to the value: a plain `<svg>` with
  one `<polyline>` and one `<circle>` ships zero new code and
  renders in one frame.

  When a future session needs a full chart (drill-into-this-metric
  detail view with axis labels, hover crosshairs, brushing), uplot
  is already in `package.json` — use it then. This component is the
  cheap dense option.

  ## Visual conventions

  - The polyline traces the value series in chronological order.
  - The last point gets a small dot so the eye lands on "where we
    are now" without scanning to the right edge.
  - The min and max points get faint horizontal guides (1 pixel
    high, `--border-subtle`) so the operator can tell at a glance
    whether the series is mostly flat, mostly rising, or volatile.
  - When all points share the same value (degenerate "no variance"
    series), the polyline collapses to the vertical midline and the
    guides disappear; the dot still anchors at the right edge.
  - Color defaults to `--signal-info` (the neutral signal color) so
    the sparkline reads as descriptive, not editorialised. Callers
    that want directional color (positive trend → green, negative →
    red) override via the `color` prop.

  ## Layout

  - Pure SVG, no padding inside the viewBox. The component leaves
    layout (margins, alignment) entirely to the parent.
  - `preserveAspectRatio="none"` so the sparkline scales freely if
    the parent overrides `width` and `height`. A sparkline is
    pixel-density-agnostic by design — what matters is the *shape*,
    not the pixel-perfect alignment of points.

  ## Empty / degenerate input

  - 0 points: renders nothing (returns a zero-size svg). The parent
    is responsible for "we have no series" copy.
  - 1 point: renders a centered dot only (no polyline). A one-point
    "trend" is not a trend.
-->
<script lang="ts">
  /**
   * The points to render. `x` is the horizontal position (any
   * monotonic numeric scale — timestamp ms, year, sequence index).
   * `y` is the value. The component normalises both to fit
   * `[0, width]` × `[0, height]`; callers do not need to pre-scale.
   */
  interface Point {
    x: number;
    y: number;
  }

  interface Props {
    points: Point[];
    /** SVG viewBox width in user units. Default 80. */
    width?: number;
    /** SVG viewBox height in user units. Default 24. */
    height?: number;
    /**
     * Stroke color for the polyline and dot. Accepts any CSS
     * color, including CSS variables (`var(--signal-positive)`).
     * Default `var(--signal-info)`.
     */
    color?: string;
  }

  let {
    points,
    width = 80,
    height = 24,
    color = 'var(--signal-info)',
  }: Props = $props();

  /**
   * Normalised points in SVG coordinates. The transform is:
   *
   *   x_svg = ((x − x_min) / (x_max − x_min)) × width
   *   y_svg = height − ((y − y_min) / (y_max − y_min)) × height
   *
   * The y inversion places higher values at the top of the svg
   * (standard chart convention). Degenerate cases (single point,
   * all-same-value, all-same-time) collapse safely:
   *   - single x: collapse to x = width/2
   *   - single y: collapse to y = height/2
   * No NaN or division-by-zero leaks into the rendered path.
   */
  let mapped = $derived.by(() => {
    if (points.length === 0) return [];
    // Session 68 — replaced spread-based Math.min(...xs) / Math.max(...xs)
    // with a single reduce loop. The spread variant blows the JS engine's
    // call-arg ceiling around 64K-100K args; the reduce loop is O(N) with
    // no arg-list ceiling and one pass over points instead of four.
    let xMin = Infinity, xMax = -Infinity, yMin = Infinity, yMax = -Infinity;
    for (const p of points) {
      if (p.x < xMin) xMin = p.x;
      if (p.x > xMax) xMax = p.x;
      if (p.y < yMin) yMin = p.y;
      if (p.y > yMax) yMax = p.y;
    }
    const xRange = xMax - xMin;
    const yRange = yMax - yMin;
    return points.map((p) => {
      const x = xRange === 0 ? width / 2 : ((p.x - xMin) / xRange) * width;
      const y = yRange === 0 ? height / 2 : height - ((p.y - yMin) / yRange) * height;
      return { x, y };
    });
  });

  let polyline = $derived(
    mapped.map((p) => `${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(' '),
  );
  let last = $derived(mapped.length > 0 ? mapped[mapped.length - 1] : null);
  let isDegenerate = $derived(mapped.length < 2);
</script>

{#if points.length === 0}
  <!-- Empty input renders nothing — the parent decides what to say. -->
{:else}
  <svg
    class="sparkline"
    viewBox="0 0 {width} {height}"
    preserveAspectRatio="none"
    aria-hidden="true"
  >
    {#if !isDegenerate}
      <polyline
        points={polyline}
        fill="none"
        stroke={color}
        stroke-width="1.25"
        stroke-linejoin="round"
        stroke-linecap="round"
        vector-effect="non-scaling-stroke"
      />
    {/if}
    {#if last}
      <circle
        cx={last.x}
        cy={last.y}
        r="1.75"
        fill={color}
      />
    {/if}
  </svg>
{/if}

<style>
  .sparkline {
    display: block;
    width: 100%;
    height: 100%;
  }
</style>
