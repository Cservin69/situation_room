<!--
  RecordsDashboard — the situation-room view of a plan's records
  (Session 58).

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
  the default when records exist.

  ## Layout

  - **Type-count strip** (top) — six tiles, one per record type, each
    showing the count. Tiles with records highlight; zeros dim. Lets
    the operator answer "what did the plan get?" without scrolling.
  - **Observations panel** — grouped by `content.metric`, each metric
    rendered as a `MetricCard`. This is the panel that carries the
    plan's quantitative payload.
  - **Other-type pills** (collapsed) — Event, Entity, Relation,
    Document, Assertion render as small count pills with "details in
    buckets view" guidance when non-zero. Full typed panels for these
    five types ship in subsequent commits — Session 58 prioritises
    Observations because every authored plan so far produces them
    and the dashboard's value is most visible there. Empty types
    don't render at all so the dashboard stays dense.

  ## Why metric-grouping for observations

  An "observation" of `production` and an "observation" of `reserves`
  share a wire shape but answer different questions; the operator
  reading the dashboard doesn't think "I have 2 observations", they
  think "I have a production number and a reserves number." Grouping
  by `content.metric` matches that mental model directly. The
  side-effect: when multiple observations of the same metric arrive
  (different `valid_at`, e.g. historical series), they collapse into
  one card with a sparkline rather than two separate cards. The
  schema makes this trivial — `metric` is a closed snake_case
  vocabulary (`production`, `reserves`, `price`, …), so grouping
  doesn't require fuzzy matching.

  ## What this component does NOT do

  - **No re-fetching.** The component is presentational. The parent
    (`PlanReview`) is responsible for the `records_for_plan` Tauri
    invocation; this component receives the resolved DTO and
    renders it.
  - **No cross-plan comparison.** The dashboard scopes to one plan,
    same as the rest of the review surface. Cross-plan views (the
    real situation-room dream — pin records across plans into a
    persistent canvas) are a separate product surface.
  - **No mutations.** Records arrive read-only at the dashboard.
    Drill-into-recipe / re-author affordances live on the
    `RecipesPanel`, not here.
-->
<script lang="ts">
  import type { RecordsByPlanDto } from '$lib/api/types/RecordsByPlanDto';
  import type { ObservationDto } from '$lib/api/types/ObservationDto';
  import MetricCard from '$components/panels/MetricCard.svelte';

  interface Props {
    /**
     * The plan's records, as returned by `records_for_plan`. All
     * six per-type Vecs are required (the wire DTO guarantees they
     * exist, possibly empty). `null` should never reach this
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

  function metricOf(o: ObservationDto): string {
    const m = safeGet(o.content, 'metric');
    return typeof m === 'string' && m.length > 0 ? m : '(unknown)';
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

  /**
   * Non-Observation types that have at least one record. Each
   * becomes a "details in buckets view" pill below the
   * Observations panel. When all five are empty, the pills
   * section collapses entirely.
   */
  let pendingTypes = $derived(
    typeCounts.filter((t) => t.kind !== 'observation' && t.count > 0),
  );
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

  <!--
    Session 60 — aspirational-note pass (D). Session 61 landed both
    gates (ADR 0018 bucket-fair dispatch raised the per-nomination cap
    to 6 and switched to round-robin order; ADR 0019 Phase 2A added
    `extracted_inner` FieldMaps for css_select and json_path). The
    note now reflects the structural-readiness state: dimmed tiles
    mean "the gates are open, but a recipe hasn't authored against
    that bucket on this plan yet" rather than "tried and empty" or
    "structurally unreachable." Removed once the typed panels have
    at least one record each — at which point `pendingTypes.length`
    falls to 0 by definition and the pill row takes over the
    "not-yet-populated" surface.

    Rendered only when at least one record exists for the plan, so the
    pre-fetch and true-empty cases don't carry an explanation for an
    absence the operator hasn't yet measured.
  -->
  {#if totalRecords > 0 && pendingTypes.length === 0}
    <p class="aspirational-note" aria-label="pending typed panels">
      Events, Entities, Relations, Documents, and Assertions are
      structurally reachable from Session 61 onward (ADR 0018
      bucket-fair dispatch + ADR 0019 per-field extraction sub-specs).
      Dimmed tiles above mean "the gates are open but this plan
      hasn't authored a recipe against that bucket yet," not
      "tried and empty." The pill row below surfaces buckets where
      a record landed.
    </p>
  {/if}

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

  <!-- "Pending-typed-panel" section. Surfaces the existence of
       non-Observation records as a single line of pills, each with a
       count and a small hint that the dashboard's typed panel for
       this record type isn't built yet. Clicking falls through to
       the buckets view (the parent owns the view-toggle wiring; this
       component just emits a `requestBucketsView` event). -->
  {#if pendingTypes.length > 0}
    <aside class="pending" aria-label="other record types">
      <span class="pending-label">other record types in this plan:</span>
      <div class="pills">
        {#each pendingTypes as t (t.kind)}
          <span class="pill" title="full typed panel for {t.label} coming in a later session; use the buckets view for now">
            <span class="pill-label">{t.label}</span>
            <span class="pill-count">{t.count}</span>
          </span>
        {/each}
      </div>
    </aside>
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

  /* ---- Pending-typed-panel pills ---- */

  .pending {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    background: var(--bg-inset);
    border: 1px dashed var(--border-subtle);
    border-radius: 3px;
  }
  .pending-label {
    font-size: 11px;
    color: var(--fg-tertiary);
    flex: 0 0 auto;
  }
  .pills {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .pill {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    padding: 2px 6px;
    border: 1px solid var(--border-subtle);
    border-radius: 10px;
    background: var(--bg-panel);
    font-size: 10px;
  }
  .pill-label {
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .pill-count {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-primary);
  }

  /* ---- Aspirational note (Session 60, ADR 0018/0019) ---- */

  .aspirational-note {
    margin: 0;
    padding: 8px 12px;
    background: var(--bg-inset);
    border-left: 2px dashed var(--border-subtle);
    border-radius: 2px;
    font-size: 11px;
    line-height: 1.6;
    color: var(--fg-tertiary);
  }
  /* Session 61: the v1.0 note's `<code>` rule for inline ADR
     references was removed when the prose stopped quoting ADR file
     paths; the gates landed so the note no longer needs to point at
     unmerged ADRs. */

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
