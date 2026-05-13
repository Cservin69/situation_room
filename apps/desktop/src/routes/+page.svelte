<!--
  +page.svelte — the only screen, for now.

  Three-pane layout:
    - Top bar: TopicInput (P3).
    - Left: RecentPlansList (P2).
    - Right: PlanReview (P1) when a plan is selected; otherwise a
             blank canvas that names the two next actions and stays
             out of the way (Session 66 revision of Session 63).

  Single SPA route. The handoff is explicit that Session 6 does not
  build out routing for separate listing / review URLs — the pane
  composition is the navigation model. If we later want
  bookmarkable plan URLs, that's an add-on, not a redesign.

  ## Session 66 — blank canvas replaces the cross-plan dashboard

  Session 63 had put the cross-plan RecordsDashboard in the home
  slot so the operator could see "what the system has" without
  selecting a plan. In practice that view was misleading: on every
  app boot the right pane filled with pooled metrics from old
  plans (Session 65's ebola + Session 64's lithium production,
  Session 63's hurricanes, etc.) regardless of which plan the
  operator actually intended to look at. The signal was
  cross-plan-by-construction and the operator's mental model of
  the right pane is plan-scoped; the mismatch read as noise.

  Session 66 reverts the home view to a minimal canvas: name the
  two next actions (classify a new topic, select an existing
  plan), nothing else. The per-plan dashboard inside `PlanReview`
  is unchanged — drill-down works exactly as before. The
  `refreshGlobalRecords` IPC + store field are retained but
  unused on boot; a later session can re-mount the cross-plan
  view in a dedicated screen if the use-case re-emerges.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { plans, refreshRecent } from '$stores/plans.svelte';
  import TopicInput from '$components/TopicInput.svelte';
  import RecentPlansList from '$components/RecentPlansList.svelte';
  import PlanReview from '$components/PlanReview.svelte';

  onMount(() => {
    // Populate the listing on app open. No LLM call. The cross-plan
    // dashboard warm-fetch was removed in Session 66 — see the
    // section comment above.
    refreshRecent();
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
        <!-- Session 66 — blank-canvas home view. Names the two
             next actions and stays out of the way. No
             cross-plan metrics here; per-plan drill-down inside
             PlanReview is the canonical "records view." -->
        <section class="home">
          <div class="home-inner">
            <p class="home-headline">
              {#if plans.recent.length === 0}
                no plans yet
              {:else}
                no plan selected
              {/if}
            </p>
            <p class="home-sub">
              {#if plans.recent.length === 0}
                type a topic above to classify your first plan.
              {:else}
                type a topic above to classify a new plan, or select
                an existing plan from the list to drill in.
              {/if}
            </p>
          </div>
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
  /* Session 66 — blank-canvas home view. The Session-63
     cross-plan dashboard rules were removed; the new home is a
     centered two-line hint that names the next action without
     filling the pane with stale pooled metrics. */
  .home {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 16px;
    background: var(--bg-canvas);
  }
  .home-inner {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 480px;
    text-align: center;
  }
  .home-headline {
    margin: 0;
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.005em;
    color: var(--fg-secondary);
  }
  .home-sub {
    margin: 0;
    font-size: 11px;
    color: var(--fg-tertiary);
    line-height: 1.55;
  }

  @media (max-width: 800px) {
    main { grid-template-columns: 1fr; }
    .left { max-height: 30vh; }
  }
</style>
