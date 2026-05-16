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
  import DocumentDrawer from '$components/DocumentDrawer.svelte';
  import SamplesModal from '$components/SamplesModal.svelte';
  import type { ChartCatalog, ChartPreview } from '$lib/dashboard/document_chart';
  import { detectChartCatalog, pickPreviewSeries } from '$lib/dashboard/document_chart';

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
   * The shape detector and truncation-recovery routines moved to
   * `$lib/dashboard/document_chart` in Session 83 so the expanded
   * `DocumentDrawer` can consume the *catalog* of all numeric
   * series (close + volume + open + high + low + …) for its metric
   * dropdown. The KindCard tile still gets a single-series preview
   * via `pickPreviewSeries` — same shape (and rendering) as
   * pre-Session-83.
   *
   * See `apps/desktop/src/lib/dashboard/document_chart.ts` for the
   * closed-vocabulary rule set and the recovery algorithm.
   */
  function documentCatalogOf(d: DocumentDto): ChartCatalog | null {
    if (!d.body || d.body.length === 0) return null;
    return detectChartCatalog(d.body);
  }

  /**
   * Single-series adapter for the KindCard tile preview. Returns
   * the same `{ points, label, valueKey } | null` shape the
   * pre-Session-83 `documentSeriesOf` returned, so the KindCard
   * wiring at the bottom of this file is unchanged.
   */
  function documentSeriesOf(d: DocumentDto): ChartPreview | null {
    const catalog = documentCatalogOf(d);
    if (catalog === null) return null;
    return pickPreviewSeries(catalog);
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
    Session 81 — derive an entity_id → attribute-tiles map from the
    assertions Vec. Each `AssertedContent::EntityAttribute` row carries
    `{entity_id, key, value}`; we dedup by latest-observation-wins per
    (entity_id, key) and project the typed value back to a flat
    string for the chip. Pre-Session-80 plans (no entity-attribute
    extraction) produce an empty map; post-Session-80 plans light up
    the strip on their Entity KindCards.

    The fold is presentational — the underlying assertion rows stay
    available under the Assertions panel for drill-down. We deliberately
    don't filter on plan_id here: the dashboard is cross-plan today
    (Session 63), and an EntityAttribute asserted on `company:tsla` in
    plan A is still relevant when plan B's entities panel surfaces
    `company:tsla`.
  */
  function formatAttributeValue(raw: unknown): string {
    // AttributeValue is a tagged enum: `{kind: 'text', value: '...'}`,
    // `{kind: 'number', value: {value: N, unit: 'persons'}}`, etc. We
    // pretty-print each shape into a single string; non-conformant
    // payloads (forward-compat hedge) fall through to JSON.stringify
    // so the chip never crashes the panel.
    if (raw && typeof raw === 'object') {
      const kind = (raw as Record<string, unknown>)['kind'];
      const value = (raw as Record<string, unknown>)['value'];
      if (kind === 'text' && typeof value === 'string') return value;
      if (kind === 'boolean' && typeof value === 'boolean') return value ? 'true' : 'false';
      if (kind === 'number' && value && typeof value === 'object') {
        const n = (value as Record<string, unknown>)['value'];
        const u = (value as Record<string, unknown>)['unit'];
        const numStr = typeof n === 'number' ? n.toLocaleString() : String(n ?? '');
        return typeof u === 'string' && u.length > 0 ? `${numStr} ${u}` : numStr;
      }
      if (kind === 'country' && typeof value === 'string') return value;
      if (kind === 'topic' && typeof value === 'string') return value;
      if (kind === 'entity' && typeof value === 'string') return value;
      // Future variants (entity_list, topic_list) — fall through.
    }
    try {
      return JSON.stringify(raw);
    } catch {
      return '';
    }
  }

  let attributeTilesByEntityId = $derived.by(() => {
    const out = new Map<string, Map<string, string>>();
    for (const a of records.assertions) {
      const content = a.content as Record<string, unknown> | null;
      if (!content || typeof content !== 'object') continue;
      // Session 78 renamed the discriminator to `asserted_kind` to
      // resolve a duplicate-`kind` collision with RelationContent;
      // the entity-attribute shape lives under the same wire form.
      const ak = content['asserted_kind'];
      if (ak !== 'entity_attribute') continue;
      const eid = content['entity_id'];
      const key = content['key'];
      const value = content['value'];
      if (typeof eid !== 'string' || typeof key !== 'string') continue;
      const formatted = formatAttributeValue(value);
      if (formatted.length === 0) continue;
      const inner = out.get(eid) ?? new Map<string, string>();
      // Records arrive observed_at-DESC from the storage layer, so
      // first-seen-wins is latest-known-value. (Subsequent rows for
      // the same key are older.)
      if (!inner.has(key)) inner.set(key, formatted);
      out.set(eid, inner);
    }
    // Collapse inner maps into ordered arrays. The Map preserves
    // insertion order so the chip strip reads in observation order
    // (typically `legal_name` first → `ticker` → …).
    const flat = new Map<string, Array<{ key: string; value: string }>>();
    for (const [eid, attrs] of out) {
      flat.set(
        eid,
        Array.from(attrs.entries()).map(([key, value]) => ({ key, value })),
      );
    }
    return flat;
  });

  function attributeTilesForEntity(e: EntityDto): Array<{ key: string; value: string }> | null {
    const eid = e.entity_id ?? '';
    if (!eid) return null;
    const tiles = attributeTilesByEntityId.get(eid);
    return tiles && tiles.length > 0 ? tiles : null;
  }

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

  // Session 70 — DocumentDrawer state. When non-null, the drawer
  // renders over the dashboard inspecting the selected Document.
  // Clicking a Document KindCard sets the selection; the drawer's
  // close handler clears it.
  let selectedDocument = $state<DocumentDto | null>(null);
  // Session 83 — drawer receives the full ChartCatalog (every numeric
  // series the detector found, ranked primary-first) so its metric
  // dropdown can switch between close / volume / open / high / low /
  // adjclose without re-running the parse. The KindCard tile keeps
  // its single-series preview via `documentSeriesOf` below.
  let selectedDocumentCatalog = $state<ChartCatalog | null>(null);

  function openDocumentDrawer(doc: DocumentDto): void {
    selectedDocument = doc;
    selectedDocumentCatalog = documentCatalogOf(doc);
  }
  function closeDocumentDrawer(): void {
    selectedDocument = null;
    selectedDocumentCatalog = null;
  }

  // Session 80 — SamplesModal state. When non-null, the modal renders
  // over the dashboard listing every distinct sample for a KindCard
  // group. The KindCard fires `onSamplesExpand(all)` with the already-
  // deduped list when the operator clicks the `+ N more` overflow row;
  // we pin (kind, count, samples) so the modal's header stays in sync
  // even if the underlying records change while it's open.
  let selectedSampleGroup = $state<{
    kind: string;
    count: number;
    samples: string[];
  } | null>(null);

  function openSamplesModal(kind: string, count: number, all: string[]): void {
    selectedSampleGroup = { kind, count, samples: all };
  }
  function closeSamplesModal(): void {
    selectedSampleGroup = null;
  }
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
            samples={g.records.map(eventSampleOf)}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
            onSamplesExpand={(all) => openSamplesModal(g.key, g.records.length, all)}
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
            samples={g.records.map(entitySampleOf)}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
            onSamplesExpand={(all) => openSamplesModal(g.key, g.records.length, all)}
            attributeTiles={attributeTilesForEntity(g.records[0])}
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
            samples={g.records.map(relationSampleOf)}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
            onSamplesExpand={(all) => openSamplesModal(g.key, g.records.length, all)}
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
            onOpen={() => openDocumentDrawer(g.records[0])}
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
            samples={g.records.map(assertionSampleOf)}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
            onSamplesExpand={(all) => openSamplesModal(g.key, g.records.length, all)}
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

<!-- Session 70 — DocumentDrawer renders over the dashboard when a
     Document is selected. The {#if} gate keeps the modal off the DOM
     entirely when no document is open, so the dashboard's
     keyboard-event handling isn't disturbed in the common case. -->
{#if selectedDocument !== null}
  <DocumentDrawer
    document={selectedDocument}
    chartCatalog={selectedDocumentCatalog}
    onClose={closeDocumentDrawer}
  />
{/if}

<!-- Session 80 — SamplesModal renders over the dashboard when a
     KindCard group's overflow row was clicked. Same {#if}-gate
     posture as DocumentDrawer: off the DOM in the common case. -->
{#if selectedSampleGroup !== null}
  <SamplesModal
    kind={selectedSampleGroup.kind}
    count={selectedSampleGroup.count}
    samples={selectedSampleGroup.samples}
    onClose={closeSamplesModal}
  />
{/if}

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
