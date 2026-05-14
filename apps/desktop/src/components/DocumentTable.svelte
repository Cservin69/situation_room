<!--
  DocumentTable — inline tabular view of a Document body when its
  JSON payload is shaped as an array of plain objects (Session 73).

  ## What this answers

  Pre-Session-73 the drawer rendered every JSON payload as a single
  pretty-printed `<pre>` block. That's fine for a small object; it's
  awful for "FEMA returned 500 disaster declarations" — the operator
  has to eyeball-scan thousands of lines to find the records of
  interest, and there's no column comparison.

  This component renders the detected `TableShape` (see
  `$lib/dashboard/document_table.ts`) as a sortable HTML table.
  Columns are click-to-sort (asc → desc → none), rows are capped at
  `INITIAL_ROWS` with a show-all toggle, and columns are capped at
  `INITIAL_COLUMNS` with a show-all toggle. The raw-JSON `<pre>`
  fallback stays available in the drawer underneath this component
  so power users can drop into the literal payload when the table
  view abstracts away something they need.

  ## Why a native Svelte component, not Grid.js

  Session 72 handoff suggested Grid.js. Adding a runtime dep for
  sort + cap + cell-render — three small functions we already have
  in `document_table.ts` — would buy us features (search box,
  pagination) that don't carry weight on a 32-128 KiB body. The
  drawer's job is "is this what I fetched?", not "let me explore a
  dataset" — exploration belongs to a future records-explorer
  surface, not the inspection modal. So we stay on the native
  control and keep the bundle size where it was.

  ## What this component does NOT do

  - **No filtering / search.** Add one if/when a payload exceeds
    the row cap by enough to make scrolling-to-find a chore. Today
    show-all + browser Cmd-F covers it.
  - **No CSV export.** Document body is already on disk; export
    belongs to a records-export surface, not the drawer.
  - **No per-cell drilldown.** Nested objects render as truncated
    JSON literals. A click-to-expand affordance is a later session
    if the operator hits a case where the truncation matters.
-->
<script lang="ts">
  import {
    renderCell,
    isNumericColumn,
    nextSortDirection,
    compareCells,
    type TableShape,
    type SortDirection,
  } from '$lib/dashboard/document_table';

  interface Props {
    /** The detected table shape from `detectTableShape`. Required;
     *  the parent only mounts this component when detection returns
     *  non-null. */
    table: TableShape;
  }
  let { table }: Props = $props();

  // -- column cap --------------------------------------------------
  //
  // FEMA-shaped payloads can carry 30+ keys per row. Showing all at
  // once forces the operator into horizontal scroll inside an
  // already-modal surface. We cap at INITIAL_COLUMNS by default —
  // since the column list is frequency-ordered, the cap surfaces
  // the dense columns first. A "show all" toggle reveals the long
  // tail when needed.
  const INITIAL_COLUMNS = 8;
  let columnsExpanded = $state(false);
  let visibleColumns = $derived(
    columnsExpanded || table.columns.length <= INITIAL_COLUMNS
      ? table.columns
      : table.columns.slice(0, INITIAL_COLUMNS),
  );
  let hiddenColumnCount = $derived(table.columns.length - visibleColumns.length);

  // -- row cap -----------------------------------------------------
  //
  // 50 is the sweet spot empirically: above ~50 a `<table>` starts
  // to feel slow to scroll-render in the WebView and the operator
  // is reaching for Cmd-F anyway. Toggle expands to the full set.
  const INITIAL_ROWS = 50;
  let rowsExpanded = $state(false);

  // -- sort state --------------------------------------------------

  let sortColumn = $state<string | null>(null);
  let sortDir = $state<SortDirection>(null);

  /**
   * Pre-compute "is this column numeric" once per (rows, columns)
   * pair so we don't reclassify on every sort tick. The map is
   * derived from `table` so it refreshes when the prop changes
   * (e.g. operator clicks a different Document KindCard).
   */
  let numericByColumn = $derived.by(() => {
    const m = new Map<string, boolean>();
    for (const c of table.columns) m.set(c, isNumericColumn(table.rows, c));
    return m;
  });

  function onHeaderClick(col: string) {
    if (sortColumn === col) {
      const next = nextSortDirection(sortDir);
      sortDir = next;
      if (next === null) sortColumn = null;
    } else {
      sortColumn = col;
      sortDir = 'asc';
    }
  }

  /**
   * Final rows after sort + row cap. The sort is non-destructive
   * (we slice first, sort on the copy) so toggling sort off
   * restores the original input order — useful when a payload's
   * own order carries meaning the operator hasn't explicitly asked
   * the table to obscure.
   */
  let displayRows = $derived.by(() => {
    let rows: Array<Record<string, unknown>>;
    if (sortColumn !== null && sortDir !== null) {
      const numeric = numericByColumn.get(sortColumn) ?? false;
      const col = sortColumn;
      rows = table.rows.slice().sort((a, b) => {
        const cmp = compareCells(a[col], b[col], numeric);
        return sortDir === 'asc' ? cmp : -cmp;
      });
    } else {
      rows = table.rows;
    }
    return rowsExpanded || rows.length <= INITIAL_ROWS
      ? rows
      : rows.slice(0, INITIAL_ROWS);
  });
  let hiddenRowCount = $derived(table.rows.length - displayRows.length);

  /**
   * Header sort marker. `▲` = ascending, `▼` = descending, blank
   * when this column isn't the current sort. Keep the marker tiny
   * so the column name stays the dominant label.
   */
  function sortMarker(col: string): string {
    if (sortColumn !== col) return '';
    if (sortDir === 'asc') return ' ▲';
    if (sortDir === 'desc') return ' ▼';
    return '';
  }
</script>

<div class="doc-table-wrap">
  <div class="caption">
    <span class="path" title="path inside JSON tree">{table.path}</span>
    <span class="counts">
      {table.rows.length} row{table.rows.length === 1 ? '' : 's'}
      · {table.columns.length} col{table.columns.length === 1 ? '' : 's'}
    </span>
    {#if table.columns.length > INITIAL_COLUMNS}
      <button
        type="button"
        class="toggle"
        onclick={() => (columnsExpanded = !columnsExpanded)}
      >
        {columnsExpanded
          ? 'show first ' + INITIAL_COLUMNS + ' cols'
          : '+' + hiddenColumnCount + ' more cols'}
      </button>
    {/if}
  </div>

  <div class="table-scroll">
    <table>
      <thead>
        <tr>
          {#each visibleColumns as col (col)}
            <th
              class:numeric={numericByColumn.get(col)}
              class:active={sortColumn === col}
            >
              <button
                type="button"
                class="header-button"
                onclick={() => onHeaderClick(col)}
                title={`sort by ${col}`}
              >
                {col}{sortMarker(col)}
              </button>
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each displayRows as row, i (i)}
          <tr>
            {#each visibleColumns as col (col)}
              {@const v = row[col]}
              <td
                class:numeric={numericByColumn.get(col)}
                class:empty={v === null || v === undefined}
                title={v === undefined ? '' : renderCell(v, 200)}
              >
                {#if v === null || v === undefined}
                  <span class="dim">—</span>
                {:else}
                  {renderCell(v)}
                {/if}
              </td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  {#if hiddenRowCount > 0 || (rowsExpanded && table.rows.length > INITIAL_ROWS)}
    <div class="row-toggle">
      <button
        type="button"
        class="toggle"
        onclick={() => (rowsExpanded = !rowsExpanded)}
      >
        {rowsExpanded
          ? 'show first ' + INITIAL_ROWS + ' rows'
          : '+' + hiddenRowCount + ' more rows'}
      </button>
    </div>
  {/if}
</div>

<style>
  .doc-table-wrap {
    display: flex;
    flex-direction: column;
    gap: 6px;
    /* Bottom border so the operator sees where the table stops and
       the raw-JSON fallback below begins. */
    padding-bottom: 10px;
    border-bottom: 1px solid var(--border-subtle);
  }

  .caption {
    display: flex;
    align-items: baseline;
    gap: 10px;
    font-size: 11px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.02em;
  }
  .path {
    font-family: var(--font-mono);
    color: var(--fg-secondary);
  }
  .counts {
    color: var(--fg-tertiary);
  }
  .toggle {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 2px 6px;
    cursor: pointer;
    margin-left: auto;
  }
  .toggle:hover,
  .toggle:focus-visible {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  /* The table can be wider than the drawer; allow horizontal scroll
     inside this region so the modal frame stays put. */
  .table-scroll {
    overflow-x: auto;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    background: var(--bg-panel-alt);
  }
  table {
    border-collapse: collapse;
    font-size: 12px;
    width: 100%;
    /* `table-layout: auto` lets long string cells expand naturally
       up to the column's max content; combined with the wrap's
       overflow-x this gives the operator real columns instead of
       everything truncated to nothing. */
  }
  thead {
    background: var(--bg-panel);
    position: sticky;
    top: 0;
  }
  th {
    text-align: left;
    border-bottom: 1px solid var(--border-strong);
    padding: 0;
  }
  th.active .header-button {
    color: var(--fg-primary);
  }
  th.numeric .header-button {
    text-align: right;
  }
  .header-button {
    width: 100%;
    background: transparent;
    border: 0;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 500;
    text-align: left;
    text-transform: none;
    letter-spacing: 0;
    padding: 6px 8px;
    cursor: pointer;
    /* Header buttons get a hover affordance so the sort affordance
       is discoverable even before the operator clicks. */
    transition: background var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .header-button:hover,
  .header-button:focus-visible {
    background: var(--bg-elevated, var(--bg-panel-alt));
    color: var(--fg-primary);
  }

  tbody tr {
    /* Subtle row separator without the noise of a full grid. */
    border-bottom: 1px solid var(--border-subtle);
  }
  tbody tr:last-child {
    border-bottom: 0;
  }
  td {
    padding: 4px 8px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-primary);
    vertical-align: top;
    /* Cap individual cell width so a long string can't push the
       whole row off-screen; horizontal scroll handles the rest. */
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  td.numeric {
    text-align: right;
  }
  td.empty .dim {
    color: var(--fg-quaternary, var(--fg-tertiary));
  }

  .row-toggle {
    display: flex;
    justify-content: flex-end;
  }
</style>
