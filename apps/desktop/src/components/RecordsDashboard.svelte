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
    <section class="typed-panel" aria-label="events">
      <header class="panel-header">
        <span>events · by event_type</span>
        <span class="panel-coord">{eventGroups.length} type{eventGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each eventGroups as g (g.key)}
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
    <section class="typed-panel" aria-label="entities">
      <header class="panel-header">
        <span>entities · by kind</span>
        <span class="panel-coord">{entityGroups.length} kind{entityGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each entityGroups as g (g.key)}
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
    <section class="typed-panel" aria-label="relations">
      <header class="panel-header">
        <span>relations · by kind</span>
        <span class="panel-coord">{relationGroups.length} kind{relationGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each relationGroups as g (g.key)}
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
    <section class="typed-panel" aria-label="documents">
      <header class="panel-header">
        <span>documents · by kind</span>
        <span class="panel-coord">{documentGroups.length} kind{documentGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each documentGroups as g (g.key)}
          <KindCard
            kind={g.key}
            count={g.records.length}
            sample={documentSampleOf(g.records[0])}
            when={whenOf(g.records[0].envelope)}
            sourceHost={hostOf(g.records[0].envelope.provenance.source_url)}
            sourceUrl={g.records[0].envelope.provenance.source_url ?? ''}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if records.assertions.length > 0}
    <section class="typed-panel" aria-label="assertions">
      <header class="panel-header">
        <span>assertions · by stance</span>
        <span class="panel-coord">{assertionGroups.length} stance{assertionGroups.length === 1 ? '' : 's'}</span>
      </header>
      <div class="cards">
        {#each assertionGroups as g (g.key)}
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
