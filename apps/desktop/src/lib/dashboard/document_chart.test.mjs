/*
  document_chart — node-runnable regression tests (Session 83).

  Mirrors the document_table.test.mjs pattern: import the compiled
  module from /tmp/dc-build and exercise the pure-TS surface. The
  session83-verify.sh wrapper does the compile + run; standalone:

    cd apps/desktop
    ./node_modules/.bin/tsc --target es2022 --moduleResolution bundler \
      --module esnext --strict --skipLibCheck \
      --outDir /tmp/dc-build src/lib/dashboard/document_chart.ts
    node src/lib/dashboard/document_chart.test.mjs

  ## What's covered

  - detectChartCatalog on a Yahoo-shaped fixture: catalog surfaces all
    six numeric series (close, volume, low, high, open, adjclose),
    label resolves to the `symbol` field, timestamps preserved.
  - Series ranking: `close` (primary) lands first; `adjclose` (no rank)
    lands after open/high/low/volume (secondary).
  - Empty / malformed inputs return null cleanly.
  - pickPreviewSeries + detectPreview signature-compat with the
    pre-Session-83 `documentSeriesOf` shape.
  - stridedPoints caps to PREVIEW_POINT_CAP and always preserves the
    last sample.
  - nearestIndex binary search on monotonic timestamps; tie-break and
    out-of-range behaviour.
  - formatChartTimestamp seconds-vs-ms heuristic + intraday-vs-daily
    granularity switch.
  - formatChartValue thousands separators, integer pass-through, and
    two-decimal float formatting.
  - recoverTruncatedJson on a Yahoo-shaped tail truncation.

  ## What's NOT covered

  - DrawerChart.svelte rendering — svelte-check covers types;
    visual + interactive behaviour is operator-verified live.
*/

import {
  detectChartCatalog,
  pickPreviewSeries,
  detectPreview,
  stridedPoints,
  nearestIndex,
  formatChartTimestamp,
  formatChartValue,
  recoverTruncatedJson,
  rankKey,
  PRIMARY_SERIES_KEYS,
  SECONDARY_SERIES_KEYS,
  PREVIEW_POINT_CAP,
} from '/tmp/dc-build/document_chart.js';

let fails = 0;
function expect(name, actual, expected) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    console.log(`FAIL ${name}: got ${a}, want ${e}`);
    fails++;
  }
}
function expectTrue(name, cond, hint) {
  if (!cond) {
    console.log(`FAIL ${name}: ${hint ?? 'cond was false'}`);
    fails++;
  }
}

// ---- Fixtures ------------------------------------------------------

const YAHOO_BODY = JSON.stringify({
  chart: {
    result: [
      {
        meta: {
          currency: 'USD',
          symbol: 'TSLA',
          longName: 'Tesla, Inc.',
          shortName: 'Tesla, Inc.',
        },
        timestamp: [1715866200, 1715952600, 1716211800, 1716298200, 1716384600],
        indicators: {
          quote: [
            {
              close: [174.83, 177.46, 174.95, 186.60, 180.11],
              volume: [59812200, 77445800, 61727400, 115266500, 88313500],
              low: [171.42, 172.75, 173.52, 174.71, 178.11],
              high: [175.78, 179.63, 177.75, 186.88, 183.80],
              open: [174.10, 173.55, 177.55, 175.50, 182.85],
            },
          ],
          adjclose: [
            {
              adjclose: [174.83, 177.46, 174.95, 186.60, 180.11],
            },
          ],
        },
      },
    ],
    error: null,
  },
});

// ---- detectChartCatalog: Yahoo shape -------------------------------

{
  const cat = detectChartCatalog(YAHOO_BODY);
  expectTrue('yahoo: catalog non-null', cat !== null, 'expected catalog');
  expect('yahoo: timestamp count', cat.timestamps.length, 5);
  expect('yahoo: timestamp[0]', cat.timestamps[0], 1715866200);
  expect('yahoo: label resolves to symbol', cat.label, 'TSLA');
  // All six numeric series enumerated:
  const keys = cat.series.map((s) => s.key).sort();
  expect('yahoo: series keys (sorted)', keys, ['adjclose', 'close', 'high', 'low', 'open', 'volume']);
  // close ranks first (PRIMARY tier), then open/high/low/volume in
  // SECONDARY tier (first-seen wins within tier), then adjclose
  // (not in either list).
  expect('yahoo: ranked first is close', cat.series[0].key, 'close');
  expect('yahoo: ranked last is adjclose', cat.series[cat.series.length - 1].key, 'adjclose');
  // PRIMARY > SECONDARY ordering: close before any of open/high/low/volume.
  const closeIdx = cat.series.findIndex((s) => s.key === 'close');
  const openIdx = cat.series.findIndex((s) => s.key === 'open');
  const volumeIdx = cat.series.findIndex((s) => s.key === 'volume');
  expectTrue('yahoo: close before open', closeIdx < openIdx);
  expectTrue('yahoo: close before volume', closeIdx < volumeIdx);
  // SECONDARY > "other": all SECONDARY keys before adjclose.
  const adjIdx = cat.series.findIndex((s) => s.key === 'adjclose');
  expectTrue('yahoo: open before adjclose', openIdx < adjIdx);
  expectTrue('yahoo: volume before adjclose', volumeIdx < adjIdx);
}

// ---- pickPreviewSeries / detectPreview signature-compat -----------

{
  const cat = detectChartCatalog(YAHOO_BODY);
  const preview = pickPreviewSeries(cat);
  expectTrue('preview: non-null', preview !== null);
  expect('preview: valueKey', preview.valueKey, 'close');
  expect('preview: label', preview.label, 'TSLA');
  expect('preview: point count', preview.points.length, 5);
  expect('preview: first point', preview.points[0], { x: 1715866200, y: 174.83 });
  expect('preview: last point', preview.points[4], { x: 1716384600, y: 180.11 });

  const direct = detectPreview(YAHOO_BODY);
  expect('detectPreview matches pickPreviewSeries', direct, preview);
}

// ---- null returns --------------------------------------------------

expect('empty body → null', detectChartCatalog(''), null);
expect('non-JSON body → null', detectChartCatalog('hello world'), null);
expect('JSON without timestamps → null',
  detectChartCatalog(JSON.stringify({ foo: 1, bar: 2 })),
  null);
expect('JSON timestamp but no value array → null',
  detectChartCatalog(JSON.stringify({ timestamp: [1, 2, 3] })),
  null);
expect('JSON timestamp + length-mismatched values → null',
  detectChartCatalog(JSON.stringify({ timestamp: [1, 2, 3], close: [10, 20] })),
  null);

// ---- stridedPoints --------------------------------------------------

{
  const ts = Array.from({ length: 1200 }, (_, i) => 1700000000 + i);
  const vs = Array.from({ length: 1200 }, (_, i) => i);
  const pts = stridedPoints(ts, vs);
  expectTrue('stride: cap respected', pts.length <= PREVIEW_POINT_CAP + 1);
  expect('stride: last point preserved', pts[pts.length - 1].y, 1199);
}
{
  const pts = stridedPoints([10, 20, 30], [1, 2, 3]);
  expect('stride: small input passes through', pts.length, 3);
}

// ---- nearestIndex --------------------------------------------------

const ts = [10, 20, 30, 40];
expect('nearestIndex: target = ts[0]', nearestIndex(ts, 10), 0);
expect('nearestIndex: target < ts[0]', nearestIndex(ts, 5), 0);
expect('nearestIndex: target nearer lo', nearestIndex(ts, 11), 0);
expect('nearestIndex: tie → lo', nearestIndex(ts, 15), 0);
expect('nearestIndex: target nearer hi', nearestIndex(ts, 16), 1);
expect('nearestIndex: target = ts[mid]', nearestIndex(ts, 30), 2);
expect('nearestIndex: target = ts[last]', nearestIndex(ts, 40), 3);
expect('nearestIndex: target > ts[last]', nearestIndex(ts, 999), 3);
expect('nearestIndex: empty array → -1', nearestIndex([], 5), -1);
expect('nearestIndex: single element', nearestIndex([42], 99), 0);

// ---- formatChartTimestamp -----------------------------------------

{
  // Yahoo-style seconds, multi-month span → date only.
  // 1715866200 = 2024-05-16T13:30:00Z (Yahoo daily close stamps).
  const span = 365 * 86400; // a year in seconds
  expect('formatTs: seconds + year span → date only',
    formatChartTimestamp(1715866200, span),
    '2024-05-16');
  // Sub-30-day span → include HH:MM.
  expect('formatTs: seconds + day span → includes HH:MM',
    formatChartTimestamp(1715866200, 86400),
    '2024-05-16 13:30 UTC');
  // ms-scale + year span → still date only via the heuristic.
  expect('formatTs: ms + year span → date only',
    formatChartTimestamp(1715866200_000, 365 * 86_400_000),
    '2024-05-16');
}

// ---- formatChartValue ---------------------------------------------

expect('value: integer pass-through', formatChartValue(42), '42');
expect('value: small float two decimals', formatChartValue(3.14159), '3.14');
expect('value: large float thousands separators', formatChartValue(1234.5678), '1,234.57');
expect('value: millions no decimals', formatChartValue(59812200), '59,812,200');
expect('value: zero', formatChartValue(0), '0');
expect('value: negative', formatChartValue(-3.5), '-3.50');

// ---- rankKey ------------------------------------------------------

expect('rank: primary close → 0', rankKey('close'), 0);
expect('rank: primary price → 1', rankKey('price'), 1);
expect('rank: secondary open → primary.length',
  rankKey('open'), PRIMARY_SERIES_KEYS.length);
expectTrue('rank: unknown > secondary',
  rankKey('adjclose') > PRIMARY_SERIES_KEYS.length + SECONDARY_SERIES_KEYS.length - 1);

// ---- recoverTruncatedJson -----------------------------------------

{
  // Build a body and chop it mid-array; recovery should slice at
  // the last balanced close and append the closing tail.
  const full = JSON.stringify({
    timestamp: [1, 2, 3],
    quote: [{ close: [10, 20, 30] }],
  });
  const chopped = full.slice(0, full.indexOf(']') + 1); // "{...timestamp:[1,2,3]"
  // Without the outer braces this isn't recoverable on its own; the
  // recovery snapshot is taken AT each close while there are still
  // open scopes. Use a body that has an inner closed array followed
  // by truncation deeper in:
  const body = '{"timestamp":[1,2,3],"close":[10,20';
  const recovered = recoverTruncatedJson(body);
  expectTrue('recovery: returns string for truncated array', typeof recovered === 'string');
  // After recovery the body should parse:
  let parsedOK = false;
  try { JSON.parse(recovered); parsedOK = true; } catch { /* */ }
  expectTrue('recovery: result parses', parsedOK);
}
expect('recovery: no safe point → null',
  recoverTruncatedJson('{"a":'),
  null);

// ---- Summary -------------------------------------------------------

if (fails === 0) {
  console.log('document_chart: all assertions passed');
  process.exit(0);
} else {
  console.log(`document_chart: ${fails} assertion(s) failed`);
  process.exit(1);
}
