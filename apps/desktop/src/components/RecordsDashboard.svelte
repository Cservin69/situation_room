<!--
  RecordsDashboard — the situation-room view of a records bucket
  (Session 58, expanded Session 63).

  ## What this replaces

  The pre-Session-58 records view was six bucket panels (Observation,
  Event, Entity, Relation, Document, Assertion), each a vertical list
  of one-line RecordCard summaries with an expand-to-JSON affordance.
  That view is honest but flat: every record looks like every other
  record, the operator has to expand each card to see the number, and
  there's no cross-record context (no trend, no comparison, no
  freshness sense).

  The dashboard view re-organises the same data so it reads as a
  briefing rather than a list. The buckets view remains available as
  a debug/power-user toggle in `PlanReview.svelte`; this component is
  the default surface when records exist.

  ## Session 63 — cross-plan + all six typed panels

  The component is now data-source-agnostic: it accepts any
  `RecordsByPlanDto`, whether produced by `records_for_plan` (per-plan)
  or `records_recent_global` (cross-plan, the new home view in
  `+page.svelte`). All five non-Observation types now ship typed
  panels rather than collapsed "pending pill" placeholders.

  ## Layout

  - **Type-count strip** (top) — six tiles, one per record type, each
    showing the count. Tiles with records highlight; zeros dim. Lets
    the operator answer "what has the bucket got?" without scrolling.
  - **Observations panel** — grouped by `content.metric`, each metric
    rendered as a `MetricCard`. Carries the quantitative payload.
  - **Events panel** — grouped by `content.event_type`. KindCard
    surfaces the latest headline as the sample.
  - **Entities panel** — grouped by top-level `kind`. KindCard
    surfaces `canonical_name`.
  - **Relations panel** — grouped by `content.kind`. KindCard
    surfaces `"{from} → {to}"`.
  - **Documents panel** — grouped by top-level `kind` (the `doc_kind`
    field on the wire). KindCard surfaces `title` or a body preview.
  - **Assertions panel** — grouped by `stance` (top-level on
    AssertionDto). KindCard surfaces `claimant`.

  Each typed panel renders only when its bucket has at least one
  record so the dashboard stays dense; absence is communicated by
  the dimmed tile in the strip above, not by an empty section.

  ## Why per-type grouping keys

  An "observation" of `production` and an "observation" of `reserves`
  share a wire shape but answer different questions; the operator
  doesn't think "I have 2 observations", they think "I have a
  production number and a reserves number." Each non-Observation
  panel applies the same principle to its type's natural grouping
  key — drawn from the closed-vocabulary fields in
  `crates/core/src/schema/content.rs`. Grouping doesn't require
  fuzzy matching because the keys are enumerated.

  ## What this component does NOT do

  - **No re-fetching.** The component is presentational. The parent
    (`+page.svelte` for the global view, `PlanReview.svelte` for the
    per-plan view) owns the Tauri invocation; this component
    receives the resolved DTO and renders it.
  - **No mutations.** Records arrive read-only at the dashboard.
    Drill-into-recipe / re-author affordances live on the
    `RecipesPanel`, not here.
-->
<script lang="ts">
  import type { RecordsByPlanDto } from '$lib/api/types/RecordsByPlanDto';
  import type { ObservationDto } from '$lib/api/types/ObservationDto';
  import type { EventDto } from '$lib/api/types/EventDto';
  import type { EntityDto } from '$lib/api/types/EntityDto';
  import type { RelationDto } from '$lib/api/types/RelationDto';
  import type { DocumentDto } from '$lib/api/types/DocumentDto';
  import type { AssertionDto } from '$lib/api/types/AssertionDto';
  import MetricCard from '$components/panels/MetricCard.svelte';
  import KindCard from '$components/panels/KindCard.svelte';

  interface Props {
    /**
     * The records bucket, as returned by either `records_for_plan`
     * (per-plan) or `records_recent_global` (cross-plan, Session 63).
     * All six per-type Vecs are required (the wire DTO guarantees
     * they exist, possibly empty). `null` should never reach this
     * component — the parent handles the "records not loaded yet"
     * state by not rendering us in that case.
     */
    records: RecordsByPlanDto;
  }
  let { records }: Props = $props();

  // -- safe shape reads --------------------------------------------

  function safeGet(obj: unknown, key: string): unknown {
    if (obj && typeof obj === 'object' && key in obj) {
      return (obj as Record<string, unknown>)[key];
    }
    return undefined;
  }
  function safeString(obj: unknown, key: string): string {
    const v = safeGet(obj, key);
    return typeof v === 'string' ? v : '';
  }

  function metricOf(o: ObservationDto): string {
    const m = safeGet(o.content, 'metric');
    return typeof m === 'string' && m.length > 0 ? m : '(unknown)';
  }

  // -- envelope-level helpers (shared across typed panels) ---------

  /**
   * Best-effort bare-host extraction. Mirrors MetricCard's `hostOf`
   * so the dashboard's per-type cards render hosts identically.
   * Strips leading `www.` so e.g. `www.noaa.gov` and `noaa.gov`
   * read as the same source.
   */
  function hostOf(rawUrl: string | null | undefined): string {
    if (!rawUrl) return '';
    try {
      const u = new URL(rawUrl);
      const h = u.host;
      return h.startsWith('www.') ? h.slice(4) : h;
    } catch {
      return '';
    }
  }

  /**
   * Short ISO date for a record's `valid_at` (preferred) or
   * `observed_at` (fallback). Used by KindCard's footer; year-only
   * isn't reachable here without knowing whether the record's
   * `content.period === 'annual'`, which is observation-specific, so
   * the non-Observation panels just show the date.
   */
  function whenOf(env: { valid_at: string | null; observed_at: string }): string {
    const raw = env.valid_at ?? env.observed_at;
    if (!raw) return '';
    const d = new Date(raw);
    if (Number.isNaN(d.valueOf())) return '';
    return d.toISOString().slice(0, 10);
  }

  // Generic group-by helper. Records are bucketed by `keyOf(r)`,
  // preserving first-seen order so the panel reads left-to-right in
  // the order records arrived. Per-group records stay in input order
  // (DB query already returns observed_at DESC, so the first record
  // in each bucket is the most recent — that's what the per-type
  // sample readers index into below).
  function groupBy<T>(
    items: T[],
    keyOf: (item: T) => string,
  ): { key: string; records: T[] }[] {
    const map = new Map<string, T[]>();
    for (const item of items) {
      const k = keyOf(item);
      const bucket = map.get(k);
      if (bucket) bucket.push(item);
      else map.set(k, [item]);
    }
    return Array.from(map.entries()).map(([key, records]) => ({ key, records }));
  }

  // -- grouping ----------------------------------------------------

  /**
   * Group observations by `content.metric`. Preserves the order
   * metrics first appear in the source array so the dashboard's
   * left-to-right reading matches the database's stable order
   * (recipe-author authored production before reserves → that's the
   * order the cards land in). A more sophisticated ordering (e.g.
   * "most-recent first", "by record count") is a later-session
   * refinement.
   */
  let metricGroups = $derived.by(() => {
    const groups = new Map<string, ObservationDto[]>();
    for (const o of records.observations) {
      const m = metricOf(o);
      const bucket = groups.get(m);
      if (bucket) bucket.push(o);
      else groups.set(m, [o]);
    }
    return Array.from(groups.entries()).map(([metric, recs]) => ({
      metric,
      records: recs,
    }));
  });

  // -- typed-panel grouping (Session 63) ---------------------------
  //
  // Each non-Observation type has its own grouping key drawn from the
  // schema in `crates/core/src/schema/content.rs`:
  //
  //   - EventContent.event_type   → controlled vocab (see event_types.toml)
  //   - EntityDto.kind            → top-level entity kind string
  //   - RelationContent.kind      → snake_case relation kind
  //   - DocumentDto.kind          → top-level doc_kind string
  //   - AssertionDto.stance       → asserted / denied / hedged / …
  //
  // Sample-line per type chooses the most operator-readable single
  // field: Event → headline, Entity → canonical_name, Relation →
  // "{from} → {to}", Document → title, Assertion → claimant. When the
  // sample is unavailable the card renders a "— no preview available"
  // hint rather than blanking the line.

  function eventKindOf(e: EventDto): string {
    return safeString(e.content, 'event_type') || '(unknown)';
  }
  function eventSampleOf(e: EventDto): string {
    return safeString(e.content, 'headline');
  }

  function entityKindOf(e: EntityDto): string {
    return e.kind.length > 0 ? e.kind : '(unknown)';
  }
  function entitySampleOf(e: EntityDto): string {
    return e.canonical_name;
  }

  function relationKindOf(r: RelationDto): string {
    return safeString(r.content, 'kind') || '(unknown)';
  }
  function relationSampleOf(r: RelationDto): string {
    const from = safeString(r.content, 'from');
    const to = safeString(r.content, 'to');
    if (from && to) return `${from} → ${to}`;
    if (from) return from;
    if (to) return to;
    return '';
  }

  function documentKindOf(d: DocumentDto): string {
    return d.kind.length > 0 ? d.kind : '(unknown)';
  }
  function documentSampleOf(d: DocumentDto): string {
    if (d.title && d.title.trim().length > 0) return d.title;
    // No title — show the first ~120 chars of the body as a preview.
    // The body can be large; clamp so the panel doesn't blow up.
    if (d.body && d.body.length > 0) {
      const trimmed = d.body.trim();
      return trimmed.length > 120 ? trimmed.slice(0, 117) + '…' : trimmed;
    }
    return '';
  }

  /**
   * Session 69 — Path B time-series chart preview.
   *
   * Detect "this Document body is a time series" by structural shape,
   * not by host/source. Rule:
   *
   *  1. Body parses as JSON.
   *  2. Somewhere in the tree there's an array named (case-insensitively)
   *     `timestamp` or `timestamps`, length >= 2, all elements numeric.
   *  3. Somewhere else in the tree there's another numeric array of
   *     the same length. Among multiple candidates we prefer keys that
   *     name a primary value series (close, price, value, rate, yield)
   *     over secondaries (open, high, low, volume). The preference list
   *     is generic to time-series shapes, not source-specific.
   *
   * A friendly `label` falls out of common identity keys
   * (`symbol`, `longName`, `name`) when present, with a generic
   * fallback. This is presentation, not routing — see
   * `project_sr_no_source_routing` memory.
   *
   * Returns `null` when no time-series shape is detectable. The
   * caller (`documentSeriesOf`) then leaves the KindCard rendering
   * the text sample as before.
   */
  interface TimeSeries {
    timestamps: number[];
    values: number[];
    label: string;
    valueKey: string;
  }

  // Keys that, when found, indicate a "primary" value series.
  // First match wins; if none match, fall back to any same-length
  // numeric array. Lowercased for case-insensitive comparison.
  const PRIMARY_SERIES_KEYS = [
    'close', 'price', 'value', 'rate', 'yield', 'level',
  ];
  // Keys we recognise but rank lower — useful as fallbacks but not
  // first-choice for a one-line chart preview.
  const SECONDARY_SERIES_KEYS = [
    'open', 'high', 'low', 'volume', 'count',
  ];
  // Keys for friendly chart labels (entity name / ticker / metric id).
  const LABEL_KEYS = ['symbol', 'longname', 'shortname', 'name', 'series_id', 'id'];

  function detectTimeSeriesShape(body: string): TimeSeries | null {
    const trimmed = body.trim();
    if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return null;
    let parsed: unknown;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      return null;
    }
    return findSeries(parsed);
  }

  function isAllNumeric(arr: unknown[]): boolean {
    if (arr.length < 2) return false;
    for (const v of arr) {
      if (typeof v !== 'number' || !Number.isFinite(v)) return false;
    }
    return true;
  }

  function findSeries(root: unknown): TimeSeries | null {
    // First pass — collect timestamp arrays and value-candidate
    // arrays in one walk. Each candidate carries the key name and the
    // array, so the preference filter can rank them after the walk.
    const timestampArrays: number[][] = [];
    const valueCandidates: { key: string; values: number[] }[] = [];
    const labelCandidates: { key: string; value: string }[] = [];

    function walk(node: unknown, key: string | null): void {
      if (Array.isArray(node)) {
        if (isAllNumeric(node)) {
          if (key !== null) {
            const lc = key.toLowerCase();
            if (lc === 'timestamp' || lc === 'timestamps') {
              timestampArrays.push(node as number[]);
            } else {
              valueCandidates.push({ key: lc, values: node as number[] });
            }
          } else {
            // Top-level numeric array — treat as a value candidate
            // with an empty key (lowest priority).
            valueCandidates.push({ key: '', values: node as number[] });
          }
        } else {
          for (const item of node) walk(item, null);
        }
        return;
      }
      if (node !== null && typeof node === 'object') {
        for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
          // Capture label candidates from string-valued keys.
          if (typeof v === 'string' && LABEL_KEYS.includes(k.toLowerCase())) {
            labelCandidates.push({ key: k.toLowerCase(), value: v });
          }
          walk(v, k);
        }
      }
    }
    walk(root, null);

    if (timestampArrays.length === 0 || valueCandidates.length === 0) {
      return null;
    }

    // Pair the first timestamp array with the best-ranked value
    // candidate of matching length. Most Yahoo-shaped feeds have one
    // timestamp array per result; for the rare multi-result payload,
    // the first one usually carries the primary series. A future
    // session can revisit if we hit a case where the first isn't.
    const timestamps = timestampArrays[0];

    const sameLength = valueCandidates.filter((c) => c.values.length === timestamps.length);
    if (sameLength.length === 0) return null;

    // Rank: primary keys (close, price, …) > secondary (open, high, …)
    // > everything else. Within a tier, first-seen wins.
    function rank(key: string): number {
      const i1 = PRIMARY_SERIES_KEYS.indexOf(key);
      if (i1 >= 0) return i1;
      const i2 = SECONDARY_SERIES_KEYS.indexOf(key);
      if (i2 >= 0) return PRIMARY_SERIES_KEYS.length + i2;
      return PRIMARY_SERIES_KEYS.length + SECONDARY_SERIES_KEYS.length + 1;
    }
    sameLength.sort((a, b) => rank(a.key) - rank(b.key));
    const chosen = sameLength[0];

    // Label preference: first LABEL_KEYS hit (symbol > longName > name).
    let label = '';
    for (const lk of LABEL_KEYS) {
      const hit = labelCandidates.find((l) => l.key === lk);
      if (hit) {
        label = hit.value;
        break;
      }
    }

    return {
      timestamps,
      values: chosen.values,
      label,
      valueKey: chosen.key,
    };
  }

  /**
   * Convert a Document into MiniSparkline-shaped points, or `null`
   * when the body has no time-series structure. The KindCard
   * conditionally renders the chart when this returns non-null.
   *
   * Cap at 500 points to keep the SVG polyline path string bounded —
   * MiniSparkline's reduce loop scales fine to many thousands, but
   * 500 is more than enough resolution for a tile-sized preview and
   * keeps per-record render cost predictable.
   */
  function documentSeriesOf(d: DocumentDto): { points: Array<{ x: number; y: number }>; label: string; valueKey: string } | null {
    if (!d.body || d.body.length === 0) return null;
    const ts = detectTimeSeriesShape(d.body);
    if (ts === null) return null;
    const n = ts.timestamps.length;
    const stride = n > 500 ? Math.ceil(n / 500) : 1;
    const points: Array<{ x: number; y: number }> = [];
    for (let i = 0; i < n; i += stride) {
      points.push({ x: ts.timestamps[i], y: ts.values[i] });
    }
    // Always include the last point even if stride'd.
    if (points.length > 0 && points[points.length - 1].x !== ts.timestamps[n - 1]) {
      points.push({ x: ts.timestamps[n - 1], y: ts.values[n - 1] });
    }
    return { points, label: ts.label, valueKey: ts.valueKey };
  }

  function assertionKindOf(a: AssertionDto): string {
    return a.stance.length > 0 ? a.stance : '(unknown)';
  }
  function assertionSampleOf(a: AssertionDto): string {
    return a.claimant;
  }

  let eventGroups = $derived(groupBy(records.events, eventKindOf));
  let entityGroups = $derived(groupBy(records.entities, entityKindOf));
  let relationGroups = $derived(groupBy(records.relations, relationKindOf));
  let documentGroups = $derived(groupBy(records.documents, documentKindOf));
  let assertionGroups = $derived(groupBy(records.assertions, assertionKindOf));

  /*
    Session 68 — per-panel KindCard rendering cap. Plans that produce
    hundreds of records (FEMA's 872-event hunt fixture is the
    motivating case) can group into many distinct kinds; rendering
    every KindCard at once turns the dashboard into a wall of small
    cards and slows initial paint. We cap at PANEL_INITIAL_KINDS per
    typed panel and let the operator click "show all" per-panel to
    expand. The cap applies to the number of distinct kind GROUPS,
    not to the number of records inside each group — the underlying
    dataset is unchanged, just the visual surface is bounded.

    Observation panel exempt: its MetricCard variant is the primary
    cargo; capping it would hide the headline numbers the dashboard
    exists to show. Realistic plans don't produce dozens of distinct
    metrics anyway (production, reserves, price, …).
  */
  const PANEL_INITIAL_KINDS = 12;
  let expandedPanels = $state<Record<string, boolean>>({});
  function panelGroups<T>(
    panel: string,
    groups: { key: string; records: T[] }[],
  ): { key: string; records: T[] }[] {
    if (expandedPanels[panel]) return groups;
    return groups.length > PANEL_INITIAL_KINDS
      ? groups.slice(0, PANEL_INITIAL_KINDS)
      : groups;
  }
  function togglePanel(panel: string) {
    expandedPanels[panel] = !expandedPanels[panel];
  }

  // -- type-count strip --------------------------------------------

  /**
   * Six tiles, in canonical record-type order. The order matches
   * ADR 0003's enumeration so the strip reads the same way the
   * schema documents the types: Observation, Event, Entity,
   * Relation, Document, Assertion.
   */
  let typeCounts = $derived([
    { kind: 'observation', label: 'observations', count: records.observations.length },
    { kind: 'event', label: 'events', count: records.events.length },
    { kind: 'entity', label: 'entities', count: records.entities.length },
    { kind: 'relation', label: 'relations', count: records.relations.length },
    { kind: 'document', label: 'documents', count: records.documents.length },
    { kind: 'assertion', label: 'assertions', count: records.assertions.length },
  ]);

  let totalRecords = $derived(typeCounts.reduce((acc, t) => acc + t.count, 0));

  // Session 63 — `pendingTypes` is gone. The pre-Session-63 surface
  // collapsed five record types into a single "pending typed panel"
  // pill row; the five typed panels below replace it entirely.
</script>

<section class="dashboard" aria-label="records dashboard">
  <!-- Type-count strip. Always renders so the operator has a
       persistent answer to "what did the plan get?" — including
       "nothing" in the cold-start case. -->
  <header class="strip" aria-label="record-type counts">
    <span class="strip-label">records</span>
    <div class="tiles">
      {#each typeCounts as t (t.kind)}
        <div class="tile" class:has-records={t.count > 0}>
          <span class="tile-count">{t.count}</span>
          <span class="tile-label">{t.label}</span>
        </div>
      {/each}
    </div>
    <span class="strip-total" title="total records across all types">
      Σ {totalRecords}
    </span>
  </header>

  <!-- Session 63: the pre-Session-63 aspirational-note + pill-row
       surface is gone. Every non-Observation type now has its own
       typed panel below. Dimmed tiles in the strip mean "this type
       has no records yet"; a populated tile points at the typed
       panel underneath it. -->

  <!-- Observations panel — the primary cargo of the dashboard.
       Renders only when there's at least one observation; the empty
       case is already communicated by the type-count strip above. -->
  {#if records.observations.length > 0}
    <section class="observations" aria-label="observations">
      <header class="panel-header">
        <span>observations · by metric</span>
        <span class="panel-coord">{metricGroups.length} metric{metricGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each metricGroups as g (g.metric)}
          <MetricCard metric={g.metric} records={g.records} />
        {/each}
      </div>
    </section>
  {/if}

  <!-- Session 63 typed panels for the five non-Observation types.
       Each panel groups by the type's natural kind field and renders
       one KindCard per group with a count + representative sample.
       Each panel renders only when its bucket has at least one record
       so the dashboard stays dense — types with nothing collected
       stay represented by the dimmed tile in the type-count strip
       above, not by an empty section. -->

  {#if records.events.length > 0}
    {@const visible = panelGroups('events', eventGroups)}
    {@const hidden = eventGroups.length - visible.length}
    <section class="typed-panel" aria-label="events">
      <header class="panel-header">
        <span>events · by event_type</span>
        <span class="panel-coord">{eventGroups.length} type{eventGroups.length === 1 ? '' : 's'}</span>
        {#if hidden > 0 || expandedPanels['events']}
          <button class="panel-expand" type="button" onclick={() => togglePanel('events')}>
            {expandedPanels['events'] ? 'show first ' + PANEL_INITIAL_KINDS : '+' + hidden + ' more'}
          </button>
        {/if}
      </header>
      <div class="cards">
        {#each visible as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={eventSampleOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if records.entities.length > 0}
    {@const visible = panelGroups('entities', entityGroups)}
    {@const hidden = entityGroups.length - visible.length}
    <section class="typed-panel" aria-label="entities">
      <header class="panel-header">
        <span>entities · by kind</span>
        <span class="panel-coord">{entityGroups.length} kind{entityGroups.length === 1 ? '' : 's'}</span>
        {#if hidden > 0 || expandedPanels['entities']}
          <button class="panel-expand" type="button" onclick={() => togglePanel('entities')}>
            {expandedPanels['entities'] ? 'show first ' + PANEL_INITIAL_KINDS : '+' + hidden + ' more'}
          </button>
        {/if}
      </header>
      <div class="cards">
        {#each visible as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={entitySampleOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if records.relations.length > 0}
    {@const visible = panelGroups('relations', relationGroups)}
    {@const hidden = relationGroups.length - visible.length}
    <section class="typed-panel" aria-label="relations">
      <header class="panel-header">
        <span>relations · by kind</span>
        <span class="panel-coord">{relationGroups.length} kind{relationGroups.length === 1 ? '' : 's'}</span>
        {#if hidden > 0 || expandedPanels['relations']}
          <button class="panel-expand" type="button" onclick={() => togglePanel('relations')}>
            {expandedPanels['relations'] ? 'show first ' + PANEL_INITIAL_KINDS : '+' + hidden + ' more'}
          </button>
        {/if}
      </header>
      <div class="cards">
        {#each visible as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={relationSampleOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if records.documents.length > 0}
    {@const visible = panelGroups('documents', documentGroups)}
    {@const hidden = documentGroups.length - visible.length}
    <section class="typed-panel" aria-label="documents">
      <header class="panel-header">
        <span>documents · by kind</span>
        <span class="panel-coord">{documentGroups.length} kind{documentGroups.length === 1 ? '' : 's'}</span>
        {#if hidden > 0 || expandedPanels['documents']}
          <button class="panel-expand" type="button" onclick={() => togglePanel('documents')}>
            {expandedPanels['documents'] ? 'show first ' + PANEL_INITIAL_KINDS : '+' + hidden + ' more'}
          </button>
        {/if}
      </header>
      <div class="cards">
        {#each visible as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={documentSampleOf(g.records[0])}
            chartSeries={documentSeriesOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if records.assertions.length > 0}
    {@const visible = panelGroups('assertions', assertionGroups)}
    {@const hidden = assertionGroups.length - visible.length}
    <section class="typed-panel" aria-label="assertions">
      <header class="panel-header">
        <span>assertions · by stance</span>
        <span class="panel-coord">{assertionGroups.length} stance{assertionGroups.length === 1 ? '' : 's'}</span>
        {#if hidden > 0 || expandedPanels['assertions']}
          <button class="panel-expand" type="button" onclick={() => togglePanel('assertions')}>
            {expandedPanels['assertions'] ? 'show first ' + PANEL_INITIAL_KINDS : '+' + hidden + ' more'}
          </button>
        {/if}
      </header>
      <div class="cards">
        {#each visible as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={assertionSampleOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  <!-- True-empty case: the plan produced nothing at all. The
       type-count strip already shows six zeros; this hint adds the
       interpretive sentence. Differentiates "we asked, got nothing"
       from "we haven't asked" (the parent hides this component in
       the latter case). -->
  {#if totalRecords === 0}
    <p class="empty">
      no records yet — run a fetch to populate, or the recipes for
      this plan haven't produced any record so far.
    </p>
  {/if}
</section>

<style>
  .dashboard {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  /* ---- Type-count strip ---- */

  .strip {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
  }
  .strip-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
    flex: 0 0 auto;
  }
  .tiles {
    display: flex;
    gap: 6px;
    flex: 1 1 auto;
    flex-wrap: wrap;
  }
  .tile {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 4px 8px;
    border-radius: 2px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
    opacity: 0.55;
    transition: opacity var(--duration-ui) var(--ease);
  }
  .tile.has-records {
    opacity: 1;
    border-color: var(--border-strong);
  }
  .tile-count {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 14px;
    color: var(--fg-primary);
  }
  .tile-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
  }
  .strip-total {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
  }

  /* ---- Observations panel ---- */

  .observations {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }

  /* ---- Typed panels for non-Observation types (Session 63) ----
     Same visual shape as `.observations` so the dashboard reads as a
     consistent stack of "type → kinds → cards" sections. The shared
     `.cards` grid above governs the card layout. */
  .typed-panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  .panel-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    font-weight: 500;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .panel-coord {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
    text-transform: none;
    letter-spacing: 0;
  }
  /* Session 68 — per-panel expand toggle for the KindCard cap.
     Tucks at the right of the panel header. Same monospace +
     muted treatment as `.panel-coord` so it reads as panel meta,
     not as a primary action. */
  .panel-expand {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    color: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 2px 6px;
    margin-left: 8px;
    cursor: pointer;
    text-transform: none;
    letter-spacing: 0;
  }
  .panel-expand:hover {
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 10px;
  }

  /* Session 63 — `.pending` / `.pill*` / `.aspirational-note` styles
     removed. The typed panels above ship as first-class sections; no
     pill row or unauthored-bucket explanation remains in the template. */

  /* ---- True-empty hint ---- */

  .empty {
    margin: 0;
    padding: 14px 12px;
    background: var(--bg-inset);
    border: 1px dashed var(--border-subtle);
    border-radius: 3px;
    font-size: 11px;
    color: var(--fg-tertiary);
    text-align: center;
  }
</style>
