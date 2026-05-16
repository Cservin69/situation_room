<!--
  PromoteStatusPanel — operator-visible authoritative-registry +
  last-promote dashboard tile (Session 84, ADR 0004 / ADR 0021).

  ## What this answers

  Three operator-facing questions:

    1. "Which sources is the binary treating as authoritative right
       now?" — pulls from `authoritative_registry_summary` and renders
       the entry list inline. The registry is hot-reloaded (Session
       84): an edit to `config/vocab/authoritative_sources.toml`
       propagates here on the next polling cycle.

    2. "Did the last promote run actually do anything?" — pulls from
       `last_promote_summary` and renders a one-line summary of the
       most recent PromoteReport, with a chip for the trigger
       (`auto_after_fetch` vs `manual`). Replaces the "grep INFO logs"
       muscle memory that Session 82 left in place.

    3. "Did the dashboard miss anything from my last burst of
       activity?" — Session 85: pulls from `promote_history` and
       renders a strip of the last ~20 passes plus a
       "N runs in the last minute" caption when more than one
       auto-trigger fired inside the rolling window.

  ## Layout

  Single panel with three horizontally-stacked sections:

    - left   — registry summary: entry count + sample rows
    - middle — last-promote summary: counters + trigger chip + age
    - bottom — history strip (Session 85): one micro-cell per recent
      pass, hover-titled with the per-pass counters

  Stacks vertically on narrow widths to keep the same readability the
  CostByTierPanel sets above.

  ## What this panel does NOT do

  - **No edit UI.** The TOML is the source of truth; the panel is
    read-only. Editing happens in the operator's editor, then propagates
    via the hot-reload watcher.
-->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import {
    authoritativeRegistrySummary,
    lastPromoteSummary,
    promoteHistory,
  } from '$lib/api/client';
  import type { AuthorityRegistrySummaryDto } from '$lib/api/types/AuthorityRegistrySummaryDto';
  import type { LastPromoteSummaryDto } from '$lib/api/types/LastPromoteSummaryDto';
  import type { PromoteHistoryDto } from '$lib/api/types/PromoteHistoryDto';

  let registry = $state<AuthorityRegistrySummaryDto | null>(null);
  let last = $state<LastPromoteSummaryDto | null>(null);
  let history = $state<PromoteHistoryDto | null>(null);
  let lastUpdated = $state<Date | null>(null);
  let error = $state<string | null>(null);

  /**
   * Polling cadence. 10s is the trade-off between "operator sees the
   * post-hot-reload entry within a glance" (the file-mtime watcher
   * has a 2s cadence on the Rust side; the dashboard polling at 10s
   * adds at most one cycle of latency) and "negligible IPC cost".
   */
  const POLL_INTERVAL_MS = 10_000;

  let pollHandle: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      const [reg, ls, hist] = await Promise.all([
        authoritativeRegistrySummary(),
        lastPromoteSummary(),
        promoteHistory(),
      ]);
      registry = reg;
      last = ls;
      history = hist;
      lastUpdated = new Date();
      error = null;
    } catch (e) {
      // Non-load-bearing surface; surface the message but don't
      // toast. Binary in startup or a partial state might
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

  function formatQuorum(q: number | null): string {
    if (q === null || q === 1) return 'fast-track';
    return `quorum ≥ ${q}`;
  }

  function quorumSign(q: number | null): string {
    if (q === null || q === 1) return 'positive';
    return 'info';
  }

  /**
   * Trigger chip styling. `auto_after_fetch` is the common path —
   * keep it neutral. `manual` is the operator-driven button, slightly
   * warmer tone so the dashboard reflects "you just hit the button".
   */
  function triggerSign(t: string): string {
    if (t === 'manual') return 'info';
    return 'muted';
  }

  function formatTrigger(t: string): string {
    if (t === 'auto_after_fetch') return 'auto · after fetch';
    if (t === 'manual') return 'manual';
    return t;
  }

  /**
   * Render an age string for the last promote run. Closed-vocab
   * spans (`12s`, `4m`, `2h`, `3d`) so the cell stays one line.
   */
  function ageOf(iso: string): string {
    const parsed = Date.parse(iso);
    if (!Number.isFinite(parsed)) return '—';
    const ms = Date.now() - parsed;
    if (ms < 0) return 'just now';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    const d = Math.floor(h / 24);
    return `${d}d ago`;
  }

  /**
   * Closed-vocab name for an entry's scope: a topic-only entry is
   * `topic:Cu`, metric-only is `metric:production`, both is
   * `topic:Cu · metric:production`, neither is `*` (applies to
   * everything from this source).
   */
  function scopeLabel(
    metric: string | null,
    topic: string | null,
  ): string {
    const bits: string[] = [];
    if (topic) bits.push(`topic:${topic}`);
    if (metric) bits.push(`metric:${metric}`);
    return bits.length === 0 ? '*' : bits.join(' · ');
  }

  /**
   * Session 85 — per-cell signal for the history strip. Each cell
   * encodes one PromoteReport's "did this run actually do anything"
   * verdict via a four-state closed-vocab data-sign:
   *   - positive   → at least one record promoted (auth or consensus)
   *   - info       → considered > 0 but nothing new (skipped-already)
   *   - muted      → no Assertions considered at all
   *   - negative   → insert_failures > 0 (load-bearing failure)
   * The verdicts are mutually exclusive in practice; we evaluate
   * negative first so the failure case dominates the others.
   */
  function entrySign(e: LastPromoteSummaryDto): string {
    if (e.report.insert_failures > 0) return 'negative';
    const promoted =
      e.report.authoritative_promoted + e.report.groups_promoted;
    if (promoted > 0) return 'positive';
    if (e.report.assertions_considered > 0) return 'info';
    return 'muted';
  }

  /**
   * Tooltip text shown on hover. Captures the per-cell verdict in
   * one line so the operator doesn't need to click through to a
   * detail surface.
   */
  function entryTitle(e: LastPromoteSummaryDto): string {
    const promoted =
      e.report.authoritative_promoted + e.report.groups_promoted;
    return [
      `trigger: ${formatTrigger(e.trigger)}`,
      `${ageOf(e.at)}`,
      `considered: ${e.report.assertions_considered}`,
      `promoted: ${promoted} (auth ${e.report.authoritative_promoted} + consensus ${e.report.groups_promoted})`,
      e.report.insert_failures > 0
        ? `insert failures: ${e.report.insert_failures}`
        : null,
    ]
      .filter((s) => s !== null)
      .join('\n');
  }
</script>

<section class="promote-panel" aria-label="authoritative registry status">
  <header class="panel-header">
    <span>authoritative · promote</span>
    <span class="panel-coord">
      {#if registry === null}
        — loading
      {:else}
        {registry.entry_count} entr{registry.entry_count === 1 ? 'y' : 'ies'}
      {/if}
    </span>
  </header>

  {#if error}
    <p class="error">authority status unavailable: {error}</p>
  {:else}
    <div class="cols">
      <!-- Registry summary -->
      <div class="col col-registry">
        <h3 class="col-title">registry</h3>
        {#if registry === null}
          <p class="empty">loading…</p>
        {:else if registry.entry_count === 0}
          <p class="empty">
            no authoritative entries loaded — promote runs consensus only.
            Edit <code>{registry.source_path}</code> and the watcher
            will pick it up.
          </p>
        {:else}
          <ul class="entries">
            {#each registry.entries as e, i (i)}
              <li class="entry">
                <span class="source-id" title="source_id (claimant trailing portion)">
                  {e.source_id}
                </span>
                <span class="scope" title="metric / topic gate">
                  {scopeLabel(e.metric, e.topic)}
                </span>
                <span
                  class="quorum-chip"
                  data-sign={quorumSign(e.consensus_quorum)}
                  title={e.consensus_quorum === null || e.consensus_quorum === 1
                    ? 'authoritative fast-track: matching Assertions promote at N=1 immediately'
                    : `consensus_quorum override: matching groups promote at N=${e.consensus_quorum} instead of the global N=3`}
                >
                  {formatQuorum(e.consensus_quorum)}
                </span>
              </li>
            {/each}
          </ul>
          {#if registry.entries_capped}
            <p class="footnote">
              entries capped at the IPC ceiling — view the TOML for the
              complete list.
            </p>
          {/if}
          <p class="path" title={registry.source_path}>
            loaded from <code>{registry.source_path}</code>
          </p>
        {/if}
      </div>

      <!-- Last promote summary -->
      <div class="col col-last">
        <h3 class="col-title">last promote</h3>
        {#if last === null}
          <p class="empty">
            no promote pass has run yet in this binary session — kick
            off a fetch on an accepted plan to populate.
          </p>
        {:else}
          <div class="last-line">
            <span class="trigger-chip" data-sign={triggerSign(last.trigger)}>
              {formatTrigger(last.trigger)}
            </span>
            <span class="age">{ageOf(last.at)}</span>
          </div>
          <ul class="counters">
            <li>
              <span class="counter-label">considered</span>
              <span class="counter-value">{last.report.assertions_considered}</span>
            </li>
            <li>
              <span class="counter-label">authoritative</span>
              <span class="counter-value">{last.report.authoritative_promoted}</span>
            </li>
            <li>
              <span class="counter-label">consensus</span>
              <span class="counter-value">{last.report.groups_promoted}</span>
            </li>
            <li>
              <span class="counter-label">skipped</span>
              <span class="counter-value">{last.report.skipped_already_promoted}</span>
            </li>
            <li>
              <span class="counter-label">obs / ev / rel / attr</span>
              <span class="counter-value">
                {last.report.observations_emitted} / {last.report.events_emitted} / {last.report.relations_emitted} / {last.report.entity_attributes_emitted}
              </span>
            </li>
            {#if last.report.insert_failures > 0}
              <li class="counter-failures">
                <span class="counter-label">insert failures</span>
                <span class="counter-value">{last.report.insert_failures}</span>
              </li>
            {/if}
          </ul>
        {/if}
      </div>
    </div>

    <!-- Session 85 — history strip + cross-plan trigger counter -->
    {#if history !== null && history.entries.length > 0}
      <div class="history-strip" aria-label="recent promote passes">
        <div class="history-header">
          <span class="history-title">recent passes</span>
          <span class="history-coord">
            {history.entries.length} · cap {history.capacity}
            {#if history.auto_triggers_last_minute > 1}
              <span class="trigger-counter"
                title="auto-trigger fired {history.auto_triggers_last_minute} times in the last {history.trigger_window_seconds}s; the last-promote tile above only shows the most recent — use this strip to see the rest">
                · {history.auto_triggers_last_minute} runs in the last {history.trigger_window_seconds}s
              </span>
            {/if}
          </span>
        </div>
        <ul class="history-cells">
          {#each history.entries as e, i (i)}
            <li
              class="history-cell"
              data-sign={entrySign(e)}
              title={entryTitle(e)}
            ></li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if lastUpdated !== null}
      <p class="footnote">
        updated {lastUpdated.toLocaleTimeString()} · auto-refresh every
        {Math.round(POLL_INTERVAL_MS / 1000)}s · hot-reloads on TOML edit
      </p>
    {/if}
  {/if}
</section>

<style>
  .promote-panel {
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

  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  @media (max-width: 720px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }

  .col {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .col-title {
    margin: 0;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }

  .empty {
    margin: 0;
    padding: 4px 0;
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .error {
    margin: 0;
    padding: 6px 4px;
    font-size: 11px;
    color: var(--signal-negative);
  }

  .entries {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 180px;
    overflow-y: auto;
  }
  .entry {
    display: grid;
    grid-template-columns: minmax(110px, 1.1fr) minmax(120px, 1.4fr) auto;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }
  .source-id {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scope {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .quorum-chip {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 2px;
    margin-left: auto;
    white-space: nowrap;
    border: 1px solid var(--border-subtle);
  }
  .quorum-chip[data-sign='positive'] {
    color: var(--signal-positive);
    background: rgba(91, 198, 133, 0.08);
    border-color: rgba(91, 198, 133, 0.25);
  }
  .quorum-chip[data-sign='info'] {
    color: var(--signal-info);
    background: var(--bg-panel-alt);
  }

  .path {
    margin: 0;
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path code {
    color: var(--fg-tertiary);
  }

  .last-line {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .trigger-chip {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 2px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    border: 1px solid var(--border-subtle);
  }
  .trigger-chip[data-sign='info'] {
    color: var(--signal-info);
    background: var(--bg-panel-alt);
  }
  .trigger-chip[data-sign='muted'] {
    color: var(--fg-tertiary);
    background: var(--bg-panel-alt);
  }
  .age {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-quaternary);
  }

  .counters {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 4px 12px;
  }
  .counters li {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 10px;
  }
  .counter-label {
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .counter-value {
    color: var(--fg-primary);
  }
  .counter-failures .counter-value {
    color: var(--signal-warning);
  }

  .footnote {
    margin: 0;
    font-size: 10px;
    color: var(--fg-quaternary);
    font-family: var(--font-mono);
  }

  /* Session 85 — history strip */
  .history-strip {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 0 0 0;
    border-top: 1px solid var(--border-subtle);
  }
  .history-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 10px;
    color: var(--fg-tertiary);
    font-family: var(--font-mono);
  }
  .history-title {
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .history-coord {
    color: var(--fg-quaternary);
  }
  .trigger-counter {
    color: var(--signal-info);
    margin-left: 4px;
  }
  .history-cells {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: row;
    gap: 3px;
    align-items: stretch;
  }
  .history-cell {
    flex: 1 1 0;
    min-width: 0;
    height: 14px;
    border-radius: 2px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
  }
  .history-cell[data-sign='positive'] {
    background: rgba(91, 198, 133, 0.55);
    border-color: rgba(91, 198, 133, 0.7);
  }
  .history-cell[data-sign='info'] {
    background: rgba(120, 144, 200, 0.35);
    border-color: rgba(120, 144, 200, 0.5);
  }
  .history-cell[data-sign='muted'] {
    background: var(--bg-panel-alt);
  }
  .history-cell[data-sign='negative'] {
    background: rgba(220, 90, 90, 0.55);
    border-color: rgba(220, 90, 90, 0.7);
  }
</style>
