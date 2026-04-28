<!--
  StatusPill — a small lifecycle-state indicator.

  Renders a plan's `PlanStatusDto` as a colored, chip-shaped pill.
  Colors are categorical signals (per ADR 0006: chrome stays
  charcoal; color is a meaning, not decoration):

    pending   → neutral chrome (the no-action-yet state)
    accepted  → warm-amber (signal-warning) — the user-curated state
                that gates Phase-6 fetching. Amber rather than green
                because acceptance is "approved for fetch", not "data
                is fresh".
    rejected  → dim foreground (fg-quaternary border) — the row is
                hidden by default, so the pill rendering is mostly
                for the All filter view.

  Lives next to Chip rather than as a Chip variant because the
  semantics differ: a Chip is a label of arbitrary content, a
  StatusPill is a single closed enum's state.
-->
<script lang="ts">
  import type { PlanStatusDto } from '$lib/api/types/PlanStatusDto';

  interface Props {
    status: PlanStatusDto;
  }
  let { status }: Props = $props();
</script>

<span class="pill pill-{status}" title="status: {status}">
  {status}
</span>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    padding: 1px 6px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    color: var(--fg-secondary);
    white-space: nowrap;
  }
  .pill-pending {
    /* Neutral chrome — the default state. */
    color: var(--fg-secondary);
    border-color: var(--border-subtle);
  }
  .pill-accepted {
    color: var(--signal-warning);
    border-color: var(--signal-warning);
  }
  .pill-rejected {
    color: var(--fg-quaternary);
    border-color: var(--fg-quaternary);
  }
</style>
