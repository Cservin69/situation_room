/*
  document_table — node-runnable regression tests (Session 73).

  Frontend doesn't carry a test framework (no vitest, no jest); the
  established convention is "compile + `npm run check`" plus
  Rust-side `cargo test` for shape-stable behaviours. This file
  bridges the gap for the detector: it imports the compiled module
  and exercises the pure-TS surface with `node` directly.

  Run via `bash session73-verify.sh` (compiles the .ts then runs
  this), or standalone:

    cd apps/desktop
    ./node_modules/.bin/tsc --target es2022 --moduleResolution bundler \
      --module esnext --strict --skipLibCheck \
      --outDir /tmp/dt-build src/lib/dashboard/document_table.ts
    node --input-type=module \
      -e "$(sed s,/tmp/dt-build/,/tmp/dt-build/, src/lib/dashboard/document_table.test.mjs)"

  The session73-verify.sh wrapper does all this. The tests exit
  non-zero on any failure so the wrapper's `EXIT=N` sentinel reports
  green/red.

  ## What's covered

  - detectTableShape: bare-array, OData, FEMA-shaped, mixed-types,
    "largest wins" tie-break, frequency-ordered columns, null
    returns for malformed/empty/non-JSON/non-array/single-row.
  - renderCell: primitive pass-through, null → '', objects →
    JSON.stringify + cap; string pass-through (CSS handles visual
    truncation, capping strings here would break the cell-title
    hover).
  - isNumericColumn: all-numeric/coercible-string → true; mixed → false.
  - nextSortDirection: 3-state cycle null → asc → desc → null.
  - compareCells: numeric vs string, nulls always last.

  ## What's NOT covered

  - DocumentTable.svelte rendering — svelte-check covers types;
    visual + interactive behaviour is operator-verified live.
  - Truncation recovery — explicitly out of scope (file-level
    comment in document_table.ts).
*/

import {
  detectTableShape,
  renderCell,
  isNumericColumn,
  nextSortDirection,
  compareCells,
  MIN_ROWS,
} from '/tmp/dt-build/document_table.js';

let fails = 0;
function expect(name, actual, expected) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    console.log(`FAIL ${name}: got ${a}, want ${e}`);
    fails++;
  } else {
    console.log(`ok ${name}`);
  }
}

// ---- detectTableShape -------------------------------------------

const r1 = detectTableShape('[{"a":1,"b":"x"},{"a":2,"b":"y"}]');
expect('bare-array path', r1.path, '$');
expect('bare-array cols', r1.columns, ['a', 'b']);
expect('bare-array rows.length', r1.rows.length, 2);

const r2 = detectTableShape(
  '{"value":[{"id":1,"name":"A"},{"id":2,"name":"B"},{"id":3,"name":"C"}]}',
);
expect('odata path', r2.path, 'value');
expect('odata cols', r2.columns, ['id', 'name']);

const r3 = detectTableShape('{"foo":"bar","baz":42}');
expect('object-no-array', r3, null);

const r4 = detectTableShape('[{"a":1}]');
expect('single-row', r4, null);

const r5 = detectTableShape('{"x":[1,2,3],"y":[{"a":1},{"a":2}]}');
expect('chooses object array', r5.path, 'y');

const r6 = detectTableShape(
  '{"a":[{"k":1},{"k":2}],"b":[{"k":1},{"k":2},{"k":3},{"k":4}]}',
);
expect('largest wins', r6.path, 'b');
expect('largest rows', r6.rows.length, 4);

const r7 = detectTableShape('[{"x":1,"y":1},{"x":2,"y":2,"z":2},{"x":3}]');
expect('freq-ordered cols', r7.columns, ['x', 'y', 'z']);

// FEMA-shaped payload regression — column count + frequency order.
const fema = JSON.stringify({
  metadata: { count: 3, top: 1000 },
  DisasterDeclarationsSummaries: [
    { id: '4001', state: 'CA', declarationDate: '2024-01-15', incidentType: 'Fire' },
    { id: '4002', state: 'TX', declarationDate: '2024-02-03', incidentType: 'Hurricane' },
    {
      id: '4003',
      state: 'FL',
      declarationDate: '2024-03-12',
      incidentType: 'Hurricane',
      closeoutDate: '2025-01-01',
    },
  ],
});
const rf = detectTableShape(fema);
expect('fema path', rf.path, 'DisasterDeclarationsSummaries');
expect('fema rows', rf.rows.length, 3);
expect(
  'fema cols',
  rf.columns,
  ['declarationDate', 'id', 'incidentType', 'state', 'closeoutDate'],
);

// Defensive: malformed JSON / empty / non-JSON → null, no throw.
expect('malformed', detectTableShape('{"a":[{...'), null);
expect('empty', detectTableShape(''), null);
expect('non-json', detectTableShape('hello world'), null);

// ---- renderCell -------------------------------------------------

expect('cell-string', renderCell('hello'), 'hello');
expect('cell-num', renderCell(42), '42');
expect('cell-bool', renderCell(true), 'true');
expect('cell-null', renderCell(null), '');
expect('cell-undefined', renderCell(undefined), '');
expect('cell-obj', renderCell({ a: 1, b: 2 }), '{"a":1,"b":2}');
// Strings pass through — table CSS handles visual ellipsis via
// `text-overflow: ellipsis` so we don't double-truncate.
expect(
  'cell-long-string-passthrough',
  renderCell('x'.repeat(100), 10),
  'x'.repeat(100),
);
// Objects/arrays cap at the requested limit (cap chars total
// including the U+2026 ellipsis).
expect(
  'cell-long-object-caps',
  renderCell({ huge: 'x'.repeat(100) }, 20),
  '{"huge":"xxxxxxxxxx…',
);

// ---- isNumericColumn --------------------------------------------

expect('numeric-col-all-num', isNumericColumn([{ a: 1 }, { a: '2' }, { a: 3 }], 'a'), true);
expect('numeric-col-mixed', isNumericColumn([{ a: 1 }, { a: 'hi' }], 'a'), false);
expect('numeric-col-empty', isNumericColumn([{}, {}], 'a'), false);
expect('numeric-col-null-skipped', isNumericColumn([{ a: 1 }, { a: null }, { a: 3 }], 'a'), true);

// ---- nextSortDirection ------------------------------------------

expect('sort-cycle-1', nextSortDirection(null), 'asc');
expect('sort-cycle-2', nextSortDirection('asc'), 'desc');
expect('sort-cycle-3', nextSortDirection('desc'), null);

// ---- compareCells -----------------------------------------------

expect('cmp-null-last-1', compareCells(null, 1, true), 1);
expect('cmp-null-last-2', compareCells(1, null, true), -1);
expect('cmp-null-both', compareCells(null, null, false), 0);
expect('cmp-numeric', Math.sign(compareCells('10', '2', true)), 1);
expect('cmp-string', Math.sign(compareCells('apple', 'banana', false)), -1);

// ---- exports sanity ---------------------------------------------

expect('min-rows', MIN_ROWS, 2);

if (fails > 0) {
  console.log(`\n${fails} FAILURES`);
  process.exit(1);
}
console.log(`\nALL PASS (${fails === 0 ? 'all assertions ok' : ''})`);
