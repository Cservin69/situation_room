<!--
  CostByTierPanel — operator-visible LLM cost-by-tier ledger
  (Session 75, candidate 1 from the Session 74 handoff).

  ## What this answers

  "Is the v1.22 prompt-cache restructure actually saving me tokens
  right now?" The Session-74 hypothesis was that moving every
  `{{VAR}}` into a tail `## Concrete inputs` section grows the
  stable cacheable prefix from ~3% to ~92%. The signal lives on
  every `complete()` response as `cached_input_tokens` — but until
  this panel landed the only way to read it was to grep INFO logs
  for `cached_tokens=Some(N)`. The dashboard tile renders the same
  signal as a glanceable ratio per (provider, tier).

  ## Layout

  One row per (provider, tier) bucket the binary has seen. Within
  a row:
    - left  — provider · tier label
    - mid   — total calls (raw count)
    - right — cache-hit ratio chip with "X% cached" copy on hover

  The chip is color-coded:
    - ≥ 50% → positive (the v1.22 lever is working)
    - 1-49% → info (partial warm; mixed shapes)
    - 0%    → muted (cold prefix, but provider reports the field)
    - "—"   → quaternary (provider doesn't expose cache metadata)

  ## What this panel does NOT do

  - **No dollar amounts.** Provider pricing drifts; the panel
    surfaces tokens (with the cache split) — the operator
    multiplies by the current $/1k themselves.
  - **No per-call timeline.** The ledger is cumulative for the
    binary session; per-call detail belongs in INFO logs, not
    here.
  - **No persistence.** Ledger resets on binary restart. The
    operator-visible value is "does the cache lever work right
    now?" — process-restart-clean is honest about that scope.
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { llmCostLedger } from '$lib/api/client';
  import type { LlmCostLedgerEntryDto } from '$lib/api/types/LlmCostLedgerEntryDto';

  let entries = $state<LlmCostLedgerEntryDto[]>([]);
  let lastUpdated = $state<Date | null>(null);
  let error = $state<string | null>(null);

  /**
   * Polling cadence. 15s is long enough that the IPC traffic stays
   * negligible (one tiny snapshot per cycle) and short enough that
   * the operator sees the cache-hit chip recover within a single
   * dashboard glance after a warm authoring call.
   */
  const POLL_INTERVAL_MS = 15_000;

  let pollHandle: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      entries = await llmCostLedger();
      lastUpdated = new Date();
      error = null;
    } catch (e) {
      // Cost telemetry is non-load-bearing — surface the message but
      // don't toast. A binary in startup or in a partial state might
      // legitimately return an error here.
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

  function formatCount(n: bigint | number): string {
    const v = typeof n === 'bigint' ? Number(n) : n;
    if (!Number.isFinite(v)) return '—';
    if (v < 1000) return String(v);
    return new Intl.NumberFormat('en-US', { maximumFractionDigits: 0 }).format(v);
  }

  /**
   * Compute the cache-hit ratio as a percentage string. Returns "—"
   * when the provider hasn't reported any cache metadata for this
   * bucket (so we don't claim a 0% hit ratio over zero observations).
   */
  function cacheHitLabel(e: LlmCostLedgerEntryDto): string {
    const denom = Number(e.calls_with_cache_data);
    if (denom <= 0) return '—';
    const input = Number(e.input_tokens);
    if (input <= 0) return '—';
    const cached = Number(e.cached_input_tokens);
    const ratio = cached / input;
    // Clamp 0..1 so a provider that double-counts cache bytes can't
    // render ">100%". One decimal point keeps the chip width stable.
    const clamped = Math.max(0, Math.min(1, ratio));
    const pct = clamped * 100;
    if (pct >= 99.95) return '100%';
    if (pct < 0.05) return '0%';
    return pct.toFixed(0) + '%';
  }

  /**
   * `data-sign` selector for the chip — keeps colour rules in CSS
   * (same pattern as MetricCard's delta chip). "muted" covers both
   * the "—" (no data) and "0%" (cold) cases; "info" is the partial-
   * warm middle band; "positive" lights up when the cache lever is
   * clearly working.
   */
  function cacheHitSign(e: LlmCostLedgerEntryDto): string {
    const denom = Number(e.calls_with_cache_data);
    if (denom <= 0) return 'quaternary';
    const input = Number(e.input_tokens);
    if (input <= 0) return 'muted';
    const ratio = Number(e.cached_input_tokens) / input;
    if (ratio >= 0.5) return 'positive';
    if (ratio > 0) return 'info';
    return 'muted';
  }

  function tierLabel(t: string): string {
    // Closed-vocab; ts-rs renders the enum as lowercase literals.
    return t;
  }

  /**
   * Session 80 — render label for the row's `purpose`. `null` means
   * "default shard" (classifier / recipe-author / propose-URL all
   * share); a string carries the extraction shard the call routed to
   * (`"extraction:document_assertions"` etc.). The label is rendered
   * compactly so the (provider · tier · purpose) row stays on one
   * line at the panel's standard width.
   */
  function purposeLabel(p: string | null): string {
    if (p === null) return 'default';
    // Strip the `extraction:` prefix for compactness when present —
    // operator already sees the (provider, tier) tuple; the shard
    // suffix is the distinguishing bit.
    if (p.startsWith('extraction:')) return p.slice('extraction:'.length);
    return p;
  }

  /**
   * Composite row key — provider · tier · purpose. Stable across
   * re-renders so `{#each}` doesn't churn the DOM when the snapshot
   * adds a new bucket. Using `null` as a sentinel keeps the default
   * shard distinct from any explicit string purpose.
   */
  function rowKey(e: LlmCostLedgerEntryDto): string {
    return e.provider + ':' + e.tier + ':' + (e.purpose ?? '<default>');
  }
</script>

<section class="cost-panel" aria-label="llm cost by tier">
  <header class="panel-header">
    <span>llm calls · by (provider, tier)</span>
    <span class="panel-coord">
      {#if entries.length === 0}
        — no calls yet this session
      {:else}
        {entries.length} bucket{entries.length === 1 ? '' : 's'}
      {/if}
    </span>
  </header>

  {#if error}
    <p class="error">
      cost ledger unavailable: {error}
    </p>
  {:else if entries.length === 0}
    <p class="empty">
      no LLM calls have completed in this binary session yet — classify
      a topic or run a fetch to populate.
    </p>
  {:else}
    <ul class="rows">
      {#each entries as e (rowKey(e))}
        <li class="row">
          <span class="label">
            <span class="provider">{e.provider}</span>
            <span class="sep">·</span>
            <span class="tier">{tierLabel(e.tier)}</span>
            <span class="sep">·</span>
            <span class="purpose" title={e.purpose ?? 'default cache shard (classifier / recipe-author / propose-URL share)'}>{purposeLabel(e.purpose)}</span>
          </span>
          <span class="calls" title="completion calls this session">
            {formatCount(e.calls)} call{Number(e.calls) === 1 ? '' : 's'}
          </span>
          <span class="tokens" title="input tokens · output tokens">
            {formatCount(e.input_tokens)} in / {formatCount(e.output_tokens)} out
          </span>
          <span
            class="cache-chip"
            data-sign={cacheHitSign(e)}
            title={`${formatCount(e.cached_input_tokens)} cached / ${formatCount(e.input_tokens)} input (over ${formatCount(e.calls_with_cache_data)} calls reporting cache data)`}
          >
            {cacheHitLabel(e)} cached
          </span>
        </li>
      {/each}
    </ul>
    {#if lastUpdated !== null}
      <p class="footnote">
        updated {lastUpdated.toLocaleTimeString()} · auto-refresh every
        {Math.round(POLL_INTERVAL_MS / 1000)}s · resets on binary restart
      </p>
    {/if}
  {/if}
</section>

<style>
  .cost-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
  }

  .panel-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 10px;
    font-weight: 500;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .panel-coord {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
    text-transform: none;
    letter-spacing: 0;
  }

  .empty {
    margin: 0;
    padding: 6px 4px;
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .error {
    margin: 0;
    padding: 6px 4px;
    font-size: 11px;
    color: var(--signal-negative);
  }

  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(140px, 1.2fr) minmax(70px, 0.6fr) minmax(140px, 1fr) auto;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }
  .label {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .provider {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-primary);
  }
  .sep {
    color: var(--fg-quaternary);
  }
  .tier {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-secondary);
  }
  /* Session 80 — purpose label. Reads as tertiary metadata next to
     the (provider, tier) tuple; mono + lower-case to match the
     cache-key wire forms (`document_assertions` etc.) without
     fighting the uppercase tier label for visual weight. */
  .purpose {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 18ch;
  }
  .calls, .tokens {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 10px;
    color: var(--fg-tertiary);
  }
  .cache-chip {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 2px;
    margin-left: auto;
    white-space: nowrap;
    border: 1px solid var(--border-subtle);
  }
  .cache-chip[data-sign='positive'] {
    color: var(--signal-positive);
    background: rgba(91, 198, 133, 0.08);
    border-color: rgba(91, 198, 133, 0.25);
  }
  .cache-chip[data-sign='info'] {
    color: var(--signal-info);
    background: var(--bg-panel-alt);
  }
  .cache-chip[data-sign='muted'] {
    color: var(--fg-tertiary);
    background: var(--bg-panel-alt);
  }
  .cache-chip[data-sign='quaternary'] {
    color: var(--fg-quaternary);
    background: var(--bg-panel-alt);
  }

  .footnote {
    margin: 0;
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
  }
</style>
