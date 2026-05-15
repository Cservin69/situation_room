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
      Assertion) on a CSS grid. Each bucket renders its expectations
      AND any records produced by the plan's recipes that fall into
      that bucket (Session 22).

  ## Accept / Reject (Session 7 §P1)

  The accept and reject buttons appear only while the plan is pending.
  Once the user has decided either way the buttons are replaced with
  a status pill, because the decision isn't reversible from this UI:
  rejecting a wrong reject means classifying a fresh plan, not
  un-rejecting (handoff §"explicitly NOT": no edit-the-plan flow).

  ## Records-on-the-workstation (Session 22)

  Each bucket renders, in order:

    1. Expectations rows (existing) — the LLM-stated intent.
    2. Records section (new) — the records produced for this plan
       and matching this record type.

  The records section only appears when `plans.records !== null` (we
  have asked the backend at least once). For pending plans the call
  is invalid, so `plans.records` stays null and the records section
  is hidden entirely. For accepted-or-rejected plans with no fetch
  runs yet, the records bucket is loaded but empty, and we show "0
  records yet" inline so the operator knows the panel is up to date,
  not stale.

  Per-expectation slotting (rendering each record under the specific
  expectation it satisfies) is deferred — the provenance string
  carries recipe id but not binding tag, so we can't tell which
  expectation a record matches without changes to the recipe-apply
  pipeline. See Session 23 handoff for the architectural follow-up.
-->
<script lang="ts">
  import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
  import type { GeoScopeDto } from '$lib/api/types/GeoScopeDto';
  import {
    plans,
    formatCreatedAt,
    acceptSelected,
    rejectSelected,
    reclassifySelected,
    classifyTopic,
  } from '$stores/plans.svelte';
  import {
    classifierPromptVersion,
    isCurrentClassifierVersion,
    parseClassifierId,
  } from '$lib/api/client';
  import Chip from '$components/common/Chip.svelte';
  import StatusPill from '$components/common/StatusPill.svelte';
  import Bucket from '$components/panels/Bucket.svelte';
  import ExpectationRow from '$components/panels/ExpectationRow.svelte';
  import NominationStatusGlyph from '$components/panels/NominationStatusGlyph.svelte';
  import NominationAttempts from '$components/panels/NominationAttempts.svelte';
  import RecordCard from '$components/panels/RecordCard.svelte';
  import RecordsDashboard from '$components/RecordsDashboard.svelte';
  import CostByTierPanel from '$components/CostByTierPanel.svelte';
  import RunFetchButton from '$components/RunFetchButton.svelte';
  import FetchReport from '$components/FetchReport.svelte';
  import RecipesPanel from '$components/RecipesPanel.svelte';
  import RecipeOutcomesHeatmap from '$components/RecipeOutcomesHeatmap.svelte';
  import ExpectationCoverage from '$components/ExpectationCoverage.svelte';
  import HostBackoffStatus from '$components/HostBackoffStatus.svelte';
  import SourcesMemoryPanel from '$components/SourcesMemoryPanel.svelte';
  import RejectDialog from '$components/dialogs/RejectDialog.svelte';

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

  // Dialog state for reject + reclassify. Local to this component;
  // the runes store handles the network mutation, the component
  // handles only the open/closed transition.
  let rejectDialogOpen = $state(false);
  let reclassifyDialogOpen = $state(false);

  async function onAccept() { await acceptSelected(); }

  function onRejectClick() {
    // The dialog drives the mutation when its onSubmit fires. The
    // button itself just opens.
    rejectDialogOpen = true;
  }

  async function onRejectSubmit(reason: string) {
    const ok = await rejectSelected(reason.trim().length > 0 ? reason : null);
    if (ok) rejectDialogOpen = false;
    // On failure: dialog stays open, user sees `plans.error` via
    // the parent error banner and can edit + resubmit.
  }
  function onRejectCancel() { rejectDialogOpen = false; }

  function onReclassifyClick() {
    // Pre-fill the dialog with the stored rejection reason so the
    // user can edit before retrying. If the user clears the field
    // entirely the backend uses the stored reason as-is (per
    // `reclassify_plan`'s edited-vs-stored fallback rule).
    reclassifyDialogOpen = true;
  }

  async function onReclassifySubmit(editedReason: string) {
    // Pass null when user didn't edit (effectively unchanged from
    // initial); otherwise pass the edit. We can't distinguish
    // "user typed the same thing back" from "user didn't edit" but
    // it doesn't matter — the backend treats both as "use this
    // text as the reason."
    const ok = await reclassifySelected(
      editedReason.trim().length > 0 ? editedReason : null,
    );
    if (ok) reclassifyDialogOpen = false;
  }
  function onReclassifyCancel() { reclassifyDialogOpen = false; }

  // Session 77 — stale-prompt-version banner. The Tauri command
  // returns the version string the binary currently loaded; we
  // compare it against the `@version` suffix parsed off the plan's
  // `classified_by` field. Pre-Session-77 plans carry just the
  // provider id (no `@`), so they parse to `promptVersion: null`
  // and are treated as "stale" — the banner fires.
  //
  // We fetch the constant once per component instance and cache it;
  // a future session can lift this into the plans store if multiple
  // components start needing it (the call is free, but each
  // PlanReview re-fetching on every navigation is unnecessary
  // churn).
  let currentPromptVersion = $state<string | null>(null);
  $effect(() => {
    // Capture in a local so the async resolution doesn't race against
    // a later effect-run (Svelte may re-run the effect if its
    // dependencies change; we don't have any here, but defensively
    // assign only the first successful response).
    let cancelled = false;
    classifierPromptVersion()
      .then((dto) => {
        if (!cancelled) currentPromptVersion = dto.current;
      })
      .catch(() => {
        // Command failure (Tauri not up, bug, etc.) — leave the
        // version null and the banner stays hidden. Surfacing a
        // toast here would be more noise than signal.
      });
    return () => {
      cancelled = true;
    };
  });

  let stalePromptVersion = $derived.by(() => {
    if (currentPromptVersion === null) return false;
    if (!plan.classified_by || plan.classified_by.length === 0) return false;
    return !isCurrentClassifierVersion(plan.classified_by, currentPromptVersion);
  });

  let storedPromptVersion = $derived(parseClassifierId(plan.classified_by ?? '').promptVersion);

  /// Session 77 — banner action. The backend's `reclassify_plan`
  /// command requires the plan to be in Rejected status; the
  /// stale-prompt banner needs to work on Pending and Accepted
  /// plans too. So instead of routing through the
  /// reject-then-reclassify flow, we kick off a fresh `classify`
  /// on the same topic. The result is a new Pending plan that
  /// shares the topic but carries the current prompt's framing;
  /// the original plan stays untouched (the operator can decide
  /// what to do with it). Same flow as `classifyTopic` from the
  /// topic input.
  async function onReclassifyUnderCurrentPrompt() {
    if (!plan.topic) return;
    await classifyTopic(plan.topic);
  }

  // Records-loaded sentinel + per-bucket counts. Reading these in
  // derived values keeps the template short and makes the empty-
  // state logic readable.
  //
  // `plans.records` is `null` when we haven't fetched (pending plan,
  // or before the first selectPlan refresh resolves). Distinguishing
  // null from all-empty matters because "we haven't asked" should
  // hide the records section entirely; "we asked, got nothing" should
  // show "0 records yet" inline.
  let recordsLoaded = $derived(plans.records !== null);
  let obsRecords = $derived(plans.records?.observations ?? []);
  let eventRecords = $derived(plans.records?.events ?? []);
  let entityRecords = $derived(plans.records?.entities ?? []);
  let relationRecords = $derived(plans.records?.relations ?? []);
  let documentRecords = $derived(plans.records?.documents ?? []);
  let assertionRecords = $derived(plans.records?.assertions ?? []);

  /**
   * Total records across all six types. Drives whether the
   * dashboard mode is offered at all — when the plan has produced
   * nothing yet, there's nothing to dashboard, so the buckets view
   * (with its inline "0 records yet — run a fetch" hints) is the
   * useful surface and the toggle hides.
   */
  let totalRecords = $derived(
    obsRecords.length +
      eventRecords.length +
      entityRecords.length +
      relationRecords.length +
      documentRecords.length +
      assertionRecords.length,
  );

  /**
   * Records-view toggle (Session 58). Two modes:
   *   - `dashboard` — the situation-room view (`RecordsDashboard`):
   *     records grouped by metric, big numbers + sparklines, type-
   *     count strip across the top.
   *   - `buckets` — the original six-bucket panel view:
   *     expectations interleaved with one-line record cards, JSON
   *     on expand. Useful when the operator is verifying a recipe
   *     or auditing the wire shape.
   *
   * The default is `dashboard` because that's the answer to the
   * "what did we get?" question that the user opens the plan to
   * see. The toggle persists across this component instance; it is
   * intentionally NOT persisted across the entire app so a fresh
   * plan-load lands the user on the dashboard reliably.
   *
   * The toggle hides entirely when no records exist yet
   * (`totalRecords === 0`) because the dashboard's empty hint and
   * the buckets' "0 records yet" hint communicate the same thing,
   * and the buckets view also surfaces expectations which is the
   * only meaningful content in the cold-start state.
   */
  let recordsViewMode = $state<'dashboard' | 'buckets'>('dashboard');
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
          onclick={onRejectClick}
        >
          reject
        </button>
      {:else if plan.status === 'rejected'}
        <StatusPill status={plan.status} />
        <button
          type="button"
          class="btn btn-primary"
          disabled={plans.classifying}
          onclick={onReclassifyClick}
          title="Re-classify with the rejection note as feedback"
        >
          {plans.classifying ? 're-classifying…' : 're-classify'}
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

  <!-- Host-backoff status strip (Session 48, piece B). Slots at the
       top of the review pane so observed network-layer signals from
       the most recent fetch run (or any fetch run this session) are
       visible alongside the plan body, not buried under the bucket
       grid. The component renders an explicit empty state when no
       host signals have been observed yet, so the strip is dim
       (rather than hidden) on a fresh-boot review. The polling
       lifecycle (5s cadence while a plan is selected) lives in the
       store; this component only reads. -->
  <HostBackoffStatus />

  <!-- Trust paragraph -->
  <section class="trust">
    <span class="trust-label">interpretation</span>
    <p>{plan.interpretation}</p>
  </section>

  <!-- Stale classifier prompt banner (Session 77).

       Shown when the version embedded in `plan.classified_by`
       (`"{provider}@{version}"`, post-Session-77) doesn't match the
       version constant the binary currently ships, OR when the plan
       predates Session 77 and carries just the provider id with no
       `@version` suffix. The user's one click hands the plan to the
       re-classify dialog — same flow the rejected-plan re-classify
       button uses, so the dialog logic is reused rather than
       re-implemented.

       Why not auto-reclassify: re-classification spends an LLM
       completion. The operator owns that decision per
       `feedback_eval_cost_discipline`; the banner surfaces the
       option without forcing it. -->
  {#if stalePromptVersion}
    <section class="stale-prompt-banner" data-testid="stale-prompt-banner">
      <span class="stale-prompt-label">stale classifier prompt</span>
      <p class="stale-prompt-text">
        This plan was classified under prompt version
        <code>{storedPromptVersion ?? '(pre-Session-77)'}</code>.
        The binary now ships <code>{currentPromptVersion}</code>.
        Re-classify to apply the newer prompt's framing.
      </p>
      <button
        type="button"
        class="stale-prompt-action"
        onclick={onReclassifyUnderCurrentPrompt}
        disabled={plans.classifying}
      >Re-classify</button>
    </section>
  {/if}

  <!-- Lineage banner: surfaces when this plan was produced by
       re-classifying a rejected predecessor. The id link is
       informational only today (no chain-walking UI yet — Session 16+);
       the visible "reclassified from" string is enough to alert the
       reader that the prior framing influenced this plan's
       feedback-fed prompt. See ADR 0011. -->
  {#if plan.reclassified_from && plan.reclassified_from.length > 0}
    <section class="lineage">
      <span class="lineage-label">reclassified from</span>
      <code class="lineage-id">{plan.reclassified_from}</code>
    </section>
  {/if}

  <!-- Rejection-reason panel: shown only on rejected plans. The
       reason is what the next re-classification will feed back into
       the classifier prompt (see Session 15 §classifier-feedback);
       surfacing it here makes that flow visible. The "edit and
       re-classify" entry point is the re-classify button in the
       header — clicking it opens the same dialog with this text
       pre-filled. -->
  {#if plan.status === 'rejected' && plan.rejection_reason && plan.rejection_reason.trim().length > 0}
    <section class="rejection">
      <span class="rejection-label">rejection note</span>
      <p>{plan.rejection_reason}</p>
    </section>
  {/if}

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

  <!-- Records view selector (Session 58). Renders only when the
       plan has produced records; in the cold-start state the
       buckets view below is the only useful surface (it carries
       expectations and "0 records yet" hints), so the toggle
       hides until there's something to dashboard.

       The dashboard mode replaces the bucket section with the
       situation-room view (metric-grouped cards, sparklines, type
       counts). The buckets mode keeps the original
       expectations-interleaved-with-records surface for auditing
       the wire shape or verifying a specific recipe.
  -->
  {#if recordsLoaded && totalRecords > 0}
    <header class="records-toolbar" aria-label="records view selector">
      <span class="records-toolbar-label">records</span>
      <div class="records-toggle" role="tablist">
        <button
          type="button"
          role="tab"
          class="seg"
          class:active={recordsViewMode === 'dashboard'}
          aria-selected={recordsViewMode === 'dashboard'}
          onclick={() => (recordsViewMode = 'dashboard')}
        >
          dashboard
        </button>
        <button
          type="button"
          role="tab"
          class="seg"
          class:active={recordsViewMode === 'buckets'}
          aria-selected={recordsViewMode === 'buckets'}
          onclick={() => (recordsViewMode = 'buckets')}
        >
          buckets
        </button>
      </div>
    </header>
  {/if}

  <!-- Dashboard view — the situation-room presentation. Renders
       when the toggle is in `dashboard` mode AND records exist.
       Expectations are intentionally NOT surfaced here: the
       dashboard answers "what did we get?", not "what did we ask
       for?". For the latter, flip to `buckets`. -->
  {#if recordsLoaded && totalRecords > 0 && recordsViewMode === 'dashboard' && plans.records}
    <RecordsDashboard records={plans.records} />
    <!-- Session 75 — cost-by-tier ledger underneath the dashboard. Same
         polling component used on the home view; it auto-refreshes on
         its own interval so this drill-in surface and the home view stay
         coherent. -->
    <CostByTierPanel />
  {/if}

  <!-- Six bucket panels (legacy default, now toggle-gated).

       Each bucket renders expectations rows from the plan, then —
       when records have been loaded for this plan — a records
       section with one RecordCard per record. The records section is
       visually separated from expectations by a thin divider.

       The "0 records yet" hint shows when expectations exist but no
       records have been produced; it does NOT show when expectations
       are also empty (the bucket already shows "no expectations by
       design" in that case via Bucket's empty-state logic).

       Session 58: this section renders in two cases:
         1. There are no records yet (`totalRecords === 0`) — the
            buckets carry the expectations and the "0 records yet"
            hints, which the dashboard view doesn't surface.
         2. The operator has explicitly switched to buckets mode
            (`recordsViewMode === 'buckets'`).
  -->
  {#if totalRecords === 0 || recordsViewMode === 'buckets'}
  <section class="buckets">
    <Bucket
      title="observation"
      count={plan.expectations.observation_metrics.length}
      recordsCount={obsRecords.length}
    >
      {#each plan.expectations.observation_metrics as m (m.name)}
        <ExpectationRow label={m.name} rationale={m.rationale}>
          {#snippet aside()}
            {#if m.unit_hint}<span>{m.unit_hint}</span>{/if}
          {/snippet}
        </ExpectationRow>
      {/each}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({obsRecords.length})</span>
          {#if obsRecords.length === 0 && plan.expectations.observation_metrics.length > 0}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each obsRecords as r (r.id)}
              <RecordCard kind="observation" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>

    <Bucket
      title="event"
      count={plan.expectations.event_types.length}
      recordsCount={eventRecords.length}
    >
      {#each plan.expectations.event_types as e (e.event_type)}
        <ExpectationRow label={e.event_type} rationale={e.rationale} />
      {/each}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({eventRecords.length})</span>
          {#if eventRecords.length === 0 && plan.expectations.event_types.length > 0}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each eventRecords as r (r.id)}
              <RecordCard kind="event" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>

    <Bucket
      title="entity"
      count={plan.expectations.entity_kinds.length}
      recordsCount={entityRecords.length}
    >
      {#each plan.expectations.entity_kinds as e (e.kind)}
        <ExpectationRow label={e.kind} rationale={e.rationale}>
          {#snippet aside()}
            {#if e.exemplars.length > 0}<span>{e.exemplars.length}×</span>{/if}
          {/snippet}
        </ExpectationRow>
      {/each}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({entityRecords.length})</span>
          {#if entityRecords.length === 0 && plan.expectations.entity_kinds.length > 0}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each entityRecords as r (r.id)}
              <RecordCard kind="entity" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>

    <Bucket
      title="relation"
      count={plan.expectations.relation_kinds.length}
      recordsCount={relationRecords.length}
    >
      {#each plan.expectations.relation_kinds as r (r.kind)}
        <ExpectationRow label={r.kind} rationale={r.rationale} />
      {/each}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({relationRecords.length})</span>
          {#if relationRecords.length === 0 && plan.expectations.relation_kinds.length > 0}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each relationRecords as r (r.id)}
              <RecordCard kind="relation" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>

    <Bucket
      title="document"
      count={plan.expectations.document_sources.length}
      recordsCount={documentRecords.length}
    >
      {#each plan.expectations.document_sources as s, i (i)}
        {#if s.kind === 'nomination'}
          <!--
            Session 39: post-Session-39 plans carry description-only
            nominations (no URL, no known_id). The propose-URL step
            picks the URL each fetch attempt fetches; URLs surface on
            the recipes / fetch-run panels, not here.

            Rendered as: description as the primary line, tier as an
            info chip, nomination_id as a short prefix for
            traceability (matching the recipe-id-prefix convention).

            Session 52 piece A: a NominationStatusGlyph renders to
            the left of the tier chip in the row aside, surfacing the
            most recent fetch outcome for this nomination — so the
            operator's vertical scan of the Document bucket reads
            "L1 description → outcome glyph → tier" in one line,
            without scrolling to a separate panel.

            Session 52 piece B: when the row is expanded, the
            ExpectationRow's `extras` snippet hosts a
            NominationAttempts chronology — one line per fetch run
            that touched this nomination, newest first — making the
            v1.1 propose-URL override's behaviour auditable inline
            against the L1 expectation it serves.
          -->
          <ExpectationRow
            label={s.description}
            rationale={'nomination ' + s.nomination_id.slice(0, 8)}
          >
            {#snippet aside()}
              <NominationStatusGlyph nominationId={s.nomination_id} />
              <Chip label={s.priority_tier.replace(/_/g, ' ')} tone="info" />
            {/snippet}
            {#snippet extras()}
              <NominationAttempts nominationId={s.nomination_id} />
            {/snippet}
          </ExpectationRow>
        {:else if s.kind === 'legacy'}
          <!--
            Pre-Session-39 plan (or pre-Session-37 plan with the older
            hint shape) persisted with DocumentSourceHintDto on the
            wire as Legacy. Rendered with a clear re-classify-to-update
            affordance — the executor will surface
            RecipeOutcomeDto.LegacyPlanCannotAuthor for each
            preferred_source_id when the operator hits Run Fetch on
            this plan.
          -->
          <ExpectationRow
            label={s.description}
            rationale={'legacy entry — re-classify the plan to update'}
          >
            {#snippet aside()}
              <Chip label="legacy" tone="warning" />
              {#each s.preferred_source_ids as id (id)}
                <Chip label={id} tone="info" />
              {/each}
            {/snippet}
          </ExpectationRow>
        {/if}
      {/each}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({documentRecords.length})</span>
          {#if documentRecords.length === 0 && plan.expectations.document_sources.length > 0}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each documentRecords as r (r.id)}
              <RecordCard kind="document" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>

    <Bucket
      title="assertion"
      count={plan.expectations.assertion_guidance ? 1 : 0}
      recordsCount={assertionRecords.length}
    >
      {#if plan.expectations.assertion_guidance}
        <p class="guidance">{plan.expectations.assertion_guidance}</p>
      {/if}
      {#if recordsLoaded}
        <div class="records">
          <span class="records-label">records ({assertionRecords.length})</span>
          {#if assertionRecords.length === 0 && plan.expectations.assertion_guidance}
            <p class="records-empty">0 records yet — run a fetch to populate</p>
          {:else}
            {#each assertionRecords as r (r.id)}
              <RecordCard kind="assertion" record={r} />
            {/each}
          {/if}
        </div>
      {/if}
    </Bucket>
  </section>
  {/if}

  <!-- Fetch report (Session 8). Renders only when the user has run a
       fetch, or when the history strip has prior runs to show. The
       component renders nothing when both are empty. -->
  {#if plans.fetchReport || plans.fetchRuns.length > 0}
    <FetchReport />
  {/if}

  <!-- Recipe-success heatmap (Session 46). Slots between the live
       fetch report (above) and the recipes inspection panel (below)
       so the operator's vertical scan reads:
         "what just happened" → "history of what happened" → "the
         recipes themselves."

       Renders an empty hint when no fetch_run_outcomes rows exist
       yet (pre-Session-46 plans, or freshly-accepted plans before
       their first fetch); see RecipeOutcomesHeatmap.svelte for the
       empty-state taxonomy. -->
  <RecipeOutcomesHeatmap />

  <!-- Expectation-coverage matrix (Session 46). Slots above the
       recipes panel. Surfaces the recipe-author prompt's "narrow
       honest coverage" discipline: for each plan expectation, list
       the recipes that bind to it, or mark it explicitly uncovered.
       The component renders nothing when the coverage matrix
       hasn't loaded (pending plan; pre-acceptance render). -->
  <ExpectationCoverage />

  <!-- Recipes panel (Session 11 P2.5). Renders the Level-2 authored
       recipes for the selected plan so the user can read what URL
       and extraction spec the LLM produced — invaluable when
       diagnosing why a fetch run came back with 0 records. The
       component itself renders nothing when there are no recipes,
       which is the legitimate state for a freshly-classified plan
       that hasn't been fetched yet. -->
  <RecipesPanel />

  <!-- Classifier sources-memory panel (Session 48, piece C). Mirrors
       what the classifier sees under `{{SOURCES_MEMORY}}` —
       recency-sorted top-30 (URL, source_id) pairs that have at
       least one successful fetch attempt across all plans. Surfacing
       it alongside the recipes panel closes the long-standing
       grounding-visibility gap noted across the 46/47/48 handoffs:
       the classifier's grounding is now operator-visible, not just
       log-visible. The component renders an explicit cold-start
       empty state when no successes have landed yet. -->
  <SourcesMemoryPanel />
</article>

{#if rejectDialogOpen}
  <RejectDialog
    topic={plan.topic}
    initial=""
    submitting={plans.mutating}
    onSubmit={onRejectSubmit}
    onCancel={onRejectCancel}
  />
{/if}

{#if reclassifyDialogOpen}
  <RejectDialog
    topic={plan.topic}
    initial={plan.rejection_reason ?? ''}
    submitting={plans.classifying}
    onSubmit={onReclassifySubmit}
    onCancel={onReclassifyCancel}
  />
{/if}

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
    /*
      Session 53 Piece E.2: sticky plan header. The bucket grid
      stacks tall (FetchReport + RecipeOutcomesHeatmap +
      ExpectationCoverage + RecipesPanel + SourcesMemoryPanel);
      pre-Session-53 the topic + accept/reject + run-fetch
      controls scrolled out of view, leaving the operator without
      orientation while reading the bottom of the page. Pinning
      the header keeps the topic + actions visible across the
      whole scroll surface.

      `position: sticky; top: 0` works against the nearest
      scrolling ancestor (`.review`'s `overflow-y: auto`).
      `background: var(--bg-panel)` gives the strip a small
      visual separation from the canvas content scrolling under
      it; the existing bottom border completes the strip.
      `z-index: 1` lifts the strip above the bucket grid's
      hover/focus rings without competing with modal dialogs.

      Padding-top picks up the 16px the parent .review used to
      provide before sticky pinning; the strip needs its own
      top buffer so content doesn't crash into the viewport edge.
    */
    position: sticky;
    top: 0;
    z-index: 1;
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 12px;
    row-gap: 6px;
    align-items: start;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border-subtle);
    padding: 10px 0;
    /* Pull the strip flush with the .review padding so the
       sticky bar spans the full panel width while content
       beneath retains its 16px gutter. */
    margin: -16px -16px 0;
    padding-left: 16px;
    padding-right: 16px;
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

  /* Lineage banner — small, inline, low-key. The visual weight
     should signal "this is metadata about provenance," not "this
     is the most important thing on the page." */
  .lineage {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 4px 12px;
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .lineage-label {
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .lineage-id {
    font-family: var(--font-mono);
    color: var(--fg-secondary);
    font-size: 10px;
  }

  /* Stale-prompt banner (Session 77) — warning-tone strip slotted
     between interpretation and lineage. Same border-left signal-color
     idiom as the rejection panel, with a right-aligned action button.
     Compact (single row in the common case, wraps on narrow widths). */
  .stale-prompt-banner {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-left: 2px solid var(--signal-warning);
    border-radius: 2px;
    padding: 8px 12px;
    font-size: 12px;
  }
  .stale-prompt-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--signal-warning);
  }
  .stale-prompt-text {
    margin: 0;
    color: var(--fg-secondary);
    line-height: 1.55;
    flex: 1 1 240px;
  }
  .stale-prompt-text code {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-primary);
    background: var(--bg-panel);
    padding: 1px 4px;
    border-radius: 2px;
  }
  .stale-prompt-action {
    flex: 0 0 auto;
    padding: 4px 10px;
    border: 1px solid var(--border-strong);
    border-radius: 2px;
    background: var(--bg-panel);
    color: var(--fg-primary);
    font: inherit;
    cursor: pointer;
  }
  .stale-prompt-action:hover {
    background: var(--bg-hover);
  }

  /* Rejection note panel — dimmer than the trust paragraph because
     it's history, not the active interpretation. The warning-tone
     left border ties it visually to the warning palette without
     shouting. */
  .rejection {
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-left: 2px solid var(--signal-warning);
    border-radius: 2px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .rejection-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--signal-warning);
  }
  .rejection p {
    margin: 0;
    color: var(--fg-secondary);
    font-size: 12px;
    line-height: 1.55;
    white-space: pre-wrap;
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

  /* Records view selector (Session 58). A small segmented control
     above the records area that toggles between the situation-room
     dashboard and the original six-bucket view. Visual weight is
     deliberately low — the toggle is a power-user affordance, not
     a primary action, and it should not compete with the topic
     header above or the dashboard content below. */
  .records-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0;
  }
  .records-toolbar-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .records-toggle {
    display: inline-flex;
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    overflow: hidden;
    background: var(--bg-panel);
  }
  .records-toggle .seg {
    appearance: none;
    background: transparent;
    color: var(--fg-tertiary);
    border: 0;
    padding: 4px 10px;
    font-family: var(--font-sans);
    font-size: 11px;
    cursor: pointer;
    transition: background var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .records-toggle .seg:hover {
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
  }
  .records-toggle .seg.active {
    background: var(--bg-panel-alt);
    color: var(--fg-primary);
  }
  .records-toggle .seg + .seg {
    border-left: 1px solid var(--border-subtle);
  }
  .records-toggle .seg:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: -1px;
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

  /* Records section — sits below expectations rows in each Bucket.
     Visually separated from expectations by a thin top divider so
     the operator can tell at a glance which rows are "intent" and
     which are "produced data." */
  .records {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-top: 8px;
    margin-top: 4px;
    border-top: 1px dashed var(--border-subtle);
  }
  .records-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-quaternary);
    padding-bottom: 2px;
  }
  .records-empty {
    margin: 0;
    color: var(--fg-quaternary);
    font-style: italic;
    font-size: 11px;
    padding: 2px 6px;
  }
</style>
