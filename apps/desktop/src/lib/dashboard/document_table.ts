/*
  document_table — Session 73 table-shape detector for Document bodies.

  Counterpart to `detectTimeSeriesShape` in RecordsDashboard. Where
  that detector finds paired numeric arrays for sparkline rendering,
  this one finds the second most common JSON record shape: an array
  of homogeneous-ish plain objects.

  Examples that arrive in the wild:

    - OData feeds:      { "value": [ { ... }, { ... } ] }
    - FEMA open data:   { "DisasterDeclarationsSummaries": [ ... ] }
    - Bare arrays:      [ { ... }, { ... } ]
    - Generic items:    { "items": [ { ... } ] }

  Rules (closed-vocabulary, no host strings — see
  project_sr_no_source_routing):

    1. Body parses as JSON.
    2. Walk the tree collecting every array whose elements are all
       plain objects (`typeof === 'object'`, not Array, not null) and
       whose length >= MIN_ROWS.
    3. Pick the *largest* candidate by row count. Ties resolve by
       first-seen order in the walk so the shallower path wins (the
       outermost record list, when sibling lists tie).
    4. Compute the column union across rows, ordered by frequency
       (descending) then lexicographically. Most-populated columns
       appear first so the operator's eye lands on the dense part
       of the table.

  The DocumentDrawer calls this only when no time-series chart shape
  was detected. A payload that satisfies both detectors is rendered
  as a chart (higher information density per pixel for the eyeball);
  if the operator wants the raw rows they can fall back to the JSON
  view via the drawer's existing pretty-printed block.

  Out of scope (deferred):
    - Schema inference (datetime vs numeric vs categorical) beyond
      the binary "numeric for sort?" check. Generic enough for
      sort-by-column; not yet a basis for typed cell formatting.
    - Truncated-JSON recovery. The chart detector has a recovery
      path because chart payloads (Yahoo-shaped) overrun the cap on
      worst-case shapes; table-shaped payloads we've seen so far
      (FEMA, OData) sit well under the 128 KiB structured-text cap
      and we'd rather show no table than guess at a partial one.
*/

/**
 * Table-shape view of a Document body. Returned when the body parses
 * as JSON and contains at least one array of plain objects with
 * `>= MIN_ROWS` entries.
 */
export interface TableShape {
  /** Column names in display order (most-populated → least; ties
   *  alphabetical so the same payload always renders the same way). */
  columns: string[];
  /** Rows in input order. Each row may be missing keys — the
   *  caller must treat absent keys as `undefined`/null cells. */
  rows: Array<Record<string, unknown>>;
  /** Dotted-path where the array sits inside the parsed tree. `$`
   *  marks the document root (bare top-level array). Surfaced as a
   *  caption so the operator can tell which array is being viewed
   *  when a payload has multiple. */
  path: string;
}

/** Minimum number of rows to qualify as a "table." A single-row
 *  array is rendered fine by the existing pretty-printed JSON path;
 *  the table view's value-add starts at row 2 (comparison) and
 *  scales with row count. */
export const MIN_ROWS = 2;

/**
 * Walk a parsed JSON tree and return the largest array-of-plain-
 * objects, or `null` when none qualifies. Caller should pass the
 * raw body string; we handle parse failures internally and return
 * `null` rather than throwing so the drawer's render path stays
 * straight-line.
 */
export function detectTableShape(body: string): TableShape | null {
  const trimmed = body.trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    // Unlike the chart detector we don't attempt truncation recovery
    // — see the file-level comment for the rationale.
    return null;
  }

  // Walk collects {path, rows} candidates without recursing inside a
  // qualifying array (the array's element shape is the leaf for that
  // candidate).
  interface Candidate {
    path: string;
    rows: Array<Record<string, unknown>>;
  }
  const candidates: Candidate[] = [];

  function isPlainObjectArray(arr: unknown[]): arr is Array<Record<string, unknown>> {
    if (arr.length < MIN_ROWS) return false;
    for (const el of arr) {
      if (el === null || typeof el !== 'object' || Array.isArray(el)) return false;
    }
    return true;
  }

  function walk(node: unknown, path: string): void {
    if (Array.isArray(node)) {
      if (isPlainObjectArray(node)) {
        candidates.push({ path: path || '$', rows: node });
        return;
      }
      // Mixed-type array — keep walking inner items so nested
      // object-arrays still get a chance.
      for (let i = 0; i < node.length; i++) {
        walk(node[i], `${path}[${i}]`);
      }
      return;
    }
    if (node !== null && typeof node === 'object') {
      for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
        walk(v, path ? `${path}.${k}` : k);
      }
    }
  }
  walk(parsed, '');

  if (candidates.length === 0) return null;

  // Largest first; preserve first-seen on ties (Array.sort is stable
  // in V8, which is what Tauri's WebView runs on).
  candidates.sort((a, b) => b.rows.length - a.rows.length);
  const best = candidates[0];

  // Column union by occurrence count. Frequency-ordered display
  // puts dense columns first; alphabetical tiebreak keeps render
  // deterministic for the same payload across reloads.
  const counts = new Map<string, number>();
  for (const row of best.rows) {
    for (const k of Object.keys(row)) {
      counts.set(k, (counts.get(k) ?? 0) + 1);
    }
  }
  if (counts.size === 0) return null;
  const columns = Array.from(counts.entries())
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([k]) => k);

  return { columns, rows: best.rows, path: best.path };
}

// ---------------------------------------------------------------
// Cell rendering + per-column sort helpers
// ---------------------------------------------------------------

/**
 * Render a cell value as a short string for inline display.
 *
 *   - `null` / `undefined` → empty string (caller decides whether to
 *     show a dimmed `—` placeholder).
 *   - primitive → its literal form.
 *   - object / array → `JSON.stringify` clamped at `cap` chars with
 *     an ellipsis. Caps the row height so a single huge nested cell
 *     can't blow up the table.
 *
 * The default cap (80 chars) is generous enough to read short
 * nested values inline; the drawer's row-expand affordance (future
 * session) would let the operator dig into a single cell.
 */
export function renderCell(v: unknown, cap = 80): string {
  if (v === null || v === undefined) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  let s: string;
  try {
    s = JSON.stringify(v);
  } catch {
    s = String(v);
  }
  if (s.length <= cap) return s;
  // Use an ellipsis (U+2026) inside the cap so the total stays
  // under `cap` characters — important for the cell-width math.
  return s.slice(0, cap - 1) + '…';
}

/**
 * Whether a column should sort numerically. Returns true when
 * *every* non-null value in the column parses as a finite number
 * (numeric literal or string coercible via `Number()`). Empty /
 * all-null columns are treated as string-sortable (returns false).
 *
 * Mixed-type columns (some numeric, some non-coercible strings)
 * return false so the sort stays well-defined — better to sort
 * mixed columns lexicographically than to silently coerce
 * "above-threshold" strings to NaN and stash them at the end.
 */
export function isNumericColumn(
  rows: Array<Record<string, unknown>>,
  col: string,
): boolean {
  let seenAny = false;
  for (const r of rows) {
    const v = r[col];
    if (v === null || v === undefined) continue;
    seenAny = true;
    if (typeof v === 'number' && Number.isFinite(v)) continue;
    if (typeof v === 'string' && v.length > 0) {
      const n = Number(v);
      if (Number.isFinite(n)) continue;
      return false;
    }
    return false;
  }
  return seenAny;
}

/** Three-state column sort. `null` means "original input order." */
export type SortDirection = 'asc' | 'desc' | null;

/** Click cycle: none → asc → desc → none. */
export function nextSortDirection(current: SortDirection): SortDirection {
  if (current === null) return 'asc';
  if (current === 'asc') return 'desc';
  return null;
}

/**
 * Compare two cell values for sort. Numeric columns compare as
 * numbers; string columns lexicographically (`localeCompare`).
 * `null`/`undefined` cells *always* sort to the end regardless of
 * direction — the operator usually wants to see data rows first,
 * not a wall of empties.
 */
export function compareCells(a: unknown, b: unknown, numeric: boolean): number {
  const aMissing = a === null || a === undefined;
  const bMissing = b === null || b === undefined;
  if (aMissing && bMissing) return 0;
  if (aMissing) return 1;
  if (bMissing) return -1;
  if (numeric) {
    const na = typeof a === 'number' ? a : Number(a);
    const nb = typeof b === 'number' ? b : Number(b);
    if (Number.isNaN(na) && Number.isNaN(nb)) return 0;
    if (Number.isNaN(na)) return 1;
    if (Number.isNaN(nb)) return -1;
    return na - nb;
  }
  const sa = typeof a === 'string' ? a : JSON.stringify(a) ?? '';
  const sb = typeof b === 'string' ? b : JSON.stringify(b) ?? '';
  return sa.localeCompare(sb);
}
