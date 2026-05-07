<!--
  RecipesPanel — shows the recipes Level-2 authored for the selected
  plan, with each recipe's URL and extraction spec rendered for human
  inspection.

  Why this exists (Session 11 P2.5)
  ----------------------------------

  Up to Session 10, recipes were authored, persisted, and executed —
  but invisible in the UI. When a recipe failed (Session 9's
  `example.invalid` echo-back, or Session 11's first real run with
  Swiss-debt fetching `CH` instead of `CHE`), the only way to see
  what the LLM actually produced was a DuckDB query against the
  `recipes` table. That worked but punished the developer for using
  the desktop app at all.

  This panel makes the recipes legible inline. For each recipe:

    - source_id and dedup_key as a header strip
    - per-recipe outcome badge from the most recent fetch run
      (Session 13 P2 — see below)
    - source_url shown as monospace + click-to-copy
    - extraction spec pretty-printed as JSON
    - production bindings pretty-printed as JSON
    - authored_at and authored_by in a footer strip

  Per the handoff, the structured fields render as JSON rather than
  per-mode bespoke components — that's a future polish session if
  this view ever ships beyond the developer audience.

  Per-recipe outcome badge (Session 13 P2)
  -----------------------------------------

  The badge below the recipe header shows the outcome of the *most
  recent* fetch run for this recipe (matched by `recipe_id` against
  `plans.fetchReport.outcomes`). This is the "show at a glance which
  recipes are working and which aren't" affordance from the Session
  13 handoff. Four states:

    - ok    → "N records" badge in positive green
    - skip  → "skipped" in chrome grey, with the reason expandable
    - fail  → "failed @ stage" in negative red, with the message
              expandable
    - none  → "no fetch run yet" in tertiary grey, no expand

  Tone semantics + label/detail strings are shared with FetchReport
  via `$lib/outcomes.ts` so the two panels can't drift in their
  rendering of the same wire shape.

  The badge reflects only the most recent fetch run's outcomes —
  earlier runs aren't surfaced here (the run history strip in
  FetchReport covers that). When the user clicks "Run fetch" again,
  the badge updates with the new run's outcome for this recipe.

  BAKED badge (Session 18, ADR 0007 Amendment 3)
  -----------------------------------------------

  When a recipe carries a `static_payload`, the runtime serves the
  baked bytes to extraction in place of an HTTP fetch. The recipe
  produces the same records on every fetch until re-authored — there
  is no live freshness path. The freshness model is materially
  different from the common HTML-addressable case, so the UI shows
  it explicitly:

    - **BAKED chip** in the recipe head, next to the source_id.
      Tooltip explains the bake-time-frozen freshness contract.
    - **Collapsible payload preview** below the produces block,
      showing the raw baked bytes the runtime feeds to extraction.
      Defaults closed (the payload can be large; users open it when
      diagnosing).

  The badge is shown iff `recipe.static_payload != null`. Empty-
  string would not occur on the wire — the validator at
  `build_validated_recipe` collapses empty / whitespace-only strings
  to None before storage, and the executor's short-circuit treats
  None as "fetch normally."

  STUB-AUTHORED chip (Session 21, ADR 0014)
  ------------------------------------------

  When `recipe.authored_from === 'stub_excerpt'`, the recipe was
  authored without the source's actual response bytes — the LLM
  saw a synthesized stub describing the source from the plan + URL
  only, and guessed the response shape. This happens when the
  source has no `endpoint_hint`, the hint is unparseable, or the
  pre-fetch returned an HTTP/transport error (e.g. the GDELT 429
  case from the Session 20 live run).

  Stockpile's central architectural claim — every number traceable
  to a source (ADR 0007) — depends on the user being able to assess
  the *quality* of the trace. A stub-authored recipe is a *guess*
  at the response shape; one authored from real bytes is *grounded*.
  The chip surfaces the difference at a glance.

    - **STUB-AUTHORED chip** in the recipe head. `--signal-warning`
      hue (warranted attention, not destructive). Tooltip explains
      the gap and points at the recovery path: re-run fetch when
      the source becomes reachable.
    - **Hint banner** in the flag dialog (RecipeFlagDialog) when
      flagging a stub-authored recipe — context the operator should
      see *before* typing a feedback note.

  No chip renders for `'fetched_bytes'` (the optimistic case;
  absence of the chip is the signal) or `'unknown'` (legacy
  pre-ADR-0014 rows; showing a chip for "we don't know" would
  create noise on every existing recipe the moment migration v10
  runs).

  ADR 0014 §"What the user does NOT see (option 3, deferred)"
  documents why this is a passive surface only — the silent
  re-author-on-real-bytes path is deferred with explicit amendment
  triggers; today the chip is the user's hook for triggering the
  manual path themselves.

  ITERATES chip (Session 38, ADR 0016)
  -------------------------------------

  When `recipe.iterator !== null`, the recipe is a listing-shaped
  recipe: the runtime evaluates the iterator's selector against the
  fetched document to obtain N matches, then evaluates the inner
  `extraction` once per match scoped to that match's sub-tree,
  producing N records per fetch instead of 1. This is the post-
  ADR-0015 cold-start case (Nature subjects, RSS feeds, arXiv
  recent, news beats, agency publication indexes).

    - **ITERATES chip** in the recipe head. `--signal-info` hue
      (informational, not degraded — the recipe is healthy and
      structurally correct; the cardinality is just non-default).
      Same baseline + sizing as BAKED / STUB-AUTHORED so the three
      render coherently when more than one applies.
    - **Iterator details block** between the extraction and produces
      blocks, open by default. Renders the iterator's serialized
      ExtractionSpec as pretty-printed JSON — same opacity-on-the-
      wire posture as `extraction`. Open by default because for
      iterator recipes the iterator's selector is more load-bearing
      for "did the LLM pick the right card boundary?" debugging
      than the inner extraction is.

  No chip and no block render for scalar recipes (`iterator ===
  null`) — the absence is the signal: the recipe produces one
  record per binding per fetch, the pre-Session-38 contract.

  ADR 0016 §"Per-match dedup becomes load-bearing" notes that
  iterator records carry a per-record `dedup_key` computed from the
  binding's `dedup_key_field`. The dedup_key isn't surfaced in this
  panel today — it's a runtime-only concern, visible only through
  storage queries.

  Empty state
  -----------

  The panel renders nothing when the selected plan has no recipes
  yet. That's the legitimate state for an accepted-but-not-fetched
  plan; the user sees a "run fetch to author them" hint via the
  RunFetchButton elsewhere on the screen. No need to add a "no
  recipes" message that would clutter the panel for the much more
  common case (just-classified plan).

  When the parent plan has no selection at all, this component also
  renders nothing — it never appears as a leaked artefact across
  selection boundaries because plans.recipes is reset on selectPlan.

  RESPONSE BYTES inline (Session 31, upgraded Session 32)
  --------------------------------------------------------

  Track A (ADR 0012 amendment 1) made the executor capture the bytes
  the runtime saw at the moment a recipe failed at apply, into the
  `recipe_fetch_attempts` table. Until Session 31 those bytes were
  reachable only through the manual re-author dialog — the operator
  had to click `re-author` to even see what came back. The Session 31
  test run made the gap visible: a `comtrade` recipe failed at apply
  with `bytes did not parse as JSON: expected value at line 1 column 1`
  against `https://comtradeplus.un.org/TradeFlow`. Diagnosing this
  required the operator to either click re-author (heavy) or query
  DuckDB by hand. Both punish the developer for using the app.

  This section adds an inline `response bytes` expander on every
  failed-apply recipe row. Click → fetches the latest captured
  attempt for the recipe (same `latest_attempt_for_recipe` command
  the re-author dialog uses; no new wire surface, no new storage
  read), then renders:

    - a content-type chip (JSON / HTML / XML / CSV / TEXT / EMPTY); and
    - the byte count; and
    - a `<pre>` of the captured excerpt (capped at 64 KiB at
      capture time per `MAX_EXCERPT_BYTES`).

  The chip is the load-bearing diagnostic. For the comtrade case
  above the chip reads `HTML` while the failure message says
  `did not parse as JSON` — the operator sees the mismatch in two
  glances and writes a flag note ("source returns HTML, not JSON;
  check for a separate JSON API") in five seconds.

  Session 32 — chip is now header-authoritative
  ----------------------------------------------

  The Session-31 chip was heuristic: it inspected the first
  non-whitespace byte of the captured excerpt and guessed. Honest
  about the limitation but lossy in two specific ways: it couldn't
  tell `text/csv` from arbitrary text (a CSV row is just commas),
  and it couldn't surface what the server actually claimed when the
  bytes alone were ambiguous (`application/javascript` vs
  `application/json` both start with `{`).

  Session 32 threads the response `Content-Type` header from
  `SecureHttpClient::get_with_headers` through
  `HttpFetcher::fetch_bytes_with_meta` and `BackoffOutcome::Bytes`
  into the `recipe_fetch_attempts.response_content_type` column
  (migration 0014), and out through `RecipeFetchAttemptDto` to
  this component. The chip now reads the header when present and
  recognized — solid border, no `?` glyph, tooltip showing the raw
  header value — and falls back to the heuristic when the header
  is absent or unrecognized — dashed border with a `?` glyph and a
  tooltip explaining the fallback. CSV joins the chip's enum as a
  first-class shape; `text/csv` against a `csv_cell` recipe is now
  legible at a glance.

  The fallback path matters: pre-migration-0014 attempt rows have
  no header captured, `static_payload` recipes never had a
  transport, and some servers omit the header entirely. The chip
  stays useful in all three cases — just with a dashed border so
  the operator knows the reading is best-effort.

  We render only on failed-apply outcomes (mirrors the re-author
  button's gate). For other failure stages — Failed @ fetch (no
  body was read) or Failed @ insert (storage-side, the bytes
  parsed) — the bytes either don't exist or wouldn't help, so we
  hide the affordance rather than surface a misleading "no bytes
  captured" placeholder on every row in those states.

  State is held in `attemptByRecipeId` with a five-state alphabet:
  `undefined` (not loaded), `'loading'`, `'error'` (load failed),
  `null` (no attempt row exists), or the attempt DTO. The button
  → loading → resolved transition is one-shot per recipe id. The
  state map is component-scoped: it survives across re-runs of
  fetch on the same plan (so a previously-loaded attempt stays
  visible) and is reset only when the user navigates away from the
  plan (the parent's `selectPlan` flow re-mounts the panel by
  changing `plans.recipes`). The known staleness window is "user
  loaded bytes for recipe X, then ran fetch again which produced a
  new attempt row for X, and the panel still shows the previous
  load" — fine for diagnosis, since the new attempt's outcome
  surfaces in the outcome strip and the re-author dialog has its
  own always-fresh load. If empirical use shows the staleness
  bites, the runes store can clear the map on fetch-run completion;
  defer until then.

  No new ADR. ADR 0012 amendment 1 already authorized capture; the
  Session-32 upgrade adds one nullable column to the same table and
  one field to the same DTO. ts-rs regenerates `RecipeFetchAttemptDto.ts`
  on `cargo test --package situation_room-api`; the patch ships the
  regenerated file so `npm run check` passes immediately.
-->
<script lang="ts">
  import { plans, flagRecipe, reauthorRecipe } from '$stores/plans.svelte';
  import type { RecipeDto } from '$lib/api/types/RecipeDto';
  import type { RecipeFetchAttemptDto } from '$lib/api/types/RecipeFetchAttemptDto';
  import {
    outcomeTone,
    outcomeLabel,
    outcomeDetail,
    outcomeForRecipe,
  } from '$lib/outcomes';
  import { latestAttemptForRecipe, asCommandError } from '$lib/api/client';
  import RecipeFlagDialog from '$components/dialogs/RecipeFlagDialog.svelte';
  import ReauthorDialog from '$components/dialogs/ReauthorDialog.svelte';

  // ADR 0013: which recipe (if any) currently has its flag dialog
  // open. We key by `source_id` rather than `recipe.id` because the
  // feedback channel itself is keyed per (plan, source) — opening
  // the dialog on the same source's later recipe version should land
  // on the same persisted note. Null = no dialog open.
  let flagDialogSourceId: string | null = $state(null);
  let flagSubmitting = $state(false);

  function openFlagDialog(sourceId: string) {
    flagDialogSourceId = sourceId;
  }
  function closeFlagDialog() {
    flagDialogSourceId = null;
  }
  async function onFlagSubmit(note: string) {
    if (!flagDialogSourceId) return;
    flagSubmitting = true;
    try {
      const ok = await flagRecipe(flagDialogSourceId, note);
      if (ok) flagDialogSourceId = null;
      // On failure: dialog stays open, plans.error renders in the
      // parent banner, user can edit + resubmit.
    } finally {
      flagSubmitting = false;
    }
  }

  // Track A, ADR 0012 amendment 1: which recipe (if any) currently
  // has its re-author dialog open. Keyed by `recipe.id`, not
  // `source_id`, because the dialog operates on the *specific* prior
  // recipe row — the bytes excerpt and failure message belong to
  // that exact recipe's last fetch attempt, not to a (plan, source)
  // pair (which has its own current head). Null = no dialog open.
  let reauthorDialogRecipe: RecipeDto | null = $state(null);
  // The latest captured attempt for the recipe whose dialog is open.
  // Loaded asynchronously when the dialog opens; null while pending.
  // The dialog mounts before the load resolves and shows a small
  // "loading" placeholder in the bytes panel (the dialog is built to
  // tolerate empty bytes anyway — see ReauthorDialog's `bytesEmpty`
  // path).
  let reauthorAttempt: RecipeFetchAttemptDto | null = $state(null);
  let reauthorLoadingAttempt = $state(false);
  let reauthorSubmitting = $state(false);

  async function openReauthorDialog(recipe: RecipeDto) {
    reauthorDialogRecipe = recipe;
    reauthorAttempt = null;
    reauthorLoadingAttempt = true;
    try {
      reauthorAttempt = await latestAttemptForRecipe(recipe.id);
    } catch (e) {
      // The capture-fetch is non-fatal — the dialog still opens with
      // empty bytes and the failure-message panel from the outcome.
      // Surface the error in the global banner so the operator sees
      // it but can still proceed.
      plans.error = asCommandError(e);
    } finally {
      reauthorLoadingAttempt = false;
    }
  }

  function closeReauthorDialog() {
    reauthorDialogRecipe = null;
    reauthorAttempt = null;
    reauthorLoadingAttempt = false;
  }

  // Session 31: inline response-bytes affordance on failed-apply
  // recipe rows. The map's value alphabet is:
  //   - undefined → not loaded yet (button is shown)
  //   - 'loading' → fetch in flight
  //   - 'error'   → fetch threw; banner shows the error
  //   - null      → no attempt row exists in storage (rare —
  //                 fetch failed before the body was read, or
  //                 the row predates Track A's capture)
  //   - DTO       → resolved; render bytes
  // Keyed by recipe.id (not source_id) because attempts are
  // per-recipe-latest in storage and a (plan, source) can have
  // multiple recipe rows in its lineage.
  type AttemptCellState =
    | undefined
    | 'loading'
    | 'error'
    | null
    | RecipeFetchAttemptDto;
  let attemptByRecipeId: Record<string, AttemptCellState> = $state({});

  async function loadRecipeAttempt(recipeId: string) {
    // Idempotent: if we've already started or finished a load for
    // this recipe, do nothing. The button that calls this is hidden
    // once the state advances out of `undefined`, so this is a
    // belt-and-suspenders guard against double-clicks during the
    // small window before the loading state renders.
    if (attemptByRecipeId[recipeId] !== undefined) return;
    attemptByRecipeId[recipeId] = 'loading';
    try {
      const attempt = await latestAttemptForRecipe(recipeId);
      attemptByRecipeId[recipeId] = attempt; // DTO or null
    } catch (e) {
      // Store the error sentinel so the row shows a stable "could
      // not load" message instead of reverting to the button (which
      // would invite an infinite click → fail loop). The error
      // banner via plans.error gives the operator the diagnostic.
      attemptByRecipeId[recipeId] = 'error';
      plans.error = asCommandError(e);
    }
  }

  /**
   * Shape categories surfaced by the response-bytes chip. The set
   * is closed and aligned with the extraction-mode enum on the
   * recipe side: a `JSON` chip across from a `json_path` recipe is
   * a shape match; a `HTML` chip across from a `json_path` recipe
   * is the diagnostic the operator wants to see.
   *
   * Session 32: `csv` joined the set when the chip became
   * header-aware. A `text/csv` response against a `csv_cell`
   * recipe is a shape match, even when the bytes start with a
   * comma or quote and the older heuristic byte-sniffer would
   * have called it `text`. Adding the variant when the underlying
   * authority arrived was cheaper than carrying a "the heuristic
   * doesn't recognize CSV" caveat for every operator who hits a
   * CSV apply failure.
   */
  type Shape = 'json' | 'html' | 'xml' | 'csv' | 'text' | 'empty';

  /**
   * The chip's value plus whether it came from the server's claim
   * (the response Content-Type header) or from a heuristic
   * inspection of the bytes themselves. Surfacing the source lets
   * the operator know whether a surprising chip ("HTML on a
   * json_path recipe") is the server telling the truth or a guess
   * the chip made when the header was absent.
   */
  type ShapeReading = {
    label: Shape;
    source: 'header' | 'heuristic';
    /**
     * The raw `Content-Type` header value when source === 'header'.
     * `null` when source === 'heuristic'. Surfaced in the chip's
     * tooltip so the operator can see what the server claimed
     * verbatim — `application/json; charset=utf-8` is more
     * diagnostic than just `JSON`.
     */
    rawContentType: string | null;
  };

  /**
   * Map a raw `Content-Type` header value to a `Shape`. Strips
   * parameters (`; charset=...`, `; boundary=...`), lowercases the
   * type/subtype, and matches against the closed set of MIME
   * shapes the chip recognizes.
   *
   * Returns `null` when the value doesn't match anything we have
   * a chip for — the caller falls back to the heuristic sniffer.
   * `text/plain` maps to `text` (chip = TEXT) rather than `null`
   * because the operator should see "server told us text/plain"
   * as a real reading, not as "we don't know."
   */
  function shapeFromContentType(value: string): Shape | null {
    // Strip parameters and whitespace, lowercase.
    const head = value.split(';')[0].trim().toLowerCase();
    if (head === '') return null;
    // JSON: standard + the half-dozen +json suffixes commonly seen
    // (application/ld+json, application/vnd.api+json, etc.).
    if (head === 'application/json' || head === 'text/json' || head.endsWith('+json')) {
      return 'json';
    }
    if (head === 'text/html' || head === 'application/xhtml+xml') {
      return 'html';
    }
    // XML covers bare XML, RSS, Atom, and the +xml suffix family
    // (application/rss+xml, application/atom+xml, etc.).
    if (
      head === 'application/xml' ||
      head === 'text/xml' ||
      head.endsWith('+xml')
    ) {
      return 'xml';
    }
    if (head === 'text/csv' || head === 'application/csv') {
      return 'csv';
    }
    if (head === 'text/plain') {
      return 'text';
    }
    return null;
  }

  /**
   * Heuristic content-type detection from the first non-whitespace
   * byte. Pre-Session-32 fallback for when the response Content-Type
   * header isn't present (legacy attempts predating migration 0014,
   * servers that omit the header, or `static_payload` recipes that
   * have no transport).
   *
   * The heuristic is structural: starts-with `{` or `[` → JSON;
   * starts-with `<?xml` / `<rss` / `<feed` / `<atom` → XML; bare
   * `<` → HTML; null/whitespace → EMPTY; anything else → TEXT.
   * It does NOT detect CSV: distinguishing CSV from arbitrary text
   * by byte-sniffing alone is unreliable (a CSV row is just
   * comma-separated text), and the chip's authority for CSV comes
   * from the `text/csv` header path. When the heuristic is invoked
   * on actual CSV bytes, the chip reads TEXT — honest about the
   * limitation.
   */
  function shapeFromBytes(
    bytes: string | null | undefined,
  ): Shape {
    if (bytes === null || bytes === undefined) return 'empty';
    const trimmed = bytes.trimStart();
    if (trimmed.length === 0) return 'empty';
    const first = trimmed[0];
    if (first === '{' || first === '[') return 'json';
    if (first === '<') {
      const head = trimmed.slice(0, 200).toLowerCase();
      if (
        head.startsWith('<?xml') ||
        head.startsWith('<rss') ||
        head.startsWith('<feed') ||
        head.startsWith('<atom')
      ) {
        return 'xml';
      }
      return 'html';
    }
    return 'text';
  }

  /**
   * The chip's full reading: prefer the server's Content-Type when
   * present and recognized, fall back to the byte-sniffer otherwise.
   * Session 32. Documented as the load-bearing authority elevation
   * over Session 31's heuristic-only chip.
   */
  function classifyShape(attempt: RecipeFetchAttemptDto): ShapeReading {
    const ct = attempt.response_content_type;
    if (ct !== null && ct !== undefined && ct.trim() !== '') {
      const fromHeader = shapeFromContentType(ct);
      if (fromHeader !== null) {
        return { label: fromHeader, source: 'header', rawContentType: ct };
      }
      // Header was present but we don't recognize the MIME type
      // (e.g. `application/octet-stream`, vendor-specific shapes).
      // Fall through to the heuristic so the operator still gets a
      // best-effort label, but tooltip-surface the raw header value
      // so the unrecognized MIME is visible.
      const fromBytes = shapeFromBytes(attempt.bytes_excerpt);
      return { label: fromBytes, source: 'heuristic', rawContentType: ct };
    }
    return {
      label: shapeFromBytes(attempt.bytes_excerpt),
      source: 'heuristic',
      rawContentType: null,
    };
  }

  /**
   * Tooltip text for the response-shape chip. Distinguishes the
   * three cases the operator cares about: header-authoritative,
   * heuristic-with-unknown-header, heuristic-with-no-header.
   */
  function shapeTooltip(reading: ShapeReading): string {
    if (reading.source === 'header' && reading.rawContentType !== null) {
      return `from response header: ${reading.rawContentType}`;
    }
    if (reading.source === 'heuristic' && reading.rawContentType !== null) {
      return `heuristic from first byte; server returned unrecognized Content-Type: ${reading.rawContentType}`;
    }
    return 'heuristic from first byte; no Content-Type header was captured';
  }

  /**
   * Format byte count with a friendly suffix. Operates on the
   * decoded string's `.length` (UTF-16 code units), which is
   * approximately byte-equivalent for typical API responses
   * (mostly ASCII) and a slight under-count for prose-heavy
   * non-ASCII. The exact byte count from storage's
   * `MAX_EXCERPT_BYTES` truncation isn't surfaced separately;
   * a 64 KiB excerpt is the wire-side cap so any value reading
   * "64.0 KB" is plausibly truncated.
   */
  function responseLengthLabel(
    bytes: string | null | undefined,
  ): string {
    if (!bytes) return '0 B';
    const len = bytes.length;
    if (len < 1024) return `${len} B`;
    if (len < 1024 * 1024) return `${(len / 1024).toFixed(1)} KB`;
    return `${(len / (1024 * 1024)).toFixed(1)} MB`;
  }

  async function onReauthorSubmit(note: string | null) {
    if (!reauthorDialogRecipe) return;
    reauthorSubmitting = true;
    try {
      const ok = await reauthorRecipe(reauthorDialogRecipe.id, note);
      if (ok) {
        reauthorDialogRecipe = null;
        reauthorAttempt = null;
      }
      // On failure: dialog stays open, plans.error renders in the
      // parent banner, user can edit + resubmit or cancel.
    } finally {
      reauthorSubmitting = false;
    }
  }

  function shortId(id: string): string {
    // UUIDv7s are too long for inline display; first 8 chars are
    // unique enough within a single plan's recipe list.
    return id.slice(0, 8);
  }

  function formatAuthoredAt(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const yyyy = d.getFullYear();
    const mm = String(d.getMonth() + 1).padStart(2, '0');
    const dd = String(d.getDate()).padStart(2, '0');
    const hh = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    return `${yyyy}-${mm}-${dd} ${hh}:${min}`;
  }

  function prettyJson(value: unknown): string {
    // The wire shape carries `extraction` and `produces` as `unknown`
    // (raw `serde_json::Value` on the Rust side). The frontend
    // renders them as 2-space-indented JSON for readability.
    //
    // If serialization fails for any reason — circular refs are the
    // only realistic case, and they shouldn't appear in DTO data —
    // the panel surfaces the error rather than crashing the render.
    try {
      return JSON.stringify(value, null, 2);
    } catch (e) {
      return `// could not stringify: ${e instanceof Error ? e.message : String(e)}`;
    }
  }
</script>

{#if plans.recipes.length > 0}
  <section class="recipes-panel" aria-label="Authored recipes">
    <header class="head">
      <span class="label">recipes</span>
      <span class="count">{plans.recipes.length}</span>
    </header>

    <div class="list">
      {#each plans.recipes as recipe (recipe.id)}
        {@render recipeCard(recipe)}
      {/each}
    </div>
  </section>
{/if}

{#snippet recipeCard(recipe: RecipeDto)}
  {@const outcome = outcomeForRecipe(recipe.id, plans.fetchReport?.outcomes)}
  {@const tone = outcomeTone(outcome)}
  {@const label = outcomeLabel(outcome)}
  {@const detail = outcomeDetail(outcome)}
  {@const feedback = plans.recipeFeedback[recipe.source_id]}
  <article class="recipe">
    <header class="recipe-head">
      <span class="source-id">{recipe.source_id}</span>
      {#if recipe.static_payload !== null}
        <!--
          BAKED badge — visible when the recipe carries a
          static_payload. The tooltip explains the bake-time-frozen
          freshness contract so users understand why the recipe
          will produce identical records every fetch.

          ADR 0007 Amendment 3 §"freshness model is explicit in
          the UI" — same data shape, different freshness, made
          visible.
        -->
        <span
          class="baked-badge"
          title="Bake-time-frozen: the runtime serves baked bytes to extraction in place of an HTTP fetch. This recipe will produce the same records on every fetch until re-authored. ADR 0007 Amendment 3."
        >BAKED</span>
      {/if}
      {#if recipe.authored_from === 'stub_excerpt'}
        <!--
          STUB-AUTHORED chip — ADR 0014. Visible when the recipe
          was authored without the source's actual response bytes
          (no endpoint_hint, unparseable hint, or pre-fetch failure).
          The tooltip names the gap and the recovery path.

          No onclick: the chip is passive, surfacing the fact
          without committing the user to a remediation policy.
          The recovery path is "run fetch when the source is
          reachable, the next authoring run will see real bytes" —
          which the user does via the existing RunFetchButton.

          ADR 0014 §"What the user does NOT see (option 3, deferred)"
          documents why this isn't a button: silent self-healing
          would change ADR 0007's runtime-is-LLM-free invariant
          and warrants its own ADR amendment, gated on
          operational data we don't have yet.
        -->
        <span
          class="stub-authored-chip"
          title="Authored from a fallback description, not the source's actual response. The recipe is a guess at the response shape. If the source becomes reachable, run fetch again — a future session may surface a 're-author from real bytes' path. ADR 0014."
        >STUB-AUTHORED</span>
      {/if}
      {#if recipe.iterator !== null}
        <!--
          ITERATES chip — ADR 0016 (Session 38). Visible when the
          recipe carries an iterator (a listing-shaped source: each
          fetch returns N items, the recipe produces N records).
          Distinguishes scalar recipes (one record per fetch) from
          iterator recipes at a glance, so the operator's reading
          of `records_produced` in the fetch report aligns with
          the recipe's structural cardinality.

          Same passive-chip posture as BAKED / STUB-AUTHORED: no
          onclick, the chip is informational. The iterator's
          actual selector renders inside the iterator details
          block below.
        -->
        <span
          class="iterates-chip"
          title="This recipe iterates: the runtime selects N matches with the iterator's selector, then evaluates the extraction once per match. Produces N records per fetch instead of 1. ADR 0016."
        >ITERATES</span>
      {/if}
      {#if feedback}
        <!--
          FLAGGED chip — ADR 0013. Visible when the operator has
          attached a feedback note for this (plan, source) pair.
          Clicking opens the dialog to edit; the note's full text
          shows on hover via the title attribute. The chip uses
          --signal-info because the recipe is annotated, not
          discarded — the freshness/lifecycle isn't broken; the
          author just has additional context for the next attempt.
        -->
        <button
          type="button"
          class="flagged-chip"
          title={feedback.note}
          onclick={() => openFlagDialog(recipe.source_id)}
        >FLAGGED</button>
      {/if}
      {#if recipe.prior_recipe_id !== null}
        <!--
          RE-AUTHORED chip — Track A, ADR 0012 amendment 1. Visible
          when this recipe row supersedes a prior one through the
          manual re-author flow. Cites the prior recipe's short id;
          hover surfaces the persisted reauthor_reason so the
          operator sees why this recipe exists in the form it does.

          --signal-info hue: lineage is informational (the recipe is
          part of a chain) rather than a defect signal. Same family
          as the FLAGGED chip, distinct content.

          No onclick today: the prior recipe id is exposed as text
          for the operator to grep / look up in storage directly.
          A future session may turn this into a button that opens
          a drilldown view of the version chain — when that pane
          earns its weight, the chip becomes the entry point.
        -->
        <span
          class="reauthored-chip"
          title={recipe.reauthor_reason ?? `Re-authored from recipe ${shortId(recipe.prior_recipe_id)}`}
        >RE-AUTHORED FROM {shortId(recipe.prior_recipe_id)}</span>
      {/if}
      <span class="recipe-id">{shortId(recipe.id)}</span>
      <!--
        Flag button — always present so the operator can attach a
        note even on a recipe that's never failed (e.g. a wrong-
        shape extraction the user noticed by reading the records).
        When already flagged, the FLAGGED chip is the primary edit
        affordance; this button is a secondary entry point for the
        unflagged state.
      -->
      {#if !feedback}
        <button
          type="button"
          class="flag-button"
          title="Attach a note about what's wrong with this recipe — fed into the next authoring attempt for this source."
          onclick={() => openFlagDialog(recipe.source_id)}
        >flag</button>
      {/if}
      <!--
        Re-author button — Track A, ADR 0012 amendment 1. Visible
        only when the latest fetch outcome is `Failed @ apply` (the
        canonical Class B / Class B-adjacent failure shape). The
        operator clicks; the dialog opens, loads the latest captured
        attempt for this recipe (bytes + failure message), and lets
        the operator add an optional diagnosis note before
        triggering the LLM call.

        Why not visible for `Failed @ fetch` / `Failed @ insert`:
          * Fetch failures have no captured bytes (the runtime never
            got a body); re-authoring would guess at the response
            shape exactly the way ADR 0012 forbids.
          * Insert failures are storage-side; re-authoring the
            extraction can't fix a DB problem.
        Both surface in the existing FetchReport panel; the operator
        addresses them through the source-curation path, not through
        re-authoring.
      -->
      {#if outcome && outcome.kind === 'failed' && outcome.stage === 'apply'}
        <button
          type="button"
          class="reauthor-button"
          title="Open the re-author dialog: shows the failure message + the bytes the runtime saw, lets you add a diagnosis note, then asks the LLM for a corrected recipe. ADR 0012 amendment 1."
          onclick={() => openReauthorDialog(recipe)}
        >re-author</button>
      {/if}
    </header>

    <!--
      Outcome strip. Always present (even in 'none' state) so the
      vertical rhythm of the cards stays consistent across plans
      with and without fetch runs. The expandable detail only
      renders when there's something to show.
    -->
    <div class="outcome-strip" data-tone={tone}>
      <span class="outcome-dot" aria-hidden="true"></span>
      <span class="outcome-label">{label}</span>
      {#if detail}
        <details class="outcome-detail">
          <summary>details</summary>
          <pre>{detail}</pre>
        </details>
      {/if}
    </div>

    <div class="kv-row">
      <span class="k">URL</span>
      <code class="url">{recipe.source_url}</code>
    </div>

    <details class="block" open>
      <summary>extraction</summary>
      <pre>{prettyJson(recipe.extraction)}</pre>
    </details>

    {#if recipe.iterator !== null}
      <!--
        ADR 0016: iterator block. Renders the iterator's serialized
        ExtractionSpec as pretty-printed JSON — same opacity-on-the-
        wire posture as `extraction`, same render style. Open by
        default because for iterator-bearing recipes the iterator's
        selector is more load-bearing for "did the LLM pick the
        right card boundary?" debugging than the inner extraction
        is — the inner extraction's failure mode is "no leaf within
        a card", which is local; the iterator's failure mode is
        "wrong cards entirely", which is structural.
      -->
      <details class="block" open>
        <summary>iterator</summary>
        <pre>{prettyJson(recipe.iterator)}</pre>
      </details>
    {/if}

    <details class="block">
      <summary>produces</summary>
      <pre>{prettyJson(recipe.produces)}</pre>
    </details>

    <!--
      Session 31: inline response-bytes affordance.

      Visible only when the latest fetch outcome is Failed @ apply
      — same gate as the re-author button, same justification: the
      bytes only exist for that failure stage (Track A, ADR 0012
      amendment 1). Fetch failures have no body to capture; insert
      failures had a parsed body that landed records (we'd be
      surfacing bytes that aren't the diagnostic).

      The state machine (button → loading → resolved | error | null)
      is described in the script's `attemptByRecipeId` block. The
      content-type chip in the resolved branch's summary is the
      load-bearing diagnostic — when it disagrees with what the
      recipe's extraction mode expected (e.g. `HTML` while the
      recipe is `json_path`), the operator sees the mismatch
      without reading any bytes.
    -->
    {#if outcome && outcome.kind === 'failed' && outcome.stage === 'apply'}
      {@const attemptState = attemptByRecipeId[recipe.id]}
      <div class="block response-bytes-block">
        {#if attemptState === undefined}
          <button
            type="button"
            class="response-bytes-toggle"
            title="Show the response bytes the runtime saw at apply time. Captured at fetch time per Track A / ADR 0012 amendment 1."
            onclick={() => loadRecipeAttempt(recipe.id)}
          >▸ show response bytes</button>
        {:else if attemptState === 'loading'}
          <div class="response-bytes-status">loading response bytes…</div>
        {:else if attemptState === 'error'}
          <div class="response-bytes-status response-bytes-error">
            could not load response bytes — see error banner above
          </div>
        {:else if attemptState === null}
          <!--
            null means the storage call returned None — no attempt
            row exists. Two reasons this can happen for a
            failed-apply outcome: (a) the row predates Track A's
            capture (migration 0013, Session 25); (b) the runtime
            was patched in a way that bypassed the capture path.
            Both are diagnostic dead-ends from the UI; we surface
            the gap honestly rather than hide the affordance.
          -->
          <div class="response-bytes-status">
            no response bytes were captured for this attempt
          </div>
        {:else}
          {@const reading = classifyShape(attemptState)}
          <details class="response-bytes-details" open>
            <summary>
              <span class="response-summary-label">response bytes</span>
              <span
                class="response-shape-chip"
                data-shape={reading.label}
                data-source={reading.source}
                title={shapeTooltip(reading)}
              >
                {reading.label.toUpperCase()}{reading.source === 'heuristic' ? '?' : ''}
              </span>
              <span class="response-length">
                {responseLengthLabel(attemptState.bytes_excerpt)}
              </span>
            </summary>
            {#if attemptState.bytes_excerpt}
              <pre class="response-bytes-pre">{attemptState.bytes_excerpt}</pre>
            {:else}
              <div class="response-bytes-empty">
                (response body was empty or non-UTF-8)
              </div>
            {/if}
          </details>
        {/if}
      </div>
    {/if}

    {#if recipe.static_payload !== null}
      <!--
        Baked-payload preview — the raw bytes the runtime serves to
        extraction in place of an HTTP fetch. Closed by default
        (payloads may be large); users open it when diagnosing why
        a baked recipe produced unexpected records.

        Rendered as a plain <pre> rather than parsed-and-pretty —
        the runtime hands these bytes to apply() exactly as written,
        and any reformatting here would mislead the user about what
        the runtime actually saw.
      -->
      <details class="block baked-block">
        <summary>baked payload</summary>
        <pre>{recipe.static_payload}</pre>
      </details>
    {/if}

    <footer class="recipe-foot">
      <span>authored {formatAuthoredAt(recipe.authored_at)}</span>
      <span>by {recipe.authored_by}</span>
      <span>v{recipe.version}</span>
    </footer>
  </article>
{/snippet}

{#if flagDialogSourceId !== null}
  <!--
    Flag dialog mount. Keyed implicitly by the {#if} block — the
    dialog mounts fresh whenever flagDialogSourceId becomes
    non-null, so the `initial` prop captured at construction time
    is the right value (matches RejectDialog's
    `untrack(() => initial)` pattern).

    `initial` is the existing note text for this source if there
    is one, else empty. Submitting empty routes through
    `flagRecipe` → `clearRecipeFeedback`.

    `authoredFrom` is sourced from the matching recipe row — the
    dialog renders a hint banner when the recipe was stub-authored
    (ADR 0014). `plans.recipes` is ordered newest-first by the
    storage layer (`ORDER BY authored_at DESC, version DESC`), so
    `find` against `source_id` returns the current recipe — the
    one whose feedback the operator is editing. Falling back to
    `'unknown'` keeps the dialog's prop type stable when the
    recipe isn't found (an edge case: the recipe was deleted
    between opening and rendering, which is currently not
    reachable but the fallback costs nothing).
  -->
  <RecipeFlagDialog
    sourceId={flagDialogSourceId}
    initial={plans.recipeFeedback[flagDialogSourceId]?.note ?? ''}
    authoredFrom={plans.recipes.find(r => r.source_id === flagDialogSourceId)?.authored_from ?? 'unknown'}
    submitting={flagSubmitting}
    onSubmit={onFlagSubmit}
    onCancel={closeFlagDialog}
  />
{/if}

{#if reauthorDialogRecipe !== null}
  <!--
    Re-author dialog mount — Track A, ADR 0012 amendment 1.

    The dialog mounts fresh whenever `reauthorDialogRecipe` becomes
    non-null. The `latestAttemptForRecipe` call kicked off in
    `openReauthorDialog` resolves into `reauthorAttempt` while the
    dialog is open; the UI tolerates the loading window via
    ReauthorDialog's `bytesEmpty` path (the dialog is built to
    render an explanatory placeholder when `bytesExcerpt === ''`).

    `failureMessage` is sourced in priority order:
      1. The captured attempt's `failure_message` (load-bearing,
         pulled from `recipe_fetch_attempts` — exactly what the
         executor recorded at the moment of failure).
      2. The current run's outcome `message` for the recipe (still
         the same string at the wire level since the executor
         records it from the same source).
      3. A placeholder explaining the gap.
    Most cases get (1); (2) is the fallback when the capture call
    is still in flight or returned null; (3) only fires for hand-
    edited DBs.
  -->
  {@const reauthorOutcome = outcomeForRecipe(
    reauthorDialogRecipe.id,
    plans.fetchReport?.outcomes,
  )}
  {@const reauthorMsg =
    reauthorAttempt?.failure_message ??
    (reauthorOutcome && reauthorOutcome.kind === 'failed'
      ? reauthorOutcome.message
      : '(failure message not captured)')}
  <ReauthorDialog
    sourceId={reauthorDialogRecipe.source_id}
    priorRecipeShortId={shortId(reauthorDialogRecipe.id)}
    failureMessage={reauthorMsg}
    bytesExcerpt={reauthorAttempt?.bytes_excerpt ?? ''}
    submitting={reauthorSubmitting || reauthorLoadingAttempt}
    onSubmit={onReauthorSubmit}
    onCancel={closeReauthorDialog}
  />
{/if}

<style>
  .recipes-panel {
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
    border-bottom: 1px solid var(--border-subtle);
    padding-bottom: 6px;
  }

  .count {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-tertiary);
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .recipe {
    background: var(--bg-canvas);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }

  .recipe-head {
    display: flex;
    align-items: baseline;
    /*
     * No `justify-content: space-between` — that distributes space
     * equally between every item, which breaks down when the head
     * gains a variable trailing slot (the flag button or the
     * FLAGGED chip). Instead, we push the right-side cluster
     * (recipe-id + optional flag button) via `margin-left: auto`
     * on .recipe-id; items before .recipe-id (source-id, BAKED,
     * FLAGGED chips) flow left-to-right with the gap.
     */
    gap: 8px;
  }

  .source-id {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg-primary);
    font-weight: 600;
  }

  .recipe-id {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
    font-variant-numeric: tabular-nums;
    /* push to the right; trailing items follow with the gap */
    margin-left: auto;
  }

  /*
   * BAKED badge — Session 18, ADR 0007 Amendment 3.
   *
   * A visible chip in the recipe head announcing the bake-time-frozen
   * freshness model. Sized to sit between source_id (left) and
   * recipe-id (right) without disrupting the existing baseline
   * alignment. The hover tooltip carries the freshness explanation.
   *
   * Color discipline (ADR 0006 §"color is meaning, not decoration"):
   * `--signal-warning` is the right semantic — baked recipes
   * deserve the user's attention because their freshness contract
   * differs from the default. Not negative (the recipe isn't broken)
   * and not positive (the recipe isn't healthier than a live one) —
   * warning, because it's information the user must hold in mind.
   *
   * If global.css doesn't expose `--signal-warning` yet, the
   * fallback chain via var() is `--signal-warning, --fg-secondary` —
   * the badge degrades to a neutral chip rather than vanishing.
   */
  .baked-badge {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-warning, var(--fg-secondary));
    border: 1px solid var(--signal-warning, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: help;
    /* sit on the same baseline as source-id and recipe-id */
    align-self: center;
  }

  /*
   * STUB-AUTHORED chip — Session 21, ADR 0014.
   *
   * A visible chip in the recipe head announcing the authoring-
   * provenance gap: the LLM saw a synthesized stub, not the
   * source's actual response, when this recipe was written. The
   * recipe may still be working perfectly — the chip is not a
   * verdict on correctness, just on what evidence the author had.
   *
   * Color discipline (ADR 0006 §"color is meaning, not decoration"):
   * `--signal-warning` is the right semantic — the recipe deserves
   * the user's attention because its grounding is weaker than the
   * default. Same hue as BAKED (which is also "deserves attention,
   * different freshness contract"); the two cases are
   * conceptually adjacent ("the runtime path is meaningfully
   * non-default for this recipe"), and the visual rhyme is
   * intentional. Distinct *content* — "BAKED" vs "STUB-AUTHORED" —
   * does the disambiguation, not distinct hues.
   *
   * Same baseline + sizing as `.baked-badge` so a recipe that's
   * both baked AND stub-authored renders both chips coherently
   * left-to-right next to the source-id. (The combination is rare
   * but reachable: a baked-payload recipe whose authoring
   * pre-fetch failed before the LLM produced the bake.)
   *
   * `cursor: help` (not `pointer`) because the chip is passive —
   * no click target, just a tooltip hover. The remediation surface
   * is the existing RunFetchButton elsewhere on the screen, not
   * this chip. ADR 0014 §"What the user does NOT see" documents
   * why this is intentional.
   */
  .stub-authored-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-warning, var(--fg-secondary));
    border: 1px solid var(--signal-warning, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: help;
    align-self: center;
  }

  /*
   * ITERATES chip — Session 38, ADR 0016.
   *
   * A visible chip in the recipe head announcing the iterator
   * cardinality contract: this recipe produces N records per fetch
   * (one per iterator match), not 1. Without the chip, an
   * iterator recipe would look identical to a scalar recipe in the
   * card head — the iterator's existence would only show in the
   * details block below, which is one click away. The fetch
   * report's `records_produced: 5` would then read as surprising
   * ("five records from one recipe?"); the chip pre-frames it.
   *
   * Color discipline (ADR 0006 §"color is meaning, not decoration"):
   * `--signal-info` rather than `--signal-warning`. The iterator
   * isn't a freshness compromise (BAKED) or an authoring gap
   * (STUB-AUTHORED) — it's an informational descriptor of the
   * recipe's structural cardinality, in the same family as the
   * FLAGGED chip below (which is also "the recipe has a
   * non-default property the user should know about, but it's not
   * degraded"). Same hue as FLAGGED to mark "informational, not
   * degraded"; distinct *content* does the disambiguation.
   *
   * Same baseline + sizing as .baked-badge / .stub-authored-chip
   * so the three render coherently left-to-right when a recipe
   * carries multiple chips. `cursor: help` because passive — the
   * iterator's actual selector lives in the iterator details
   * block below.
   */
  .iterates-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    border: 1px solid var(--signal-info, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: help;
    align-self: center;
  }

  /*
   * FLAGGED chip — ADR 0013. Shown when the operator has attached a
   * feedback note for the recipe's (plan, source) pair. Same baseline
   * + sizing as .baked-badge, different hue: --signal-info because
   * the flag is informational (the recipe is annotated, not in a
   * degraded freshness state). Clickable: opens the dialog to edit.
   */
  .flagged-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    border: 1px solid var(--signal-info, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: pointer;
    align-self: center;
    transition: filter var(--duration-ui) var(--ease);
  }
  .flagged-chip:hover {
    filter: brightness(1.15);
  }

  /*
   * Flag button — secondary entry point on unflagged recipes. Sits
   * to the right of the recipe-id so it doesn't displace the head's
   * primary identifiers, and uses --fg-tertiary so it reads as
   * subordinate to the source-id and recipe-id. The FLAGGED chip
   * (when present) is the primary edit affordance; this button is
   * the "add a note" entry point.
   */
  .flag-button {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 2px;
    color: var(--fg-tertiary);
    background: transparent;
    border: 1px solid var(--border-subtle);
    cursor: pointer;
    align-self: center;
    transition: border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .flag-button:hover {
    border-color: var(--signal-info, var(--border-accent));
    color: var(--fg-primary);
  }

  /*
   * Re-authored chip — Track A, ADR 0012 amendment 1. Visible on a
   * recipe row whose `prior_recipe_id` is non-null: this row is the
   * head of a re-author chain. Mirrors the FLAGGED chip's chrome
   * (small caps mono, --signal-info hue) because the meaning is the
   * same family — informational annotation, not a defect signal.
   *
   * Wider than FLAGGED because it carries the prior recipe's short
   * id inline ("RE-AUTHORED FROM 019dee9a"). The text is the affordance:
   * the operator can copy / grep / look up the prior id directly. No
   * cursor:pointer because this is a passive label, not a button —
   * a future drilldown view of the version chain may turn it into
   * one (per the comment on the chip's mount site).
   */
  .reauthored-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    border: 1px solid var(--signal-info, var(--border-subtle));
    background: var(--bg-canvas);
    align-self: center;
    white-space: nowrap;
  }

  /*
   * Re-author button — Track A, ADR 0012 amendment 1. Visible only
   * when the latest fetch outcome for this recipe is Failed @ apply
   * (the canonical Class B failure shape). Same chrome family as the
   * flag button — both are "act on this recipe" affordances that sit
   * subordinate to the head's primary identifiers — but distinct
   * hover hue (--signal-warning) because re-authoring spends LLM
   * budget and re-authoring deserves slightly louder visual weight
   * than flagging.
   *
   * align-self: center so it lines up with the chips and the recipe-id
   * across the head row regardless of the row's natural flex height.
   */
  .reauthor-button {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 2px;
    color: var(--fg-tertiary);
    background: transparent;
    border: 1px solid var(--border-subtle);
    cursor: pointer;
    align-self: center;
    transition: border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .reauthor-button:hover {
    border-color: var(--signal-warning, var(--border-accent));
    color: var(--fg-primary);
  }

  /*
   * Outcome strip — small status row sitting between the recipe
   * head and the URL. Tone-driven: the dot color and the label
   * color shift with the wire's outcome kind. Border-left accent
   * mirrors the FetchReport outcome rows for visual consistency
   * across the two panels — same data shape, same chrome.
   *
   * Only canonical signal vars from global.css are used. ADR 0006
   * §"color is a meaning, not decoration" — and the no-hardcoded-hex
   * rule from the Session 13 handoff hard rules.
   */
  .outcome-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    background: var(--bg-panel);
    border-left: 2px solid var(--border-subtle);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 11px;
    flex-wrap: wrap;
  }
  .outcome-strip[data-tone="ok"]   { border-left-color: var(--signal-positive); }
  .outcome-strip[data-tone="skip"] { border-left-color: var(--fg-quaternary); }
  .outcome-strip[data-tone="fail"] {
    border-left-color: var(--signal-negative);
    background: var(--bg-panel-alt);
  }
  .outcome-strip[data-tone="none"] { border-left-color: var(--border-subtle); }

  .outcome-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-quaternary);
    flex-shrink: 0;
  }
  .outcome-strip[data-tone="ok"]   .outcome-dot { background: var(--signal-positive); }
  .outcome-strip[data-tone="skip"] .outcome-dot { background: var(--fg-tertiary); }
  .outcome-strip[data-tone="fail"] .outcome-dot { background: var(--signal-negative); }
  .outcome-strip[data-tone="none"] .outcome-dot { background: var(--fg-quaternary); }

  .outcome-label {
    color: var(--fg-primary);
    flex: 1;
    min-width: 0;
  }
  .outcome-strip[data-tone="none"] .outcome-label {
    color: var(--fg-tertiary);
  }
  .outcome-strip[data-tone="fail"] .outcome-label {
    color: var(--signal-negative);
  }

  .outcome-detail {
    /* Inline expandable; the summary is the click target, the pre
       is the message body. Same disclosure shape as the extraction
       and produces blocks below — keeps the recipe card's
       interaction grammar consistent. */
    flex-basis: 100%;
    margin-top: 4px;
  }
  .outcome-detail summary {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    cursor: pointer;
    user-select: none;
  }
  .outcome-detail summary:hover {
    color: var(--fg-secondary);
  }
  .outcome-detail pre {
    margin: 4px 0 0 0;
    padding: 6px 8px;
    background: var(--bg-inset);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-secondary);
    white-space: pre-wrap;
    word-break: break-word;
    /* Long failure messages (e.g. JSONPath syntax errors with the
       full expression echoed back) shouldn't blow out the card. */
    max-height: 160px;
    overflow-y: auto;
  }

  .kv-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .k {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    min-width: 32px;
  }

  .url {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
    word-break: break-all;
    background: var(--bg-panel);
    padding: 2px 4px;
    border-radius: 2px;
    flex: 1;
    /* Long URLs (especially World Bank's indicator paths) wrap
       gracefully rather than overflowing the card. */
  }

  .block {
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 6px 8px;
    background: var(--bg-panel);
  }

  .block summary {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    cursor: pointer;
    user-select: none;
  }

  .block summary:hover {
    color: var(--fg-secondary);
  }

  .block pre {
    margin: 6px 0 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-primary);
    white-space: pre-wrap;
    word-break: break-word;
    /* Constrains very deep production bindings from blowing out the
       review pane; the user can scroll within the block. */
    max-height: 320px;
    overflow-y: auto;
  }

  /*
   * Baked-payload block — same shape as the extraction/produces
   * blocks but with a left-edge accent in the warning tone, so the
   * user's eye picks up the bake-time-frozen distinction even when
   * the BAKED badge in the head has scrolled out of view.
   */
  .baked-block {
    border-left: 2px solid var(--signal-warning, var(--border-subtle));
  }
  .baked-block summary {
    color: var(--signal-warning, var(--fg-secondary));
  }
  .baked-block summary:hover {
    color: var(--fg-primary);
  }

  .recipe-foot {
    display: flex;
    gap: 12px;
    font-size: 10px;
    color: var(--fg-quaternary);
    padding-top: 4px;
    border-top: 1px solid var(--border-subtle);
  }

  /*
   * Response-bytes affordance — Session 31. Surfaces the bytes the
   * runtime saw at apply time, alongside a content-type chip for
   * fast diagnosis.
   *
   * The collapsed state is the button (`response-bytes-toggle`); the
   * expanded state is a `<details open>` with the bytes inside. Both
   * sit inside the same `.block.response-bytes-block` container so the
   * row's vertical rhythm matches the extraction / produces blocks
   * around it.
   *
   * The block shares chrome with the failed-outcome treatment in the
   * outcome strip — same `--signal-negative` left edge — so the eye
   * groups "this recipe failed" with "here is what came back" without
   * needing a separate header.
   */
  .response-bytes-block {
    border-left: 2px solid var(--signal-negative, var(--border-subtle));
  }

  .response-bytes-toggle {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 0;
    color: var(--fg-tertiary);
    background: transparent;
    border: none;
    cursor: pointer;
    transition: color var(--duration-ui) var(--ease);
  }
  .response-bytes-toggle:hover {
    color: var(--fg-primary);
  }

  .response-bytes-status {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .response-bytes-error {
    color: var(--signal-negative, var(--fg-secondary));
  }

  .response-bytes-details summary {
    /* Mirrors `.block summary` but with extra inline children for
       the chip + length, so we override the layout to flex. */
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    cursor: pointer;
    user-select: none;
  }
  .response-bytes-details summary:hover {
    color: var(--fg-secondary);
  }
  /*
   * `.response-summary-label` (the "response bytes" prefix span)
   * deliberately has no rules of its own — it inherits from
   * `.response-bytes-details summary`'s flex layout above. The class
   * is kept as a hook for future per-element tuning, but an empty
   * ruleset trips svelte-check's `Do not use empty rulesets` lint
   * (Session 31 follow-up), so the prior empty `.response-summary-label`
   * block is removed. If a future session needs to tune the label,
   * re-add the rule with content.
   */

  /*
   * Content-type chip. The hue tells the operator at a glance what
   * the response actually is, so a JSON-expecting recipe failing
   * against an HTML body reads in the chip alone.
   *
   *   JSON  → --signal-info     (most recipes target JSON; neutral-positive)
   *   HTML  → --signal-warning  (almost always a wrong-endpoint diagnosis)
   *   XML   → --signal-info     (RSS / Atom / EUR-Lex; expected for some recipes)
   *   CSV   → --signal-info     (Session 32; first-class extraction mode)
   *   TEXT  → --fg-tertiary     (neutral; could be plain text, unknown shape)
   *   EMPTY → --fg-quaternary   (no body or stripped to whitespace)
   *
   * No --signal-negative variant: the chip describes what came back,
   * not whether what came back was right. The outcome strip already
   * carries the verdict; this chip is descriptive, not evaluative.
   *
   * Session 32 adds `data-source` (`header` | `heuristic`). The
   * header-source chip is solid; the heuristic-source chip carries
   * a `?` glyph after the label and a slightly muted border to
   * mark its lower confidence. Tooltip on hover spells out which
   * source produced the reading and (when present) what the raw
   * Content-Type header value was.
   */
  .response-shape-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 1px 5px;
    border-radius: 2px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-canvas);
  }
  .response-shape-chip[data-shape="json"] {
    color: var(--signal-info, var(--fg-secondary));
    border-color: var(--signal-info, var(--border-subtle));
  }
  .response-shape-chip[data-shape="html"] {
    color: var(--signal-warning, var(--fg-secondary));
    border-color: var(--signal-warning, var(--border-subtle));
  }
  .response-shape-chip[data-shape="xml"] {
    color: var(--signal-info, var(--fg-secondary));
    border-color: var(--signal-info, var(--border-subtle));
  }
  .response-shape-chip[data-shape="csv"] {
    color: var(--signal-info, var(--fg-secondary));
    border-color: var(--signal-info, var(--border-subtle));
  }
  .response-shape-chip[data-shape="text"] {
    color: var(--fg-tertiary);
  }
  .response-shape-chip[data-shape="empty"] {
    color: var(--fg-quaternary);
  }
  /*
   * Heuristic-source chips: muted border to mark them as lower
   * confidence than header-source chips. The shape-specific color
   * still applies; only the border softens. Tooltip via the
   * `title` attribute carries the diagnostic detail.
   */
  .response-shape-chip[data-source="heuristic"] {
    border-style: dashed;
    opacity: 0.85;
  }

  .response-length {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    /* sits flush with the chip, no transform — read as a numeric
       literal, not a uppercase label */
    text-transform: none;
    letter-spacing: 0;
  }

  .response-bytes-pre {
    margin: 6px 0 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
    color: var(--fg-primary);
    background: var(--bg-inset);
    padding: 8px;
    border-radius: 2px;
    white-space: pre-wrap;
    word-break: break-word;
    /* Bytes can be up to 64 KiB; cap the visible height so the
       expanded affordance doesn't push other recipes off-screen.
       Operators who need more open the re-author dialog (which
       has its own scroll region tuned for diagnosis). */
    max-height: 280px;
    overflow-y: auto;
  }

  .response-bytes-empty {
    margin: 6px 0 0 0;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
    font-style: italic;
  }
</style>
