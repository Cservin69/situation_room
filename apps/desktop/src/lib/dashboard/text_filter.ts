/*
  text_filter — shared substring-match predicate for long-list
  inspection surfaces (Session 91).

  Two consumers today:

    - `DocumentTable.svelte` — when a Document body's table view runs
      to hundreds of rows, scanning is a chore. Wraps cell-rendered
      text per row and filters before the existing row cap.
    - `SamplesModal.svelte` — when a kind's `+N more` overflow list
      grows past a few dozen distinct strings (e.g. relations with
      153 distinct from→to pairs), scrolling to find a specific name
      is the same chore. Filters the full sample list.

  Both surfaces deliberately shipped *without* filtering, on the same
  assumption: "ship the affordance when scrolling-to-find becomes
  costly." Session 90 verify validated the gap on both flows in the
  same operator session. This helper exists so the two consumers
  share one predicate — drift between them would surprise the
  operator (case folding behaviour, whitespace handling).

  ## Shape

  - **Substring `contains` match.** No regex, no fuzzy match.
    Operators read these affordances as filter inputs, not as search
    expressions; a regex affordance would force them to think about
    escaping when they want to type `tsla` and find the company.
  - **Locale-aware lowercase.** `String.prototype.toLocaleLowerCase`
    over `.toLowerCase`. Both already cover the default Unicode
    cased-pair fold (Latin accented letters, Cyrillic, Greek, etc.);
    using the locale-aware variant keeps the door open for a
    deployment-time locale tag in a future iteration without changing
    the call sites. Operator-visible: `Düsseldorf` / `Москва` match
    their lowercase forms cleanly.

    **Known gap.** Turkish dotted-I (`İ` U+0130 → `i̇` i+U+0307 under
    default-locale fold) won't substring-match plain `i`. NFC
    normalise + combining-mark strip would close this; v1
    deliberately ships without that step (no operator gap surfaced
    for it yet, and the helper's reach is small enough that adding
    it later is one diff).
  - **Empty/whitespace-only query is a passthrough.** Returning
    `true` for an empty query lets consumers use the predicate
    directly in a `filter` without an outer "if query then filter
    else passthrough" branch.

  ## What this module does NOT do

  - **No tokenisation.** "supplies to" matches "supplies_to" only if
    the haystack contains the literal substring. Token-level fuzz
    belongs to a search service, not a filter input.
  - **No highlight rendering.** Consumers render their own rows;
    this module only answers yes/no.
  - **No diacritic folding.** `Müller` won't match `muller`. The
    surface area where this matters today is small (operator-typed
    queries against operator-visible entity names that the operator
    can paste verbatim if needed). If a future session surfaces a
    diacritic-fold use case, the predicate can grow.
*/

/**
 * Substring `contains` predicate. Case- and locale-aware.
 *
 * Returns:
 *   - `true` when `query.trim()` is empty (no-filter passthrough), or
 *   - `true` when `haystack.toLocaleLowerCase()` contains
 *     `query.trim().toLocaleLowerCase()` as a substring.
 *
 * Otherwise returns `false`.
 *
 * @param haystack the rendered, operator-visible text the filter
 *   surface is showing for one row / one sample
 * @param query   the operator's filter input
 */
export function matchesQuery(haystack: string, query: string): boolean {
  const q = query.trim();
  if (q.length === 0) return true;
  // Locale-aware lowercase: handles Unicode cased pairs that the
  // ASCII-only `toLowerCase` mishandles (Turkish dotted-I, etc.).
  const needle = q.toLocaleLowerCase();
  const hay = haystack.toLocaleLowerCase();
  return hay.includes(needle);
}
