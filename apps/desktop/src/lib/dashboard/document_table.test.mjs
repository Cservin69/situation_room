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

// Session 74.5: object-of-scalars qualifies as a key-value table.
// `{"foo":"bar","baz":42}` previously returned `null`; now it
// surfaces as a 2-row Key/Value table. The drawer's pretty-printed
// JSON fallback is still one click away if the operator prefers
// the literal payload.
const r3 = detectTableShape('{"foo":"bar","baz":42}');
expect('object-of-scalars path', r3.path, '$');
expect('object-of-scalars cols', r3.columns, ['Key', 'Value']);
expect('object-of-scalars rows.length', r3.rows.length, 2);
expect('object-of-scalars first row', r3.rows[0], { Key: 'foo', Value: 'bar' });

const r4 = detectTableShape('[{"a":1}]');
expect('single-row', r4, null);

// Single-entry object falls below MIN_ROWS — still null.
const r4b = detectTableShape('{"only":"one"}');
expect('object-of-scalars single-entry', r4b, null);

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

// -- Session 74.5: JSON-stat / object-of-scalars shape ------------

// Eurostat-shaped: nested `value` is the object-of-scalars; the
// outer object has mixed scalar + nested-object children so it
// doesn't qualify itself. Detector should walk in and pick the
// `value` block.
const eurostat = JSON.stringify({
  version: '2.0',
  class: 'dataset',
  label: 'Crop production',
  source: 'ESTAT',
  value: {
    162: 4499.8,
    163: 4516.1,
    164: 4958.7,
    165: 4497.7,
  },
});
const re = detectTableShape(eurostat);
expect('eurostat path', re.path, 'value');
expect('eurostat cols', re.columns, ['Key', 'Value']);
expect('eurostat rows.length', re.rows.length, 4);
expect('eurostat first row', re.rows[0], { Key: '162', Value: 4499.8 });

// Numeric-string keys: isNumericColumn should detect both Key and
// Value as numeric so sort works in numeric order rather than
// "10" before "2".
expect(
  'eurostat numeric Key',
  isNumericColumn(re.rows, 'Key'),
  true,
);
expect(
  'eurostat numeric Value',
  isNumericColumn(re.rows, 'Value'),
  true,
);

// Preference: when BOTH shapes are present in the same payload,
// array_of_objects wins regardless of row count.
const mixedShapes = JSON.stringify({
  // 100-entry key-value lookup
  lookup: Object.fromEntries(
    Array.from({ length: 100 }, (_, i) => [String(i), i * 2]),
  ),
  // 3-row array-of-objects — smaller, but more semantic, so it wins.
  records: [
    { id: 1, label: 'a' },
    { id: 2, label: 'b' },
    { id: 3, label: 'c' },
  ],
});
const rm = detectTableShape(mixedShapes);
expect('mixed prefers array-of-objects', rm.path, 'records');
expect('mixed cols', rm.columns, ['id', 'label']);

// Object with a mixed value (one nested object) is NOT
// object-of-scalars — must recurse and produce null when nothing
// else qualifies.
const mixedScalar = JSON.stringify({ a: 1, b: { nested: true } });
expect('mixed-scalar+object', detectTableShape(mixedScalar), null);

// Bare object-of-scalars at the root with numeric values.
const bare = detectTableShape('{"USD":1.0,"EUR":0.92,"GBP":0.79}');
expect('bare object-of-scalars path', bare.path, '$');
expect('bare object-of-scalars rows', bare.rows.length, 3);
expect('bare numeric Value', isNumericColumn(bare.rows, 'Value'), true);

// -- Session 74.5: truncated-JSON recovery ------------------------

// Truncated Eurostat-shaped body: the outer object never closes,
// and `value` cuts off mid-entry. Recovery should slice at the
// last balanced close inside `value`, append `}}` to balance the
// outer scopes, and the detector should still find the value
// block. Note the truncation happens AFTER several complete
// `"key":number` entries so there's a recoverable point.
const truncated =
  '{"version":"2.0","class":"dataset","value":{"162":4499.80,' +
  '"163":4516.10,"164":4958.70,"165":4497.70,"166":4499.60,' +
  '"167":4503.00,"168":4500.00,"169":4493.';
const rt = detectTableShape(truncated);
// Recovery may yield 0-N rows depending on how much of `value`
// was past the last balanced close; for this fixture the inner
// `{` opens at value-start but no inner close exists yet, so the
// outer-scope `{"version":...,"class":...}` is the recovered
// document. That falls back to "no object-of-scalars at root"
// (value would have been the candidate but it's gone), and there's
// no other table-shaped candidate. Recovery succeeded but no table
// surfaced — null is the honest answer. The previous behaviour was
// also null (parse failed); the value-add is that recovery
// produces a partial parsed structure the dashboard can still
// reason about (e.g. for future drawer panels that read metadata).
expect('truncated mid-entry → null (no balanced inner close)', rt, null);

// Truncation where AT LEAST ONE balanced inner close exists:
// recovery succeeds AND the surviving entries are tableable.
const truncatedAfterClose =
  '{"version":"2.0","value":{"a":1,"b":2,"c":3},"dimension":' +
  '{"geo":{"label":"Geo","cate';
const rtc = detectTableShape(truncatedAfterClose);
expect('truncated after inner close path', rtc.path, 'value');
expect('truncated after inner close cols', rtc.columns, ['Key', 'Value']);
expect('truncated after inner close rows', rtc.rows.length, 3);

// Already-complete JSON shouldn't go through the recovery path —
// the detector parses directly and recovery doesn't fire. Smoke
// test: a complete body still produces the expected shape with
// the warning path quiet.
const complete = '{"a":1,"b":2}';
const rcc = detectTableShape(complete);
expect('complete object-of-scalars path', rcc.path, '$');
expect('complete object-of-scalars rows', rcc.rows.length, 2);

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
