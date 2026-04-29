<!--
  PlanFilterStrip — All / Pending / Accepted / Rejected toggle.

  Sits above the recent-plans listing. Selecting a chip refreshes the
  listing through `setStatusFilter`, which routes the choice through
  the api with the right wire mapping (`'all'` → null, others
  pass-through).

  Default lands on Pending (Session 7 §P1: "the user lands on what
  needs review"). The active chip gets a strong-border treatment;
  inactive chips are quiet chrome.
-->
<script lang="ts">
  import { plans, setStatusFilter, type StatusFilter } from '$stores/plans.svelte';

  const options: { value: StatusFilter; label: string }[] = [
    { value: 'pending',  label: 'pending'  },
    { value: 'accepted', label: 'accepted' },
    { value: 'rejected', label: 'rejected' },
    { value: 'all',      label: 'all'      },
  ];
</script>

<div class="strip" role="tablist" aria-label="filter recent plans by status">
  {#each options as opt (opt.value)}
    <button
      type="button"
      role="tab"
      class="opt"
      class:active={plans.statusFilter === opt.value}
      aria-selected={plans.statusFilter === opt.value}
      onclick={() => setStatusFilter(opt.value)}
    >
      {opt.label}
    </button>
  {/each}
</div>

<style>
  .strip {
    display: flex;
    gap: 4px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .opt {
    flex: 0 0 auto;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    padding: 3px 8px;
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    color: var(--fg-tertiary);
    cursor: pointer;
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .opt:hover {
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
  }
  .opt:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .opt.active {
    color: var(--fg-primary);
    border-color: var(--border-strong);
    background: var(--bg-panel-alt);
  }
</style>
