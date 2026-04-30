<!--
  PlanReview — the heart of the review pane.

  Renders a `ResearchPlanDto` as an interactive, scannable panel.
  Composition:

    - Header: topic, created_at, status badge, accept/reject buttons
              (when pending).
    - Trust paragraph: `interpretation` rendered prominently. This is
      "the moment of trust before fetching" per ADR 0007.
    - Topic-tags strip: chips, one per tag.
    - Geographic-scope strip: chips. Each chip's primary label is
      `display`; the `code` is in the title attribute for hover. When
      `display` is empty, the chip falls back to `code`.
    - Six bucket panels (Observation, Event, Entity, Relation, Document,
      Assertion) on a CSS grid.

  ## Accept / Reject (Session 7 §P1)

  The accept and reject buttons appear only while the plan is pending.
  Once the user has decided either way the buttons are replaced with
  a status pill, because the decision isn't reversible from this UI:
  rejecting a wrong reject means classifying a fresh plan, not
  un-rejecting (handoff §"explicitly NOT": no edit-the-plan flow).
-->
<script lang="ts">
  import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
  import type { GeoScopeDto } from '$lib/api/types/GeoScopeDto';
  import { plans, formatCreatedAt, acceptSelected, rejectSelected } from '$stores/plans.svelte';
  import Chip from '$components/common/Chip.svelte';
  import StatusPill from '$components/common/StatusPill.svelte';
  import Bucket from '$components/panels/Bucket.svelte';
  import ExpectationRow from '$components/panels/ExpectationRow.svelte';
  import RunFetchButton from '$components/RunFetchButton.svelte';
  import FetchReport from '$components/FetchReport.svelte';
  import RecipesPanel from '$components/RecipesPanel.svelte';

  interface Props {
    plan: ResearchPlanDto;
  }
  let { plan }: Props = $props();

  /**
   * Geographic scope label fallback: prefer `display`, fall back to
   * `code` when display is empty. The empty-string convention matches
   * the wire form (see classifier docs in pipeline crate).
   */
  function scopeLabel(g: GeoScopeDto): string {
    return g.display.trim().length > 0 ? g.display : g.code;
  }

  // The handlers go through the runes store helpers, which do the
  // optimistic update + rollback dance. We just await the boolean
  // return and ignore failures — the store has already populated
  // `plans.error` for the toast / banner layer to surface.
  async function onAccept() { await acceptSelected(); }
  async function onReject() { await rejectSelected(); }
</script>

<article class="plan">
  <header class="head">
    <div class="topic-line">
      <h2 class="topic">{plan.topic}</h2>
      <span class="created">{formatCreatedAt(plan.created_at)}</span>
    </div>
    <div class="actions">
      {#if plan.status === 'pending'}
        <button
          type="button"
          class="btn btn-primary"
          disabled={plans.mutating}
          onclick={onAccept}
        >
          accept
        </button>
        <button
          type="button"
          class="btn btn-secondary"
          disabled={plans.mutating}
          onclick={onReject}
        >
          reject
        </button>
      {:else}
        <StatusPill status={plan.status} />
        <RunFetchButton />
      {/if}
    </div>
    <div class="meta">
      <span class="kv"><span class="k">id</span><span class="v">{plan.id}</span></span>
      <span class="kv"><span class="k">window</span><span class="v">{plan.historical_window_days}d</span></span>
    </div>
  </header>

  <!-- Trust paragraph -->
  <section class="trust">
    <span class="trust-label">interpretation</span>
    <p>{plan.interpretation}</p>
  </section>

  <!-- Topic tags + geographic scope on a single row -->
  <section class="strip">
    {#if plan.topic_tags.length > 0}
      <div class="strip-group">
        <span class="strip-label">topics</span>
        <div class="chips">
          {#each plan.topic_tags as t (t)}
            <Chip label={t} />
          {/each}
        </div>
      </div>
    {/if}
    {#if plan.geographic_scope.length > 0}
      <div class="strip-group">
        <span class="strip-label">scope</span>
        <div class="chips">
          {#each plan.geographic_scope as g (g.code)}
            <Chip label={scopeLabel(g)} aside={g.display && g.display !== g.code ? g.code : ''} title={g.code} />
          {/each}
        </div>
      </div>
    {/if}
  </section>

  <!-- Six bucket panels -->
  <section class="buckets">
    <Bucket title="observation" count={plan.expectations.observation_metrics.length}>
      {#each plan.expectations.observation_metrics as m (m.name)}
        <ExpectationRow label={m.name} rationale={m.rationale}>
          {#snippet aside()}
            {#if m.unit_hint}<span>{m.unit_hint}</span>{/if}
          {/snippet}
        </ExpectationRow>
      {/each}
    </Bucket>

    <Bucket title="event" count={plan.expectations.event_types.length}>
      {#each plan.expectations.event_types as e (e.event_type)}
        <ExpectationRow label={e.event_type} rationale={e.rationale} />
      {/each}
    </Bucket>

    <Bucket title="entity" count={plan.expectations.entity_kinds.length}>
      {#each plan.expectations.entity_kinds as e (e.kind)}
        <ExpectationRow label={e.kind} rationale={e.rationale}>
          {#snippet aside()}
            {#if e.exemplars.length > 0}<span>{e.exemplars.length}×</span>{/if}
          {/snippet}
        </ExpectationRow>
      {/each}
    </Bucket>

    <Bucket title="relation" count={plan.expectations.relation_kinds.length}>
      {#each plan.expectations.relation_kinds as r (r.kind)}
        <ExpectationRow label={r.kind} rationale={r.rationale} />
      {/each}
    </Bucket>

    <Bucket title="document" count={plan.expectations.document_sources.length}>
      {#each plan.expectations.document_sources as s, i (i)}
        <ExpectationRow label={s.description} rationale={'preferred ids: ' + (s.preferred_source_ids.length > 0 ? s.preferred_source_ids.join(', ') : '(none — match by description)')}>
          {#snippet aside()}
            {#each s.preferred_source_ids as id (id)}
              <Chip label={id} tone="info" />
            {/each}
          {/snippet}
        </ExpectationRow>
      {/each}
    </Bucket>

    <Bucket title="assertion" count={plan.expectations.assertion_guidance ? 1 : 0}>
      {#if plan.expectations.assertion_guidance}
        <p class="guidance">{plan.expectations.assertion_guidance}</p>
      {/if}
    </Bucket>
  </section>

  <!-- Fetch report (Session 8). Renders only when the user has run a
       fetch, or when the history strip has prior runs to show. The
       component renders nothing when both are empty. -->
  {#if plans.fetchReport || plans.fetchRuns.length > 0}
    <FetchReport />
  {/if}

  <!-- Recipes panel (Session 11 P2.5). Renders the Level-2 authored
       recipes for the selected plan so the user can read what URL
       and extraction spec the LLM produced — invaluable when
       diagnosing why a fetch run came back with 0 records. The
       component itself renders nothing when there are no recipes,
       which is the legitimate state for a freshly-classified plan
       that hasn't been fetched yet. -->
  <RecipesPanel />
</article>

<style>
  .plan {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    height: 100%;
    overflow-y: auto;
    background: var(--bg-canvas);
  }

  /* Header */
  .head {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 12px;
    row-gap: 6px;
    align-items: start;
    border-bottom: 1px solid var(--border-subtle);
    padding-bottom: 10px;
  }
  .topic-line {
    grid-column: 1;
    grid-row: 1;
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
  }
  .actions {
    grid-column: 2;
    grid-row: 1;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .meta {
    grid-column: 1 / -1;
    grid-row: 2;
    display: flex;
    gap: 16px;
    font-family: var(--font-mono);
    font-size: 10px;
  }
  .topic {
    font-size: 18px;
    font-weight: 600;
    margin: 0;
    color: var(--fg-primary);
    letter-spacing: -0.01em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .created {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
    flex: 0 0 auto;
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .k  { color: var(--fg-quaternary); text-transform: uppercase; letter-spacing: 0.06em; }
  .v  { color: var(--fg-secondary); }

  /* Buttons. Following ADR 0006 — primary uses warm-amber for the
     "approve and proceed" semantic, secondary stays chrome. */
  .btn {
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    padding: 4px 10px;
    font-family: var(--font-mono);
    font-size: 11px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    cursor: pointer;
    color: var(--fg-secondary);
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .btn:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
  .btn-primary {
    color: var(--signal-warning);
    border-color: var(--signal-warning);
  }
  .btn-primary:hover:not(:disabled) {
    background: rgba(224, 165, 46, 0.1);
  }
  .btn-secondary {
    color: var(--fg-secondary);
  }
  .btn-secondary:hover:not(:disabled) {
    background: var(--bg-panel-alt);
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  /* Trust paragraph */
  .trust {
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-left: 2px solid var(--border-strong);
    border-radius: 2px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .trust-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .trust p {
    margin: 0;
    color: var(--fg-primary);
    font-size: 13px;
    line-height: 1.55;
  }

  /* Strip (topics + scope) */
  .strip {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
  }
  .strip-group {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .strip-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .chips {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  /* Six-bucket grid: 3 cols on wide screens, 2 on medium, 1 on narrow. */
  .buckets {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    flex: 1 1 auto;
    min-height: 0;
  }
  @media (max-width: 1100px) {
    .buckets { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  }
  @media (max-width: 700px) {
    .buckets { grid-template-columns: 1fr; }
  }

  .guidance {
    margin: 0;
    font-size: 12px;
    color: var(--fg-secondary);
    line-height: 1.55;
  }
</style>
