<!--
  +page.svelte — the only screen, for now.

  Three-pane layout:
    - Top bar: TopicInput (P3).
    - Left: RecentPlansList (P2).
    - Right: PlanReview (P1) when a plan is selected; otherwise the
             cross-plan situation-room dashboard (Session 63) — the
             cumulative view of every record produced by every plan
             so the operator sees what's been collected without
             selecting any particular plan.

  Single SPA route. The handoff is explicit that Session 6 does not
  build out routing for separate listing / review URLs — the pane
  composition is the navigation model. If we later want
  bookmarkable plan URLs, that's an add-on, not a redesign.

  ## Session 63 — global dashboard as the home view

  Pre-Session-63 the right pane was either PlanReview (when a plan
  was selected) or a one-line "select a plan…" hint (when nothing
  was selected). The hint surface carried no signal; the operator's
  view of "what does the system have" required selecting a plan,
  which by definition scopes the view to that plan and hides all
  others' records.

  Session 63 puts the cross-plan RecordsDashboard in the home slot:
  whenever no plan is selected, the operator sees every plan's
  records merged into one dashboard, capped at the backend's default
  per-type limit. Switching to a specific plan still drops into the
  per-plan PlanReview for drill-down (the per-plan dashboard there
  is unchanged). The "select a plan…" hint is reduced to a thin
  footer line below the dashboard so the navigation cue is still
  present but doesn't dominate.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import {
    plans,
    refreshRecent,
    refreshGlobalRecords,
  } from '$stores/plans.svelte';
  import TopicInput from '$components/TopicInput.svelte';
  import RecentPlansList from '$components/RecentPlansList.svelte';
  import PlanReview from '$components/PlanReview.svelte';
  import RecordsDashboard from '$components/RecordsDashboard.svelte';

  onMount(() => {
    // Populate the listing on app open. No LLM call.
    refreshRecent();
    // Session 63: warm the cross-plan dashboard so the right pane
    // has data to show as soon as the empty (no plan selected)
    // state renders. Failure is non-fatal — the dashboard renders
    // its own empty-state copy.
    refreshGlobalRecords();
  });
</script>

<div class="app">
  <header class="topbar">
    <div class="brand">
      <span class="name">situation_room</span>
      <span class="phase">situation room</span>
    </div>
    <div class="input-wrap">
      <TopicInput />
    </div>
  </header>

  <main>
    <div class="left">
      <RecentPlansList />
    </div>
    <div class="right">
      {#if plans.selected}
        <PlanReview plan={plans.selected} />
      {:else}
        <!-- Session 63 — cross-plan dashboard as the home view.
             The dashboard renders its own empty-state copy when the
             global records bucket is null (pre-boot) or all-zero
             (boot landed but the store has nothing), so we mount it
             unconditionally. -->
        <section class="home">
          <header class="home-head">
            <h2 class="home-title">situation room</h2>
            <p class="home-sub">
              every record every plan has produced — cross-plan view.
              {#if plans.recent.length === 0}
                type a topic above to begin populating.
              {:else}
                select a plan from the list for per-plan drill-down.
              {/if}
            </p>
          </header>
          {#if plans.globalRecords}
            <RecordsDashboard records={plans.globalRecords} />
          {:else}
            <p class="home-loading">loading records…</p>
          {/if}
        </section>
      {/if}
    </div>
  </main>
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg-canvas);
  }

  .topbar {
    display: flex;
    align-items: center;
    gap: 24px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-panel);
  }
  .brand {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex: 0 0 auto;
  }
  .name {
    font-size: 15px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--fg-primary);
  }
  .phase {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--fg-tertiary);
  }
  .input-wrap {
    flex: 1 1 auto;
    max-width: 720px;
  }

  main {
    display: grid;
    grid-template-columns: 320px 1fr;
    gap: 8px;
    padding: 8px;
    flex: 1 1 auto;
    min-height: 0;
  }
  .left {
    min-height: 0;
    overflow: hidden;
  }
  .right {
    min-height: 0;
    overflow: hidden;
    background: var(--bg-canvas);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  /* Session 63: the pre-Session-63 `.empty` rule is gone — the
     "select a plan…" hint surface that used it was replaced by the
     `.home` cross-plan dashboard below. Removed rather than kept as
     dead CSS so svelte-check's unused-selector warning stays clean. */

  /* Session 63 — cross-plan dashboard home view. Scrolls
     independently of the rest of the app so the panel grid can grow
     without pushing the topbar / brand off the screen. Padding
     matches PlanReview's inner padding so the visual rhythm is
     consistent between the two right-pane modes. */
  .home {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    height: 100%;
    overflow-y: auto;
    background: var(--bg-canvas);
  }
  .home-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .home-title {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--fg-primary);
  }
  .home-sub {
    margin: 0;
    font-size: 11px;
    color: var(--fg-tertiary);
    line-height: 1.55;
  }
  .home-loading {
    margin: 0;
    padding: 14px 12px;
    background: var(--bg-inset);
    border: 1px dashed var(--border-subtle);
    border-radius: 3px;
    font-size: 11px;
    color: var(--fg-tertiary);
    text-align: center;
  }

  @media (max-width: 800px) {
    main { grid-template-columns: 1fr; }
    .left { max-height: 30vh; }
  }
</style>
