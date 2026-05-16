<!--
  CostTimelinePanel — per-call cost timeline (Session 81).

  ## What this answers

  Sibling to CostByTierPanel. The cost-by-tier panel shows cumulative
  bucket tallies and a cache-hit ratio per (provider, tier, purpose).
  That tells the operator "how much have we spent in this bucket",
  which is the right shape for the v1.22 prompt-cache-lever question.

  This panel shows the *most-recent 50 LLM calls in order*. The same
  signal is in `cost_by_tier` over a long enough horizon, but the
  cumulative bucket hides bursty-shape behaviour: a 30-call extraction
  surge spread across 5 seconds reads as the same blob as 30 calls
  spread across 5 minutes once they all land in the same bucket. The
  timeline preserves the burstiness so the operator can see it.

  ## Layout

  One row per entry, newest-first. Each row:
    - left  — time (HH:MM:SS local)
    - mid   — provider · tier · purpose
    - right — input / output / cached tokens

  Tokens render as a `120 + 80 + 96` triplet (input + output + cached)
  with the cached chunk dimmed when the value is non-trivial; trivial
  zeros render as `·` so dense data doesn't drown in zeros.

  ## What this panel does NOT do

  - **No persistence.** Same as CostByTierPanel: ring buffer is in
    binary memory; restart-clean. The operator-visible value is "what
    just happened" — that's the right scope.
  - **No dollar amounts.** Same reason as the cost-by-tier panel:
    pricing tables drift per provider, tokens are the more stable
    portable unit.
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { llmCostTimeline } from '$lib/api/client';
  import type { LlmCostTimelineEntryDto } from '$lib/api/types/LlmCostTimelineEntryDto';

  let entries = $state<LlmCostTimelineEntryDto[]>([]);
  let lastUpdated = $state<Date | null>(null);
  let error = $state<string | null>(null);

  /**
   * 10s. Slightly tighter than CostByTierPanel's 15s because the
   * timeline is the surface operators reach for *during* a fetch run
   * to watch costs accrue, not the steady-state-glance surface the
   * cost-by-tier tile is. Still bounded so a panel left open
   * overnight isn't ringing the IPC bell every second.
   */
  const POLL_INTERVAL_MS = 10_000;

  let pollHandle: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      entries = await llmCostTimeline();
      lastUpdated = new Date();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    refresh();
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS);
  });

  onDestroy(() => {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  });

  // Newest-first for the UI. The Rust ring buffer surfaces oldest-
  // first (insertion order) so the operator's eye lands on the
  // freshest call without scrolling.
  let displayEntries = $derived([...entries].reverse());

  function formatTime(iso: string): string {
    try {
      const d = new Date(iso);
      if (Number.isNaN(d.valueOf())) return '?';
      const hh = String(d.getHours()).padStart(2, '0');
      const mm = String(d.getMinutes()).padStart(2, '0');
      const ss = String(d.getSeconds()).padStart(2, '0');
      return `${hh}:${mm}:${ss}`;
    } catch {
      return '?';
    }
  }

  function purposeLabel(p: string | null): string {
    if (p === null) return 'default';
    return p.startsWith('extraction:') ? p.slice('extraction:'.length) : p;
  }

  function tokensCell(n: number | null): string {
    if (n === null) return '—';
    return n.toLocaleString();
  }
</script>

<section class="timeline-panel" aria-label="LLM cost timeline">
  <header class="head">
    <span class="title">cost timeline</span>
    <span class="meta">
      {#if lastUpdated}
        <span title="last updated">{formatTime(lastUpdated.toISOString())}</span>
      {/if}
      <span class="count">· {entries.length} call{entries.length === 1 ? '' : 's'}</span>
    </span>
  </header>

  {#if error !== null}
    <p class="error" role="alert">cost timeline failed to refresh: {error}</p>
  {/if}

  {#if entries.length === 0}
    <p class="empty">no LLM calls yet this session.</p>
  {:else}
    <ul class="rows">
      {#each displayEntries as e (e.timestamp + e.provider)}
        <li class="row">
          <span class="ts" title={e.timestamp}>{formatTime(e.timestamp)}</span>
          <span class="bucket">
            <span class="prov">{e.provider}</span>
            <span class="sep">·</span>
            <span class="tier">{e.tier}</span>
            <span class="sep">·</span>
            <span class="purpose" title={e.purpose ?? 'default'}>{purposeLabel(e.purpose)}</span>
          </span>
          <span class="tokens" title="input / output / cached input">
            <span class="in">in {tokensCell(e.input_tokens)}</span>
            <span class="out">· out {tokensCell(e.output_tokens)}</span>
            <span class="cached">· cached {tokensCell(e.cached_input_tokens)}</span>
          </span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .timeline-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
  }
  .title {
    font-weight: 500;
  }
  .meta {
    font-family: var(--font-mono);
    color: var(--fg-quaternary);
    text-transform: none;
    letter-spacing: 0;
    font-size: 10px;
  }
  .count {
    margin-left: 4px;
  }
  .error {
    margin: 0;
    padding: 8px;
    font-size: 11px;
    color: var(--fg-tertiary);
    background: var(--bg-inset);
    border-radius: 3px;
  }
  .empty {
    margin: 0;
    padding: 12px;
    font-size: 11px;
    color: var(--fg-tertiary);
    background: var(--bg-inset);
    border: 1px dashed var(--border-subtle);
    border-radius: 3px;
    text-align: center;
  }
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 280px;
    overflow-y: auto;
  }
  .row {
    display: grid;
    grid-template-columns: 70px 1fr auto;
    gap: 10px;
    padding: 2px 6px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-primary);
    border-bottom: 1px solid var(--border-subtle);
  }
  .row:last-child {
    border-bottom: none;
  }
  .ts {
    color: var(--fg-secondary);
  }
  .bucket {
    display: inline-flex;
    gap: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .prov {
    color: var(--fg-primary);
  }
  .tier {
    color: var(--fg-secondary);
  }
  .purpose {
    color: var(--fg-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 24ch;
  }
  .sep {
    color: var(--fg-quaternary);
  }
  .tokens {
    color: var(--fg-secondary);
    white-space: nowrap;
  }
  .tokens .cached {
    color: var(--fg-tertiary);
  }
</style>
