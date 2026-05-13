<!--
  NominationAttempts — Session 52 piece B.

  Renders the cross-run chronology for a single nomination — one
  line per fetch run that touched it, newest first, with the
  outcome shape and a head of the message string. Designed to slot
  into `ExpectationRow`'s expanded panel (the `extras` snippet
  prop) so clicking a Document-bucket row reveals the row's
  rationale and, immediately below, the per-run history.

  ## Why cross-run, not intra-run

  The propose-URL retry loop tries up to three URLs per run before
  declining (`fetch_executor::author_one`). Those intra-run attempts
  are summarised inline in the decline `message` field
  (`url proposer declined after N attempt(s): … ; attempts: …`),
  but are not exposed as separate wire rows — there's no per-attempt
  IPC surface today (Session 50 deferred the live channel; intra-
  run history would need its own).

  What *is* persisted is per-run: `fetch_run_outcomes` carries one
  row per (recipe-or-source, run) for the kept window (default 20
  runs). That's the chronology this component renders. The intra-
  run attempts surface as part of each row's `message` head, which
  the operator can read in the row's `title=` tooltip on hover or
  in the expanded line itself.

  ## Empty state

  A nomination with zero history rows means: either the plan was
  freshly classified and no fetch has been run, or the plan's runs
  predate the Session-46 outcomes-history migration. The component
  renders a small italic hint rather than nothing, so the operator
  sees the surface exists and is up-to-date — not a stale or
  broken slot.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import { runsForNomination } from '$lib/nominationOutcomes';

  interface Props {
    nominationId: string;
  }
  let { nominationId }: Props = $props();

  let runs = $derived(runsForNomination(plans.outcomesHistory, nominationId));

  function shortRunId(id: string): string {
    return id.slice(0, 8);
  }

  function whenLabel(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

  /**
   * Cap message length to keep the chronology line scannable; the
   * full message is in the entry's `title=` attr for hover.
   */
  function shortMessage(m: string | null): string {
    if (!m) return '';
    if (m.length <= 160) return m;
    return m.slice(0, 157) + '…';
  }

  function entryTitle(
    cell: { run_id: string; attempted_at: string; outcome_kind: string; message: string | null },
  ): string {
    const lines = [
      `run ${cell.run_id} · ${whenLabel(cell.attempted_at)}`,
      cell.outcome_kind,
    ];
    if (cell.message) lines.push(cell.message);
    return lines.join('\n');
  }
</script>

<section class="chronology">
  <span class="head">prior attempts</span>
  {#if runs.length === 0}
    <p class="empty">no fetch attempts yet for this nomination</p>
  {:else}
    <ol class="entries">
      {#each runs as cell, i (cell.run_id + ':' + i)}
        <li
          class="entry"
          data-kind={cell.outcome_kind}
          title={entryTitle(cell)}
        >
          <span class="when">{whenLabel(cell.attempted_at)}</span>
          <span class="kind">{cell.outcome_kind}</span>
          <span class="run">run {shortRunId(cell.run_id)}</span>
          {#if cell.message}
            <span class="msg">{shortMessage(cell.message)}</span>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .chronology {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 0;
  }
  .head {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-quaternary);
  }
  .empty {
    margin: 0;
    color: var(--fg-quaternary);
    font-size: 10px;
    font-style: italic;
  }
  .entries {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
  }
  .entry {
    display: grid;
    grid-template-columns: minmax(0, auto) minmax(0, auto) minmax(0, auto) minmax(0, 1fr);
    column-gap: 8px;
    align-items: baseline;
    padding: 3px 0;
    border-top: 1px solid var(--border-subtle);
    cursor: help;
  }
  .entry:first-child {
    border-top: 0;
  }
  .when {
    color: var(--fg-secondary);
    white-space: nowrap;
  }
  .kind {
    color: var(--fg-primary);
    text-transform: lowercase;
    white-space: nowrap;
  }
  .entry[data-kind='succeeded'] .kind {
    color: var(--signal-positive);
  }
  .entry[data-kind='failed'] .kind {
    color: var(--signal-negative);
  }
  .entry[data-kind='rate_limited'] .kind {
    color: var(--signal-warning);
  }
  .entry[data-kind='declined'] .kind {
    color: var(--fg-tertiary);
  }
  .run {
    color: var(--fg-quaternary);
    white-space: nowrap;
  }
  .msg {
    color: var(--fg-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
</style>
