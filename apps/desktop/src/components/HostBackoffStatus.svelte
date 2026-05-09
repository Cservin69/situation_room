<!--
  HostBackoffStatus — Session 48, piece B.

  A compact strip rendering the per-host adaptation state the network
  layer has observed during this binary's session. Reads
  `plans.hostBackoff` (refreshed on a 5s poll while a plan is selected;
  see the store's `startHostBackoffPolling` / `stopHostBackoffPolling`).

  ## Why this surface earns its weight

  Before Session 48 the per-host backoff state was process-internal —
  the only externally-visible signal was a slow fetch run, with no way
  to tell whether the slowness was "the host is throttling us" or
  "the request is in transit." Session 45 introduced the adaptation
  layer; this component is the operator-side window into what it has
  observed. It also closes a long-standing principle gap: the layer's
  decisions (when to back off, by how long) are derived entirely from
  observed signals (429, Retry-After, timeouts, successes), so showing
  the operator the same observations the layer used is the most
  honest summary available.

  ## Three states per host

  Each row falls into one of three states. The handoff explicitly
  flagged this as a wrong-fix risk — surfacing only `wait_remaining`
  collapses the "recovering" state (counter > 0, wait == 0) into the
  "clean" state (counter == 0, wait == 0). This component renders
  both fields explicitly and uses a visible tone to disambiguate.

    - clean       → counter == 0, wait == 0  (host has succeeded at
                    least once this session; no failure pressure)
    - recovering  → counter > 0,  wait == 0  (schedule expired, but
                    the failure history is still in effect for the
                    next observed signal)
    - blocked     → counter > 0,  wait > 0   (the next request will
                    sleep at least the remaining wait before firing)

  ## Empty state

  No rows mean the layer has not observed a signal for any host in
  this session. That's the legitimate state for a freshly-booted
  binary; render the panel header with an explainer rather than a
  blank box so the operator sees the surface but understands why
  it's empty. (Same posture `RecipeOutcomesHeatmap` takes for its
  pre-Session-46 plan empty state.)

  ## Polling lifecycle

  This component is purely declarative — it reads from
  `plans.hostBackoff`. The polling lifecycle (start on `selectPlan`,
  stop on `clearSelection`) lives in the store; mounting/unmounting
  this component does not start or stop the timer. That keeps the
  polling cadence tied to the operator's "looking at any plan"
  intent rather than the component's render lifecycle.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { HostBackoffSnapshotDto } from '$lib/api/types/HostBackoffSnapshotDto';

  /**
   * Closed-tone vocabulary mirroring `outcomes.ts` so the panel
   * shares the same colour palette as the FetchReport / Heatmap
   * surfaces. `clean` is positive (the layer has touched this host
   * and seen success); `recovering` is warning (the host's failure
   * history is in effect for the next signal); `blocked` is negative
   * (the next request will be delayed).
   */
  type BackoffTone = 'clean' | 'recovering' | 'blocked';

  function rowTone(row: HostBackoffSnapshotDto): BackoffTone {
    // ts-rs v8+ emits Rust `u64` as TS `bigint`; coerce to Number for
    // the comparison. Wait values are bounded by the schedule
    // (capped at 60s) so the coercion is lossless in practice.
    const wait = Number(row.wait_seconds_remaining);
    if (row.consecutive_failures === 0 && wait === 0) return 'clean';
    if (row.consecutive_failures > 0 && wait === 0) return 'recovering';
    return 'blocked';
  }

  function toneLabel(t: BackoffTone): string {
    switch (t) {
      case 'clean':
        return 'clean';
      case 'recovering':
        return 'recovering';
      case 'blocked':
        return 'blocked';
    }
  }

  /**
   * Format a whole-seconds wait into a short human string ("45s",
   * "2m 5s"). Values above 60s won't appear in practice (the
   * adaptation layer caps the schedule at 60s), but the formatter
   * handles the long form defensively in case the operator hits a
   * server-supplied Retry-After above that ceiling.
   */
  function formatWait(secsBig: bigint | number): string {
    const secs = Number(secsBig);
    if (!Number.isFinite(secs) || secs <= 0) return '—';
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${m}m ${s}s`;
  }

  /**
   * Stable key per row. The host string is the natural unique
   * identifier (HostBackoff's map is keyed by lowercased host).
   */
  function rowKey(row: HostBackoffSnapshotDto): string {
    return row.host;
  }

  /**
   * Sort rows so the operator sees blocked hosts first (they're the
   * actionable state), then recovering, then clean. Within a tone,
   * sort by host alphabetically so the strip is visually stable
   * across polling refreshes (HashMap iteration order isn't).
   */
  let sortedRows = $derived(
    [...plans.hostBackoff].sort((a, b) => {
      const order: Record<BackoffTone, number> = {
        blocked: 0,
        recovering: 1,
        clean: 2,
      };
      const ta = rowTone(a);
      const tb = rowTone(b);
      if (ta !== tb) return order[ta] - order[tb];
      return a.host.localeCompare(b.host);
    }),
  );
</script>

{#if plans.hostBackoff.length > 0}
  <section class="backoff">
    <header class="head">
      <span class="label">host backoff</span>
      <span class="hint">
        {plans.hostBackoff.length}
        {plans.hostBackoff.length === 1 ? 'host' : 'hosts'} · this session
      </span>
    </header>
    <ul class="rows">
      {#each sortedRows as row (rowKey(row))}
        <li class="row" data-tone={rowTone(row)}>
          <span class="host" title={row.host}>{row.host}</span>
          <span class="state">
            <span class="state-label">{toneLabel(rowTone(row))}</span>
          </span>
          <span class="counters">
            <span
              class="counter"
              title="consecutive failures since last success"
            >
              fails: <strong>{row.consecutive_failures}</strong>
            </span>
            <span class="counter" title="seconds until next request may fire">
              wait: <strong>{formatWait(row.wait_seconds_remaining)}</strong>
            </span>
          </span>
        </li>
      {/each}
    </ul>
  </section>
{:else}
  <!--
    Empty state: a freshly-booted binary or a session that has only
    fetched against one host that always succeeds. The adaptation
    layer is operating; it just has nothing to report. Same posture
    RecipeOutcomesHeatmap takes for its empty state.
  -->
  <section class="backoff empty">
    <header class="head">
      <span class="label">host backoff</span>
    </header>
    <p class="empty-explainer">
      No host signals observed this session. Run a fetch to populate.
    </p>
  </section>
{/if}

<style>
  .backoff {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }

  .backoff.empty {
    /* Same dimming RecipeOutcomesHeatmap uses for its empty state.
       Keeps the slot's chrome consistent across populated/empty
       transitions so the operator's eye doesn't have to re-locate
       the panel. */
    opacity: 0.7;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .hint {
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
  }

  .empty-explainer {
    margin: 0;
    color: var(--fg-tertiary);
    font-size: 11px;
    font-style: italic;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .row {
    display: grid;
    grid-template-columns: minmax(160px, 320px) 100px 1fr;
    column-gap: 12px;
    align-items: baseline;
    padding: 4px 6px;
    border-left: 2px solid var(--border-subtle);
    font-family: var(--font-mono);
    font-size: 11px;
    background: var(--bg-panel);
    transition: background var(--duration-ui) var(--ease);
  }
  .row:hover {
    background: var(--bg-panel-alt);
  }

  /*
    Tone-driven left-border colour. Mirrors FetchReport's row-border
    convention — green for the "this is fine" state, amber for
    "needs attention but not actionable yet", red for "the next
    request will be delayed."
  */
  .row[data-tone='clean'] {
    border-left-color: var(--signal-positive);
  }
  .row[data-tone='recovering'] {
    border-left-color: var(--signal-warning);
  }
  .row[data-tone='blocked'] {
    border-left-color: var(--signal-negative);
  }

  .host {
    color: var(--fg-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .state-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .row[data-tone='clean'] .state-label {
    color: var(--signal-positive);
  }
  .row[data-tone='recovering'] .state-label {
    color: var(--signal-warning);
  }
  .row[data-tone='blocked'] .state-label {
    color: var(--signal-negative);
  }

  .counters {
    display: flex;
    gap: 14px;
    flex-wrap: wrap;
  }
  .counter {
    color: var(--fg-quaternary);
  }
  .counter strong {
    color: var(--fg-primary);
    font-weight: 600;
  }
</style>
