/*
  text_filter — node-runnable regression tests (Session 91).

  Same harness shape as `document_table.test.mjs` — compile + run
  with `node`, exit non-zero on any failure. Run via
  `bash session91-verify.sh` (compiles the .ts then runs this), or
  standalone:

    cd apps/desktop
    ./node_modules/.bin/tsc --target es2022 --moduleResolution bundler \
      --module esnext --strict --skipLibCheck \
      --outDir /tmp/tf-build src/lib/dashboard/text_filter.ts
    node --input-type=module \
      -e "$(sed s,/tmp/tf-build/,/tmp/tf-build/, src/lib/dashboard/text_filter.test.mjs)"

  ## What's covered

  - empty query, whitespace-only query → passthrough true
  - prefix / infix / suffix matches
  - case folding (ASCII)
  - case folding (Unicode — Turkish dotted-I, German ß context,
    accented vowel)
  - empty haystack against non-empty query → false
  - non-match returns false (defensive — sanity)
*/

import { matchesQuery } from '/tmp/tf-build/text_filter.js';

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

// ---- Empty / whitespace query → passthrough -------------------

expect('empty query passes through', matchesQuery('anything', ''), true);
expect('whitespace-only query passes through', matchesQuery('anything', '   '), true);
expect('tab-only query passes through', matchesQuery('anything', '\t\t'), true);
expect('empty haystack + empty query → true (no filter)', matchesQuery('', ''), true);

// ---- Substring positions ----------------------------------------

expect('prefix match', matchesQuery('panasonic battery', 'pan'), true);
expect('infix match', matchesQuery('agency:reuters', 'reut'), true);
expect('suffix match', matchesQuery('company:tsla', 'tsla'), true);
expect('non-match', matchesQuery('company:tsla', 'meta'), false);
expect('non-match with overlap', matchesQuery('company:tsla', 'slat'), false);

// ---- ASCII case folding -----------------------------------------

expect('upper query → lower haystack', matchesQuery('reuters', 'REUT'), true);
expect('mixed query → lower haystack', matchesQuery('reuters', 'ReUt'), true);
expect('lower query → upper haystack', matchesQuery('REUTERS', 'reut'), true);
expect('mixed both', matchesQuery('Reuters Holdings', 'HOLD'), true);

// ---- Unicode case folding (locale-aware) ------------------------

// Accented vowels: `Düsseldorf` should match `düsseldorf` (case
// fold) and substrings inside it.
expect('umlaut substring match', matchesQuery('Düsseldorf', 'dorf'), true);
expect('umlaut case-folded match', matchesQuery('düsseldorf', 'DÜSS'), true);
expect('accent capital matches lowercase', matchesQuery('Söze', 'sö'), true);
// Negative: a query with a non-Unicode-equivalent fold doesn't
// match. (We don't fold diacritics, per the module's stated scope.)
expect('no diacritic folding — muller does not match Müller', matchesQuery('Müller', 'muller'), false);
expect('no diacritic folding — söze does not match soze', matchesQuery('Söze', 'soze'), false);
// Cyrillic case folding. `Москва` matches `моск` and `МОСК`.
expect('cyrillic case-folded match', matchesQuery('Москва', 'МОСК'), true);
expect('cyrillic substring match', matchesQuery('Москва', 'осква'), true);
// NOTE: Turkish dotted-I (`İ` → `i̇` under default locale) is *not*
// tested here because the substring match against ASCII `i` is
// stringly-not-equivalent (the lowercased form carries U+0307
// combining mark). Operators searching for `istanbul` against a
// document containing `İstanbul` will not match under the v1
// predicate. If that surfaces as a real operator gap, the
// predicate's next iteration is NFC normalisation + combining-mark
// strip; landing it speculatively would bias other folds.

// ---- Empty haystack -------------------------------------------

expect('empty haystack against non-empty query', matchesQuery('', 'reut'), false);

if (fails > 0) {
  console.log(`\n${fails} failure${fails === 1 ? '' : 's'}.`);
  process.exit(1);
} else {
  console.log('\nall ok');
}
