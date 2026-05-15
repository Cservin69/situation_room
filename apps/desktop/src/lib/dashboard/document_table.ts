/*
  document_table — Session 73 table-shape detector for Document bodies,
  extended Session 74.5 to also recognise object-of-scalars (JSON-stat).

  Counterpart to `detectTimeSeriesShape` in RecordsDashboard. Where
  that detector finds paired numeric arrays for sparkline rendering,
  this one finds two second-tier JSON record shapes:

    a. Array of homogeneous-ish plain objects (Session 73). Examples
       in the wild:

         - OData feeds:      { "value": [ { ... }, { ... } ] }
         - FEMA open data:   { "DisasterDeclarationsSummaries": [ ... ] }
         - Bare arrays:      [ { ... }, { ... } ]
         - Generic items:    { "items": [ { ... } ] }

    b. Object of scalars (Session 74.5). A `{ key: scalar }` map with
       ≥ MIN_ROWS entries, where every value is `string | number |
       boolean | null`. Examples in the wild:

         - JSON-stat 'value' block:
             { "version": "2.0",
               "value": { "162": 4499.8, "163": 4516.1, ... } }
           (Eurostat APRO_CPSH1 and friends; positional indices into a
           sparse multi-dimensional cube. The dimension labels live in
           a sibling `dimension` block this detector deliberately does
           not decode — that's a JSON-stat-specific renderer's job, not
           a generic detector's.)
         - Lookup maps:   { "USD": 1.0, "EUR": 0.92, "GBP": 0.79 }

       The shape renders as a 2-column "Key | Value" virtual table:
       row count = entry count, columns = ['Key', 'Value']. Sort
       works the same as array-of-objects because `isNumericColumn`
       already handles numeric-string keys, so numeric indices like
       "162" sort correctly.

  Rules (closed-vocabulary, no host strings — see
  project_sr_no_source_routing):

    1. Body parses as JSON.
    2. Walk the tree:
         - At each array: if every element is a plain object and
           length >= MIN_ROWS, register as an `array_of_objects`
           candidate and stop recursing into it.
         - At each object: if every value is a scalar
           (`string | number | boolean | null`) and entry count
           >= MIN_ROWS, register as an `object_of_scalars` candidate
           and stop recursing into it. Otherwise recurse into the
           values.
    3. Resolve to a single best candidate. `array_of_objects` always
       beats `object_of_scalars` (more semantic — actual records vs
       a key-value lookup); within each kind, largest row count
       wins, with stable first-seen on ties.
    4. Columns:
         - `array_of_objects`: union across rows, ordered by
           frequency (descending) then lexicographically.
         - `object_of_scalars`: synthesised as `['Key', 'Value']`,
           rows synthesised as `[{Key, Value}, ...]` so the
           DocumentTable.svelte rendering path is shape-uniform.

  The DocumentDrawer calls this only when no time-series chart shape
  was detected. A payload that satisfies both detectors is rendered
  as a chart (higher information density per pixel for the eyeball);
  if the operator wants the raw rows they can fall back to the JSON
  view via the drawer's existing pretty-printed block.

  Truncation recovery: when JSON.parse fails (the body was clipped
  at the structured-body cap mid-value), we re-run the chart
  detector's `recoverTruncatedJson` algorithm — walk left-to-right
  tracking brace/bracket depth, slice at the last balanced close,
  append the missing closing punctuation. The recovered prefix is
  guaranteed to be parseable; rows after the truncation point are
  lost, but the operator sees a partial table instead of a wall of
  raw JSON. Session 74.5 added this after a Eurostat APRO_CPSH1
  body hit the 128 KiB cap and broke the detector's happy path.

  Out of scope (deferred):
    - JSON-stat dimension decoding. Decoding "162" → ("HU", "C1110",
      "AR", "2024") via the sibling `dimension` block would turn the
      2-column "Key | Value" table into a true multi-column data
      table. That's host-class-specific structure beyond this
      detector's closed vocabulary.
    - Schema inference (datetime vs numeric vs categorical) beyond
      the binary "numeric for sort?" check. Generic enough for
      sort-by-column; not yet a basis for typed cell formatting.
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
 * Walk a parsed JSON tree and return the best table candidate, or
 * `null` when none qualifies. Caller should pass the raw body
 * string; we handle parse failures internally and return `null`
 * rather than throwing so the drawer's render path stays straight-
 * line.
 *
 * Returns the largest `array_of_objects` candidate when present.
 * Falls back to the largest `object_of_scalars` candidate (JSON-stat
 * and similar key-value lookup maps) when no array-of-objects is
 * available. See the file-level comment for the rationale on the
 * preference order.
 */
export function detectTableShape(body: string): TableShape | null {
  const trimmed = body.trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (e) {
    // Session 74.5: best-effort recovery for truncated JSON. Same
    // algorithm the chart detector uses (RecordsDashboard.svelte) so
    // a body clipped at the 128 KiB structured-text cap still
    // surfaces a partial table instead of dropping to raw `<pre>`.
    // Eurostat APRO_CPSH1 was the motivating shape — JSON-stat
    // responses can carry hundreds of `{key: number}` entries and
    // tip over the cap on countries with long dimension histories.
    const recovered = recoverTruncatedJson(trimmed);
    if (recovered === null) {
      // eslint-disable-next-line no-console
      console.warn(
        'situation_room: table-shape detection failed — JSON.parse threw and no recovery point found. ' +
          'Body length=' + trimmed.length + ', first error=' + String(e),
      );
      return null;
    }
    try {
      parsed = JSON.parse(recovered);
      // eslint-disable-next-line no-console
      console.warn(
        'situation_room: table-shape detection recovered truncated JSON ' +
          '(body was over the preview cap; consider raising STRUCTURED_BODY_CAP_BYTES). ' +
          'Original length=' + trimmed.length + ', recovered length=' + recovered.length,
      );
    } catch (e2) {
      // eslint-disable-next-line no-console
      console.warn(
        'situation_room: table-shape detection failed — recovery also unparseable. ' +
          'Body length=' + trimmed.length + ', second error=' + String(e2),
      );
      return null;
    }
  }

  // Walk collects two distinct candidate kinds. The kind tag is the
  // load-bearing field: array_of_objects always beats object_of_scalars
  // regardless of row count (more semantic).
  type Kind = 'array_of_objects' | 'object_of_scalars';
  interface Candidate {
    kind: Kind;
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

  function isScalar(v: unknown): boolean {
    return (
      v === null ||
      typeof v === 'string' ||
      typeof v === 'number' ||
      typeof v === 'boolean'
    );
  }

  function isObjectOfScalars(obj: Record<string, unknown>): boolean {
    const entries = Object.entries(obj);
    if (entries.length < MIN_ROWS) return false;
    for (const [, v] of entries) {
      if (!isScalar(v)) return false;
    }
    return true;
  }

  function walk(node: unknown, path: string): void {
    if (Array.isArray(node)) {
      if (isPlainObjectArray(node)) {
        candidates.push({
          kind: 'array_of_objects',
          path: path || '$',
          rows: node,
        });
        return;
      }
      // Mixed-type array — keep walking inner items so nested
      // object-arrays / object-of-scalars maps still get a chance.
      for (let i = 0; i < node.length; i++) {
        walk(node[i], `${path}[${i}]`);
      }
      return;
    }
    if (node !== null && typeof node === 'object') {
      const obj = node as Record<string, unknown>;
      if (isObjectOfScalars(obj)) {
        // Synthesise a 2-column virtual table so the renderer path
        // stays shape-uniform with array_of_objects. `Key` carries
        // the original key (string); `Value` carries the scalar
        // value verbatim so numeric values stay numeric for sort.
        const rows = Object.entries(obj).map(([k, v]) => ({
          Key: k,
          Value: v,
        }));
        candidates.push({
          kind: 'object_of_scalars',
          path: path || '$',
          rows,
        });
        return;
      }
      for (const [k, v] of Object.entries(obj)) {
        walk(v, path ? `${path}.${k}` : k);
      }
    }
  }
  walk(parsed, '');

  if (candidates.length === 0) return null;

  // Kind preference first (array_of_objects > object_of_scalars),
  // then row count desc, with stable first-seen on ties (Array.sort
  // is stable in V8, which is what Tauri's WebView runs on). Manual
  // kind ordering rather than reaching for `Intl.Collator` etc.
  const kindRank = (k: Kind): number => (k === 'array_of_objects' ? 0 : 1);
  candidates.sort((a, b) => {
    const dk = kindRank(a.kind) - kindRank(b.kind);
    if (dk !== 0) return dk;
    return b.rows.length - a.rows.length;
  });
  const best = candidates[0];

  if (best.kind === 'object_of_scalars') {
    // Synthesised shape always has the same column header. No
    // frequency count needed — every "row" has exactly Key + Value.
    return { columns: ['Key', 'Value'], rows: best.rows, path: best.path };
  }

  // Column union by occurrence count for array_of_objects.
  // Frequency-ordered display puts dense columns first; alphabetical
  // tiebreak keeps render deterministic for the same payload across
  // reloads.
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
// Truncated-JSON recovery (Session 74.5)
// ---------------------------------------------------------------

/**
 * Best-effort recovery of a truncated JSON string. Mirrors the chart
 * detector's helper in `RecordsDashboard.svelte` so both surfaces
 * react the same way to bodies clipped at the 128 KiB structured-
 * text cap.
 *
 * Walks the string left-to-right tracking open-brace / open-bracket
 * depth (ignoring string contents — quotes toggle a "in-string" flag
 * and escapes are skipped). At each structural close (`]` or `}`) we
 * record a "safe truncation point" — the index just after that
 * close, plus the closing-tail string we'd need to balance the
 * still-open scopes.
 *
 * After the walk we slice at the last recorded safe point and append
 * the snapshot's closing tail. The result is a syntactically valid
 * JSON document that re-parses cleanly — though with whatever entries
 * came after the truncation point dropped.
 *
 * Returns `null` when:
 *   - no balanced close was ever seen (the input was truncated
 *     before the first inner object/array closed),
 *   - the input is malformed beyond truncation (mismatched
 *     brackets), or
 *   - the input was already complete (depth returned to zero — the
 *     caller would have parsed directly; the recovery path isn't
 *     applicable).
 *
 * This is a literal port of the algorithm in RecordsDashboard.svelte
 * (Session 71). Duplication-with-comments rather than a shared module
 * import: the two surfaces live in different layers (a `.svelte`
 * component vs a `.ts` library) and the algorithm is short, stable,
 * and easy to keep in lockstep by inspection.
 */
function recoverTruncatedJson(input: string): string | null {
  let lastSafeEnd = 0;
  let lastSafeTail = '';
  const stack: string[] = [];
  let inString = false;
  let escape = false;
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (escape) {
      escape = false;
      continue;
    }
    if (inString) {
      if (c === '\\') {
        escape = true;
      } else if (c === '"') {
        inString = false;
      }
      continue;
    }
    if (c === '"') {
      inString = true;
      continue;
    }
    if (c === '{') {
      stack.push('}');
    } else if (c === '[') {
      stack.push(']');
    } else if (c === '}' || c === ']') {
      const expected = stack[stack.length - 1];
      if (expected !== c) {
        // Malformed input — bail; recovery only handles the
        // truncation case.
        return null;
      }
      stack.pop();
      lastSafeEnd = i + 1;
      lastSafeTail = stack.slice().reverse().join('');
      if (stack.length === 0) {
        // Body is already complete — caller would have parsed
        // directly; nothing for us to do.
        return null;
      }
    }
  }
  if (lastSafeEnd === 0) return null;
  return input.slice(0, lastSafeEnd) + lastSafeTail;
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
