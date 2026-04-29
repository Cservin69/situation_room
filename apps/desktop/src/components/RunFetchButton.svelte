<!--
  RunFetchButton — kicks off the Phase-6 fetch executor for the
  currently-selected plan.

  Visible only when the plan is in the `accepted` state. A pending
  plan can't be fetched (the executor rejects with InvalidInput);
  a rejected plan likewise can't. Hiding the button rather than
  disabling it matches the accept/reject pattern in PlanReview:
  the UI shows what's actionable, not what's theoretically possible.

  No "are you sure?" confirmation — fetch is read-only against
  external sources from the user's perspective and can be re-run
  freely. Each run produces a fresh row in `fetch_runs`.
-->
<script lang="ts">
  import { plans, runFetch } from '$stores/plans.svelte';

  async function onClick() {
    await runFetch();
  }
</script>

{#if plans.selected && plans.selected.status === 'accepted'}
  <button
    type="button"
    class="btn"
    disabled={plans.fetching}
    onclick={onClick}
  >
    {#if plans.fetching}
      fetching…
    {:else}
      run fetch
    {/if}
  </button>
{/if}

<style>
  /* Mirrors the button system in PlanReview; lifted here as the
     "primary action" of the fetch flow. Warm-amber matches the
     accept/proceed semantic from ADR 0006. */
  .btn {
    background: transparent;
    border: 1px solid var(--signal-warning);
    border-radius: 2px;
    padding: 4px 10px;
    font-family: var(--font-mono);
    font-size: 11px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    cursor: pointer;
    color: var(--signal-warning);
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .btn:hover:not(:disabled) {
    background: rgba(224, 165, 46, 0.1);
  }
  .btn:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
</style>
