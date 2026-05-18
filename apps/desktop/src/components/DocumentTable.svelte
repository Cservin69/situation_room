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

  - **No CSV export.** Document body is already on disk; export
    belongs to a records-export surface, not the drawer.
  - **No per-cell drilldown.** Nested objects render as truncated
    JSON literals. A click-to-expand affordance is a later session
    if the operator hits a case where the truncation matters.

  ## What this component DOES (Session 91)

  - **Substring filter.** Icon-first search affordance in the header
    that expands into a text input on focus. Filter is
    case-insensitive `contains` against the rendered cells of
    *visible* columns only (so toggling "show all columns" widens
    the filter's surface in step with what the operator sees).
    Filter composes with sort: sort first, filter second. Empty
    query passes everything through. The existing row cap applies
    to the *filtered* set, so the cap stays meaningful when a
    filter is active. Sister surface: SamplesModal's filter shares
    `$lib/dashboard/text_filter.ts::matchesQuery` — drift between
    the two would surprise operators using both in one session.
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
  import { matchesQuery } from '$lib/dashboard/text_filter';

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

  // -- filter state (Session 91) -----------------------------------
  //
  // `filterQuery` is the live input value. The expanded/collapsed
  // affordance keeps the header compact on short payloads: an icon-
  // only button until the operator focuses or types into it, then
  // an inline text input. The collapsed state matters because the
  // caption strip is also where the col-cap toggle lives — three
  // affordances side by side without the collapse would crowd the
  // header on narrow drawers.
  let filterQuery = $state('');
  let filterExpanded = $state(false);
  // `$state` wrap so `bind:this` writes are picked up by the
  // reactivity system — without it, svelte-check warns that the
  // post-mount `filterInput` reference won't notify dependents.
  // Today nothing reactive *reads* filterInput (it's only used
  // inside an event handler microtask), so the runtime impact is
  // nil, but the lint is structurally correct.
  let filterInput = $state<HTMLInputElement | undefined>(undefined);

  function onFilterIconClick() {
    filterExpanded = true;
    // Defer focus until Svelte's reactive update lights up the input
    // node; otherwise `filterInput` is still the pre-render undefined
    // and `.focus()` no-ops.
    queueMicrotask(() => filterInput?.focus());
  }
  function onFilterBlur() {
    // Auto-collapse only when empty; keep the input visible if the
    // operator typed something so they don't lose the filter on a
    // stray click elsewhere.
    if (filterQuery.trim().length === 0) {
      filterExpanded = false;
    }
  }
  function onFilterClear() {
    filterQuery = '';
    filterExpanded = false;
  }

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
   * Filter-first pipeline (Session 91): sort → filter → cap. The
   * row cap applies to the *filtered* set so it remains meaningful
   * when a filter is active. The filter haystack is the rendered
   * text of the *visible* columns only — toggling "show all
   * columns" widens what the filter can match in step with what
   * the operator sees.
   *
   * Two derivations:
   *   1. `sortedFilteredRows` — full filtered+sorted set, used by
   *      the "Showing M of N" caption and the row-cap toggle.
   *   2. `displayRows` — same set capped to INITIAL_ROWS unless
   *      `rowsExpanded`.
   *
   * Sort is non-destructive (slice + sort on the copy) so toggling
   * sort off restores the input order.
   */
  let sortedFilteredRows = $derived.by(() => {
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
    // Filter against *visible* columns only. `renderCell` matches
    // what the operator sees in the cell; joining with a space
    // separator means a query that spans two columns matches the
    // visual reading order.
    const q = filterQuery.trim();
    if (q.length === 0) return rows;
    return rows.filter((row) => {
      const hay = visibleColumns.map((c) => renderCell(row[c])).join(' ');
      return matchesQuery(hay, q);
    });
  });
  let displayRows = $derived(
    rowsExpanded || sortedFilteredRows.length <= INITIAL_ROWS
      ? sortedFilteredRows
      : sortedFilteredRows.slice(0, INITIAL_ROWS),
  );
  let hiddenRowCount = $derived(sortedFilteredRows.length - displayRows.length);
  // "Showing M of N" caption visible only when a filter is active.
  // Empty-query case stays clean — the existing row count already
  // tells the operator how many rows the table has.
  let filterIsActive = $derived(filterQuery.trim().length > 0);

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
    {#if filterIsActive}
      <span class="counts filter-caption">
        showing {sortedFilteredRows.length} of {table.rows.length}
      </span>
    {/if}
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
    <!-- Session 91 filter: icon-first when collapsed, input when
         focused or non-empty. Sits at the right edge of the caption
         strip; `margin-left: auto` on `.filter` and `.toggle` lets
         each push the other against the edge cleanly. -->
    <div class="filter" class:expanded={filterExpanded}>
      {#if filterExpanded || filterIsActive}
        <input
          bind:this={filterInput}
          bind:value={filterQuery}
          class="filter-input"
          type="text"
          placeholder="filter rows…"
          aria-label="filter rows"
          onblur={onFilterBlur}
        />
        {#if filterIsActive}
          <button
            class="filter-clear"
            type="button"
            aria-label="clear filter"
            onclick={onFilterClear}
            title="clear filter"
          >
            ×
          </button>
        {/if}
      {:else}
        <button
          type="button"
          class="filter-icon"
          onclick={onFilterIconClick}
          aria-label="filter rows"
          title="filter rows"
        >
          ⌕
        </button>
      {/if}
    </div>
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

  {#if filterIsActive && sortedFilteredRows.length === 0}
    <p class="empty">— no rows match the filter</p>
  {/if}

  {#if hiddenRowCount > 0 || (rowsExpanded && sortedFilteredRows.length > INITIAL_ROWS)}
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

  /* Session 91 — filter affordance. Icon-only when collapsed; expands
     into an inline text input on focus or when a query is active. Sits
     at the right edge of the caption strip; `.toggle` already claims
     `margin-left: auto`, so when both render, the toggle stays leftmost
     of the right-aligned pair and the filter follows. */
  .filter {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: 6px;
  }
  .filter:not(.expanded) {
    margin-left: auto;
  }
  .filter-icon {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1;
    padding: 2px 6px;
    cursor: pointer;
  }
  .filter-icon:hover,
  .filter-icon:focus-visible {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .filter-input {
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 6px;
    width: 140px;
    outline: none;
  }
  .filter-input:focus {
    border-color: var(--border-strong);
  }
  .filter-clear {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1;
    padding: 1px 5px;
    cursor: pointer;
  }
  .filter-clear:hover,
  .filter-clear:focus-visible {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .filter-caption {
    /* Tucked next to the row+col counts; same colour as `.counts` so it
       reads as a sibling, not an alert. */
    color: var(--fg-tertiary);
  }
  .empty {
    margin: 4px 0 0;
    padding: 8px 0;
    font-family: var(--font-mono);
    font-size: 11px;
    font-style: italic;
    color: var(--fg-tertiary);
    text-align: center;
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
