/*
  document_chart — chart-shape detector for Document bodies
  (Session 83 refactor; logic extracted from RecordsDashboard.svelte
  where it shipped in Session 69 and absorbed truncation-recovery in
  Session 71).

  Why the extraction
  ------------------
  Session 83 generalises the detector to surface the *catalog* of
  numeric series found in the body, not just the single best-ranked
  one. The KindCard tile keeps its single-series preview via
  `detectPreview(body)` (signature-compatible with the pre-Session-83
  `documentSeriesOf` helper); the expanded `DocumentDrawer` consumes
  the full catalog via `detectChartCatalog(body)` so the operator can
  pick which metric to plot from a dropdown.

  Closed-vocabulary discipline (project_sr_no_source_routing)
  -----------------------------------------------------------
  The detector matches on structural shape (a timestamp array plus
  same-length numeric arrays) and only uses *generic* field names —
  `close`, `price`, `open`, `high`, `low`, `volume`, etc — for
  ranking. There are no Yahoo / FRED / NHC strings in this module; a
  new feed surface that happens to declare `temperature: [..]` and
  `timestamp: [..]` would Just Work. Yahoo's `adjclose` and any other
  non-listed key fall into the "other" tier (rank = last).

  Truncation recovery
  -------------------
  `recoverTruncatedJson` is the same Session 71 routine the dashboard
  used to ship inline; preserved verbatim so feeds that overrun
  `STRUCTURED_BODY_CAP_BYTES` still render a chart.
*/

export interface ChartSeries {
  /** Lowercased field name found in the JSON (e.g. "close", "volume"). */
  key: string;
  /** Numeric values, same length as the catalog's timestamps. */
  values: number[];
}

export interface ChartCatalog {
  /** x-axis values, length ≥ 2, all finite numbers. */
  timestamps: number[];
  /** Numeric series of matching length, ranked primary-first
   *  (close, price, …) then secondary (open, high, …) then other. */
  series: ChartSeries[];
  /** Friendly label drawn from common identity keys (symbol >
   *  longName > name). Empty string when none was found. */
  label: string;
}

export interface ChartPreview {
  points: Array<{ x: number; y: number }>;
  label: string;
  valueKey: string;
}

export const PRIMARY_SERIES_KEYS = [
  'close', 'price', 'value', 'rate', 'yield', 'level',
];
export const SECONDARY_SERIES_KEYS = [
  'open', 'high', 'low', 'volume', 'count',
];
export const LABEL_KEYS = [
  'symbol', 'longname', 'shortname', 'name', 'series_id', 'id',
];

/** Maximum points fed to the sparkline / chart polyline. Strided
 *  uniformly when N exceeds this. 500 is plenty for both the 120-px
 *  tile and the ~700-px drawer-width chart. */
export const PREVIEW_POINT_CAP = 500;

export function rankKey(key: string): number {
  const i1 = PRIMARY_SERIES_KEYS.indexOf(key);
  if (i1 >= 0) return i1;
  const i2 = SECONDARY_SERIES_KEYS.indexOf(key);
  if (i2 >= 0) return PRIMARY_SERIES_KEYS.length + i2;
  return PRIMARY_SERIES_KEYS.length + SECONDARY_SERIES_KEYS.length + 1;
}

function isAllNumeric(arr: unknown[]): boolean {
  if (arr.length < 2) return false;
  for (const v of arr) {
    if (typeof v !== 'number' || !Number.isFinite(v)) return false;
  }
  return true;
}

/**
 * Best-effort recovery of a truncated JSON string (Session 71).
 *
 * Walks left-to-right tracking open-brace / open-bracket depth
 * (ignoring string contents — quotes toggle an "in-string" flag and
 * escapes are skipped). At each top-level structural close (`]` or
 * `}`) we record a "safe truncation point" — the index just after
 * that close — and snapshot the closing tail we'd need at that
 * depth.
 *
 * After the walk we slice at the last recorded safe point and
 * append the closing tail. For Yahoo-shaped feeds this typically
 * lands inside `chart.result[0]` after a complete inner array, then
 * closes the remaining `}]}`.
 *
 * Returns the repaired string, or `null` if no safe point exists.
 */
export function recoverTruncatedJson(input: string): string | null {
  let lastSafeEnd = 0;
  let lastSafeTail = '';
  const stack: string[] = [];
  let inString = false;
  let escape = false;
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (escape) { escape = false; continue; }
    if (inString) {
      if (c === '\\') escape = true;
      else if (c === '"') inString = false;
      continue;
    }
    if (c === '"') { inString = true; continue; }
    if (c === '{') {
      stack.push('}');
    } else if (c === '[') {
      stack.push(']');
    } else if (c === '}' || c === ']') {
      const expected = stack[stack.length - 1];
      if (expected !== c) return null;
      stack.pop();
      lastSafeEnd = i + 1;
      lastSafeTail = stack.slice().reverse().join('');
      if (stack.length === 0) return null;
    }
  }
  if (lastSafeEnd === 0) return null;
  return input.slice(0, lastSafeEnd) + lastSafeTail;
}

function parseOrRecover(trimmed: string): unknown | null {
  try {
    return JSON.parse(trimmed);
  } catch {
    const recovered = recoverTruncatedJson(trimmed);
    if (recovered === null) return null;
    try {
      return JSON.parse(recovered);
    } catch {
      return null;
    }
  }
}

/**
 * Walk the parsed body, collect every same-length numeric series
 * paired with the first-seen timestamp array, and return the ranked
 * catalog. Returns `null` when no time-series shape is found.
 */
export function detectChartCatalog(body: string): ChartCatalog | null {
  if (typeof body !== 'string' || body.length === 0) return null;
  const trimmed = body.trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return null;
  const parsed = parseOrRecover(trimmed);
  if (parsed === null) return null;

  const timestampArrays: number[][] = [];
  const valueCandidates: ChartSeries[] = [];
  const labelCandidates: { key: string; value: string }[] = [];

  function walk(node: unknown, key: string | null): void {
    if (Array.isArray(node)) {
      if (isAllNumeric(node)) {
        if (key !== null) {
          const lc = key.toLowerCase();
          if (lc === 'timestamp' || lc === 'timestamps') {
            timestampArrays.push(node as number[]);
          } else {
            valueCandidates.push({ key: lc, values: node as number[] });
          }
        } else {
          // Top-level numeric array — treat as a value candidate
          // with an empty key (lowest priority).
          valueCandidates.push({ key: '', values: node as number[] });
        }
      } else {
        for (const item of node) walk(item, null);
      }
      return;
    }
    if (node !== null && typeof node === 'object') {
      for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
        if (typeof v === 'string' && LABEL_KEYS.includes(k.toLowerCase())) {
          labelCandidates.push({ key: k.toLowerCase(), value: v });
        }
        walk(v, k);
      }
    }
  }
  walk(parsed, null);

  if (timestampArrays.length === 0 || valueCandidates.length === 0) {
    return null;
  }

  // Pair with the first timestamp array — most feeds have one
  // timestamp array per result; rare multi-result payloads use the
  // first one as the primary series anchor. See Session 69 notes.
  const timestamps = timestampArrays[0];
  const sameLength = valueCandidates.filter(
    (c) => c.values.length === timestamps.length,
  );
  if (sameLength.length === 0) return null;

  // Rank: primary keys > secondary > other. Within a tier, first-
  // seen wins (Array#sort is stable on engines we ship for).
  sameLength.sort((a, b) => rankKey(a.key) - rankKey(b.key));

  let label = '';
  for (const lk of LABEL_KEYS) {
    const hit = labelCandidates.find((l) => l.key === lk);
    if (hit) { label = hit.value; break; }
  }

  return { timestamps, series: sameLength, label };
}

/**
 * Uniformly-strided points for an N-length series, capped at
 * `cap` points. Always preserves the last sample so the rightmost
 * point in the chart matches the rightmost sample in the data.
 */
export function stridedPoints(
  timestamps: number[],
  values: number[],
  cap = PREVIEW_POINT_CAP,
): Array<{ x: number; y: number }> {
  const n = Math.min(timestamps.length, values.length);
  if (n === 0) return [];
  const stride = n > cap ? Math.ceil(n / cap) : 1;
  const out: Array<{ x: number; y: number }> = [];
  for (let i = 0; i < n; i += stride) {
    out.push({ x: timestamps[i], y: values[i] });
  }
  if (out.length > 0 && out[out.length - 1].x !== timestamps[n - 1]) {
    out.push({ x: timestamps[n - 1], y: values[n - 1] });
  }
  return out;
}

/**
 * Pick the catalog's first (ranked-highest) series and return it
 * in `MiniSparkline`-ready form. Returns `null` for empty catalogs.
 */
export function pickPreviewSeries(catalog: ChartCatalog): ChartPreview | null {
  if (catalog.series.length === 0) return null;
  const chosen = catalog.series[0];
  const points = stridedPoints(catalog.timestamps, chosen.values);
  return { points, label: catalog.label, valueKey: chosen.key };
}

/**
 * Convenience wrapper for the KindCard tile path: parse body → pick
 * one preview series. Signature-compatible with the pre-Session-83
 * `documentSeriesOf` helper. Returns `null` when no time-series
 * shape is detectable.
 */
export function detectPreview(body: string): ChartPreview | null {
  const catalog = detectChartCatalog(body);
  if (catalog === null) return null;
  return pickPreviewSeries(catalog);
}

// ---- Hover helpers (Session 83) ------------------------------------

/**
 * Nearest-x lookup for a chart-hover crosshair. Assumes timestamps
 * are monotonic non-decreasing (the only shape detectChartCatalog
 * surfaces in practice — JSON feeds emit timestamps in order).
 *
 * Returns the index of the nearest sample to `target`. For an
 * empty array returns -1.
 */
export function nearestIndex(timestamps: number[], target: number): number {
  const n = timestamps.length;
  if (n === 0) return -1;
  if (n === 1) return 0;
  let lo = 0;
  let hi = n - 1;
  if (target <= timestamps[lo]) return lo;
  if (target >= timestamps[hi]) return hi;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (timestamps[mid] < target) lo = mid;
    else hi = mid;
  }
  return Math.abs(timestamps[lo] - target) <= Math.abs(timestamps[hi] - target)
    ? lo
    : hi;
}

/**
 * Format a chart x-value as a date/time string for the tooltip.
 *
 * Heuristic for unit:
 *   - x < 1e12 → seconds (Yahoo-shaped feeds)
 *   - x >= 1e12 → milliseconds (JS Date convention)
 *
 * The decision boundary is cleanly separable: anything after year
 * 2001 in seconds is above 1e9; anything before year 2286 in ms
 * is below 1e12.
 *
 * Granularity depends on the chart's x-span:
 *   - span < 30 days → include HH:MM (intraday data)
 *   - span >= 30 days → date only (daily / weekly bars)
 */
export function formatChartTimestamp(x: number, spanInSameUnits: number): string {
  const isSeconds = Math.abs(x) < 1e12;
  const ms = isSeconds ? x * 1000 : x;
  const spanMs = isSeconds ? spanInSameUnits * 1000 : spanInSameUnits;
  const d = new Date(ms);
  if (Number.isNaN(d.valueOf())) return String(x);
  if (spanMs < 30 * 86_400_000) {
    return d.toISOString().replace('T', ' ').slice(0, 16) + ' UTC';
  }
  return d.toISOString().slice(0, 10);
}

/**
 * Format a numeric value for the tooltip and y-axis ticks.
 * Uses thousands separators for large numbers and two decimals for
 * non-integer fractions.
 */
export function formatChartValue(v: number): string {
  if (!Number.isFinite(v)) return String(v);
  if (Math.abs(v) >= 1e6) {
    return v.toLocaleString('en-US', { maximumFractionDigits: 0 });
  }
  if (Math.abs(v) >= 1000) {
    return v.toLocaleString('en-US', { maximumFractionDigits: 2 });
  }
  if (Number.isInteger(v)) return v.toString();
  return v.toFixed(2);
}
