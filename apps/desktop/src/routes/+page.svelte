<!--
  +page.svelte — the only screen, for now.

  Three-pane layout:
    - Top bar: TopicInput (P3).
    - Left: RecentPlansList (P2).
    - Right: PlanReview (P1) when a plan is selected; empty-state
             prompt otherwise.

  Single SPA route. The handoff is explicit that Session 6 does not
  build out routing for separate listing / review URLs — the pane
  composition is the navigation model. If we later want
  bookmarkable plan URLs, that's an add-on, not a redesign.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { plans, refreshRecent } from '$stores/plans.svelte';
  import TopicInput from '$components/TopicInput.svelte';
  import RecentPlansList from '$components/RecentPlansList.svelte';
  import PlanReview from '$components/PlanReview.svelte';

  onMount(() => {
    // Populate the listing on app open. No LLM call.
    refreshRecent();
  });
</script>

<div class="app">
  <header class="topbar">
    <div class="brand">
      <span class="name">Stockpile</span>
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
        <div class="empty">
          <p>
            {#if plans.recent.length === 0}
              type a topic above to begin.
            {:else}
              select a plan from the list, or classify a new topic.
            {/if}
          </p>
        </div>
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
  .empty {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--fg-tertiary);
    font-size: 13px;
  }

  @media (max-width: 800px) {
    main { grid-template-columns: 1fr; }
    .left { max-height: 30vh; }
  }
</style>
