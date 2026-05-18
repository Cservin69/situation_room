<!--
  PromoteDetailDrawer — click-through detail for one history-strip
  cell on the PromoteStatusPanel (Session 86, Sn-85 candidate 3).

  ## What this component answers

  The Session-85 history strip renders one micro-cell per recent
  promote pass, hover-titled with a compact counter list. That hover
  is fine for "did this pass do anything?" but it doesn't let the
  operator read the full PromoteReport when a specific cell looks
  off — the page-source HTML tooltip caps at ~5 lines on most
  platforms and isn't selectable.

  Clicking any history cell opens this drawer with the full
  per-counter breakdown for that single pass, the trigger and exact
  timestamp, and the plan id (copy-able for grep'ing logs).

  ## Why a modal, not an expand-in-place row

  The strip's value is "all N passes at a glance"; expanding any one
  cell would push every later cell off-screen and lose the at-a-
  glance contract. The DocumentDrawer pattern (Session 70) is the
  established way to break out of the dashboard for one record's
  full body — same posture here.

  ## Session 87 — Promoted record-id list

  PromoteReport now carries `promoted_record_ids`: the per-pass
  list of record ids each insert produced (stamped at insert-time
  inside `crates/pipeline/src/promote.rs`). The drawer surfaces
  those ids as a tail section ("records produced by this pass")
  with a copy button per id. The Sn-86-vintage history rows that
  don't carry the field (deserialised with an empty Vec via
  `#[serde(default)]`) just don't render the section.

  ## What this component does NOT do

  - **No editing.** Promote reports are immutable historical artifacts.
  - **No re-run button.** The PromoteStatusPanel above has the manual
    promote button; re-running from a historical-pass cell would
    invite operator confusion ("did this re-run write a new entry?
    Yes it did, but to the strip's left, not in place").
  - **No record-id → record-type cross-resolution.** The id list
    is opaque on the wire; the operator copies an id and uses it
    in DuckDB or the records dashboard. A future session might
    join across the six per-type tables to colour-code ids by
    record_type, but that's product-shape work, not infra-debt.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import CopyButton from '$components/common/CopyButton.svelte';
  import type { LastPromoteSummaryDto } from '$lib/api/types/LastPromoteSummaryDto';
  import {
    recordTypesForIds,
    type RecordTypeTag,
  } from '$lib/api/client';

  interface Props {
    /** The history-strip cell's underlying entry. Required; parent
     * only renders this component when a cell has been clicked. */
    entry: LastPromoteSummaryDto;
    /** Close callback — wired to Escape, backdrop click, close button. */
    onClose: () => void;
  }
  let { entry, onClose }: Props = $props();

  // ---- Derived display -------------------------------------------

  /** Mirror PromoteStatusPanel's trigger labeller for visual consistency. */
  function formatTrigger(t: string): string {
    if (t === 'auto_after_fetch') return 'auto · after fetch';
    if (t === 'manual') return 'manual';
    return t;
  }
  function triggerSign(t: string): string {
    if (t === 'manual') return 'info';
    return 'muted';
  }

  /** Mirror PromoteStatusPanel's per-cell verdict — operator should
   *  recognise the same four-state classification they hovered over. */
  function verdictSign(e: LastPromoteSummaryDto): string {
    if (e.report.insert_failures > 0) return 'negative';
    const promoted = e.report.authoritative_promoted + e.report.groups_promoted;
    if (promoted > 0) return 'positive';
    if (e.report.assertions_considered > 0) return 'info';
    return 'muted';
  }

  /** Human-readable verdict sentence. The header chip is colour;
   *  this sentence is the why so the operator doesn't have to
   *  translate the data-sign mentally. */
  function verdictText(e: LastPromoteSummaryDto): string {
    if (e.report.insert_failures > 0) {
      return `${e.report.insert_failures} insert failure${e.report.insert_failures === 1 ? '' : 's'} — investigate`;
    }
    const promoted = e.report.authoritative_promoted + e.report.groups_promoted;
    if (promoted > 0) {
      return `${promoted} record${promoted === 1 ? '' : 's'} promoted (auth ${e.report.authoritative_promoted} + consensus ${e.report.groups_promoted})`;
    }
    if (e.report.assertions_considered > 0) {
      return `${e.report.assertions_considered} considered, nothing new promoted (skipped or below quorum)`;
    }
    return 'no Assertions considered — empty pass';
  }

  /** Format the timestamp as both human local time + ISO for grep. */
  function formatAt(iso: string): { human: string; iso: string } {
    const parsed = Date.parse(iso);
    if (!Number.isFinite(parsed)) return { human: iso, iso };
    const d = new Date(parsed);
    // Locale-aware human form; the ISO under it is exact for
    // log-grep purposes.
    return {
      human: d.toLocaleString(),
      iso: d.toISOString(),
    };
  }

  let atFormatted = $derived(formatAt(entry.at));

  /** Closed-vocab counter list. Mirrors the structure of
   *  PromoteReport in `crates/pipeline/src/promote.rs` so the
   *  operator can scan top-to-bottom in the same order code reads. */
  type Counter = {
    label: string;
    value: number;
    emphasise: 'normal' | 'warn';
    hint: string;
  };
  let counters: Counter[] = $derived([
    {
      label: 'considered',
      value: entry.report.assertions_considered,
      emphasise: 'normal',
      hint: 'Assertion rows the pass walked. 0 means the plan has nothing yet to promote.',
    },
    {
      label: 'authoritative',
      value: entry.report.authoritative_promoted,
      emphasise: 'normal',
      hint: 'Promoted via ADR 0004 pathway 1 (single authoritative source, N=1 fast-track).',
    },
    {
      label: 'consensus',
      value: entry.report.groups_promoted,
      emphasise: 'normal',
      hint: 'Promoted via ADR 0004 pathway 2 (multiple-source consensus quorum hit).',
    },
    {
      label: 'skipped',
      value: entry.report.skipped_already_promoted,
      emphasise: 'normal',
      hint: 'Records already promoted on a prior pass (idempotency hit). Healthy when > 0 after the second run.',
    },
    {
      label: 'observations',
      value: entry.report.observations_emitted,
      emphasise: 'normal',
      hint: 'Promoted Observations this run. Sub-counter of authoritative + consensus.',
    },
    {
      label: 'events',
      value: entry.report.events_emitted,
      emphasise: 'normal',
      hint: 'Promoted Events this run.',
    },
    {
      label: 'relations',
      value: entry.report.relations_emitted,
      emphasise: 'normal',
      hint: 'Promoted Relations this run.',
    },
    {
      label: 'entity attributes',
      value: entry.report.entity_attributes_emitted,
      emphasise: 'normal',
      hint: 'Consensus-stamped EntityAttribute Assertions this run.',
    },
    {
      label: 'insert failures',
      value: entry.report.insert_failures,
      emphasise: entry.report.insert_failures > 0 ? 'warn' : 'normal',
      hint: 'Per-Assertion insert errors (warn-logged in-band). > 0 is the verdict-negative case.',
    },
  ]);

  // ---- Session 88 — record-id → record-type colour coding ----------
  //
  // The per-pass `promoted_record_ids` (Session 87) carry no type tag
  // on the wire — they're opaque UUIDs. Resolving the type for each
  // id with one batched IPC call lets the chip strip render each id
  // with a small `event` / `observation` / `entity` chip beside it,
  // so the operator can scan a long ids list and pick out "the
  // Observation rows are these three" at a glance.
  //
  // The batch is shape-bounded: pre-Sn-87 history rows have no ids
  // (the `{#if … > 0}` upstream gates the section), and one
  // PromoteReport ships at most a few dozen ids in practice. The
  // server-side cap is 500.
  //
  // Resolution is best-effort: ids that resolve nowhere (deleted /
  // never persisted) simply don't appear in the returned map; we
  // render an `unknown` chip in that case. This matches the rest of
  // the dashboard's "missing data is dim, not fatal" posture.
  let recordTypes = $state<Record<string, RecordTypeTag> | null>(null);
  let recordTypesError = $state<string | null>(null);
  // Closed-vocab → short chip label. The four-character upper limit
  // keeps the chip narrow so it doesn't push the id text below the
  // 30vh scroll cap.
  const RECORD_TYPE_CHIP: Record<RecordTypeTag, string> = {
    observation: 'obs',
    event: 'ev',
    entity: 'ent',
    relation: 'rel',
    document: 'doc',
    assertion: 'asrt',
  };
  function chipLabelFor(id: string): { label: string; kind: RecordTypeTag | 'unknown' } {
    if (!recordTypes) return { label: '…', kind: 'unknown' };
    const kind = recordTypes[id];
    if (!kind) return { label: '?', kind: 'unknown' };
    return { label: RECORD_TYPE_CHIP[kind], kind };
  }

  // ---- Close-on-Escape --------------------------------------------

  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);

    // Kick the batch lookup on mount. The drawer is per-pass; ids
    // don't change while the modal is open, so a single fetch is
    // sufficient. Errors are surfaced via `recordTypesError` and
    // render an unobtrusive "couldn't resolve types" line below the
    // ids list — the rest of the drawer continues to function.
    const ids = entry.report.promoted_record_ids ?? [];
    if (ids.length > 0) {
      recordTypesForIds(ids)
        .then((map) => {
          recordTypes = map;
        })
        .catch((err: unknown) => {
          recordTypesError =
            err && typeof err === 'object' && 'message' in err
              ? String((err as { message: unknown }).message)
              : 'lookup failed';
        });
    }

    return () => window.removeEventListener('keydown', handler);
  });

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onClose();
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-label="promote-pass detail"
  tabindex={-1}
  onclick={onBackdropClick}
>
  <article class="drawer">
    <header class="head">
      <div class="head-main">
        <span class="title">promote pass</span>
        <span class="trigger-chip" data-sign={triggerSign(entry.trigger)}>
          {formatTrigger(entry.trigger)}
        </span>
        <span class="verdict-chip" data-sign={verdictSign(entry)}>
          {verdictText(entry)}
        </span>
      </div>
      <button
        class="close"
        type="button"
        aria-label="close promote pass detail"
        onclick={onClose}
      >
        ×
      </button>
    </header>

    <div class="meta-row">
      <span class="meta-label">when</span>
      <span class="meta-value" title={atFormatted.iso}>
        {atFormatted.human}
      </span>
      <span class="meta-iso" title="ISO 8601 timestamp">{atFormatted.iso}</span>
    </div>

    <div class="meta-row">
      <span class="meta-label">plan id</span>
      <span class="meta-value plan-id" title={entry.plan_id}>{entry.plan_id}</span>
      <CopyButton value={entry.plan_id} />
    </div>

    <ul class="counters" aria-label="per-counter breakdown">
      {#each counters as c (c.label)}
        <li class="counter-row" data-emphasise={c.emphasise} title={c.hint}>
          <span class="counter-label">{c.label}</span>
          <span class="counter-value">{c.value.toLocaleString('en-US')}</span>
        </li>
      {/each}
    </ul>

    {#if entry.report.promoted_record_ids && entry.report.promoted_record_ids.length > 0}
      <section class="ids" aria-label="records produced by this pass">
        <h3 class="ids-title">
          records produced
          <span class="ids-count">
            {entry.report.promoted_record_ids.length}
          </span>
        </h3>
        <ul class="ids-list">
          {#each entry.report.promoted_record_ids as recordId (recordId)}
            {@const chip = chipLabelFor(recordId)}
            <li class="id-row">
              <span class="type-chip" data-kind={chip.kind} title={chip.kind}>
                {chip.label}
              </span>
              <span class="id-value" title={recordId}>{recordId}</span>
              <CopyButton value={recordId} />
            </li>
          {/each}
        </ul>
        {#if recordTypesError}
          <p class="ids-error" title={recordTypesError}>
            couldn't resolve record types — {recordTypesError}
          </p>
        {/if}
      </section>
    {/if}
  </article>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }

  .drawer {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(560px, 100%);
    max-height: 90vh;
    background: var(--bg-panel);
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    padding: 16px 20px 20px;
    overflow: hidden;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-main {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .title {
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-primary);
  }
  .trigger-chip,
  .verdict-chip {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 2px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
  }
  .trigger-chip[data-sign='info'] {
    color: var(--signal-info);
  }
  .verdict-chip[data-sign='positive'] {
    color: var(--signal-positive);
    background: rgba(91, 198, 133, 0.08);
    border-color: rgba(91, 198, 133, 0.25);
  }
  .verdict-chip[data-sign='info'] {
    color: var(--signal-info);
  }
  .verdict-chip[data-sign='negative'] {
    color: var(--signal-negative);
    background: rgba(220, 90, 90, 0.08);
    border-color: rgba(220, 90, 90, 0.3);
  }
  .verdict-chip[data-sign='muted'] {
    color: var(--fg-tertiary);
  }

  .close {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-panel-alt);
    color: var(--fg-secondary);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
    border-radius: 3px;
    transition:
      background var(--duration-ui) var(--ease),
      border-color var(--duration-ui) var(--ease),
      color var(--duration-ui) var(--ease);
  }
  .close:hover,
  .close:focus-visible {
    background: var(--bg-elevated, var(--bg-panel));
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  .meta-row {
    display: flex;
    align-items: baseline;
    gap: 10px;
    font-size: 11px;
    font-family: var(--font-mono);
    color: var(--fg-secondary);
  }
  .meta-label {
    color: var(--fg-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 9px;
    min-width: 60px;
  }
  .meta-value {
    color: var(--fg-primary);
  }
  .meta-iso {
    color: var(--fg-quaternary);
    font-size: 10px;
  }
  .plan-id {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1 1 auto;
  }

  .counters {
    margin: 0;
    padding: 6px 0 0 0;
    list-style: none;
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 4px 16px;
  }
  .counter-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    padding: 3px 6px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }
  .counter-row[data-emphasise='warn'] {
    border-color: rgba(220, 90, 90, 0.3);
    background: rgba(220, 90, 90, 0.06);
  }
  .counter-label {
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }
  .counter-value {
    color: var(--fg-primary);
  }
  .counter-row[data-emphasise='warn'] .counter-value {
    color: var(--signal-warning, var(--signal-negative));
  }

  .ids {
    margin-top: 6px;
    padding-top: 8px;
    border-top: 1px solid var(--border-subtle);
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
    overflow: hidden;
  }
  .ids-title {
    margin: 0;
    font-size: 9px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .ids-count {
    font-family: var(--font-mono);
    color: var(--fg-quaternary);
    font-variant-numeric: tabular-nums;
  }
  .ids-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    max-height: 30vh;
  }
  .id-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 6px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }
  .id-value {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
    min-width: 0;
  }
  /* Session 88 — record-type chip beside each id. Six closed-vocab
     colour treatments + an `unknown` fallback for ids that resolve
     nowhere (deleted / pre-Sn-88 history rows). Treatment matches
     the type-count strip's visual language on the dashboard so the
     operator's eye reads the same colour-to-kind mapping in both
     places. */
  .type-chip {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 1px 4px;
    border-radius: 2px;
    border: 1px solid var(--border-subtle);
    color: var(--fg-secondary);
    background: var(--bg-panel);
    min-width: 32px;
    text-align: center;
  }
  .type-chip[data-kind='observation'] {
    color: var(--signal-positive, var(--fg-primary));
    border-color: rgba(91, 198, 133, 0.35);
    background: rgba(91, 198, 133, 0.06);
  }
  .type-chip[data-kind='event'] {
    color: var(--signal-info, var(--fg-primary));
    border-color: rgba(120, 170, 220, 0.35);
    background: rgba(120, 170, 220, 0.06);
  }
  .type-chip[data-kind='entity'] {
    color: var(--fg-primary);
    border-color: var(--border-strong);
  }
  .type-chip[data-kind='relation'] {
    color: var(--signal-warn, var(--fg-primary));
    border-color: rgba(220, 175, 90, 0.35);
    background: rgba(220, 175, 90, 0.06);
  }
  .type-chip[data-kind='document'] {
    color: var(--fg-tertiary);
    border-style: dashed;
  }
  .type-chip[data-kind='assertion'] {
    color: var(--fg-quaternary);
    font-style: italic;
  }
  .type-chip[data-kind='unknown'] {
    color: var(--fg-quaternary);
    border-style: dotted;
  }
  .ids-error {
    margin: 6px 0 0;
    font-size: 10px;
    color: var(--fg-tertiary);
    font-style: italic;
  }
</style>
