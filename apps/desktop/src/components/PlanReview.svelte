<!--
  PlanReview — the heart of Session 6. P1 in the handoff.

  Renders a `ResearchPlanDto` as an interactive, scannable panel.
  Composition matches the handoff spec:

    - Header: topic, plan id, created_at.
    - Trust paragraph: `interpretation` rendered prominently. This is
      "the moment of trust before fetching" per ADR 0007.
    - Topic-tags strip: chips, one per tag.
    - Geographic-scope strip: chips. Each chip's primary label is
      `display`; the `code` is in the title attribute for hover. When
      `display` is empty, the chip falls back to `code`. This is where
      the GeoScope work pays off visually.
    - Six bucket panels (Observation, Event, Entity, Relation, Document,
      Assertion) on a CSS grid.
    - Source-nominations panel listing the document sources in the order
      the LLM produced them.

  ## What's deliberately NOT here

  - Accept / Reject / Re-classify buttons: the handoff lists them under
    the P1 spec but the storage layer has no soft-delete or supersede
    operation yet (handoff §5: "no way to delete or amend a plan").
    Adding non-functional buttons would be UI theater. They land when
    storage grows the underlying operation.
  - "Hover showing usage count" on topic chips: the topic-tags row
    shows tags as plain chips. Usage count requires a `topics_in_use`
    call, which isn't a current Tauri command. Cheap to add later.
-->
<script lang="ts">
  import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
  import type { GeoScopeDto } from '$lib/api/types/GeoScopeDto';
  import { formatCreatedAt } from '$stores/plans.svelte';
  import Chip from '$components/common/Chip.svelte';
  import Bucket from '$components/panels/Bucket.svelte';
  import ExpectationRow from '$components/panels/ExpectationRow.svelte';

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
</script>

<article class="plan">
  <header class="head">
    <div class="topic-line">
      <h2 class="topic">{plan.topic}</h2>
      <span class="created">{formatCreatedAt(plan.created_at)}</span>
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
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-bottom: 1px solid var(--border-subtle);
    padding-bottom: 10px;
  }
  .topic-line {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .topic {
    font-size: 18px;
    font-weight: 600;
    margin: 0;
    color: var(--fg-primary);
    letter-spacing: -0.01em;
  }
  .created {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .meta {
    display: flex;
    gap: 16px;
    font-family: var(--font-mono);
    font-size: 10px;
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .k  { color: var(--fg-quaternary); text-transform: uppercase; letter-spacing: 0.06em; }
  .v  { color: var(--fg-secondary); }

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
