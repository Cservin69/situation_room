<!--
  NominationStatusGlyph — Session 52 piece A.

  Tiny glyph that renders the most-recent outcome of a nomination
  inline in the Document bucket's row aside, next to the priority-
  tier chip. The closed glyph vocabulary mirrors `nominationStatus`
  in `$lib/nominationOutcomes.ts`:

    ✓  authored     — succeeded: a recipe ran and produced records
    ✗  declined     — propose-URL or recipe-author exhausted attempts
    ⚠  failed       — recipe ran and broke at fetch / apply / insert
    ⌛  rate_limited — host returned 429
    ·  skipped      — executor declined to run the recipe
    !  legacy       — pre-Session-37 plan; re-classify to author
    ◌  idle         — no fetch run has touched this nomination yet

  ## Why aside-of-row, not below-the-row

  The Document bucket already lists nominations with a tier chip on
  the right. Surfacing the outcome status in the same aside means
  the operator's vertical scan reads description → glyph → tier in
  one line, without the row growing. The full reason text lives in
  the prior-attempts chronology (NominationAttempts) and on the
  glyph's `title=` for hover, so this column stays one character
  wide and high-density.

  ## Design-token discipline

  Colours come from the existing signal palette
  (`--signal-positive` / `--signal-negative` / `--signal-warning`)
  and the foreground tertiary / quaternary tokens for neutral
  states. No hex literals; no per-host routing.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import {
    latestRunForNomination,
    nominationStatus,
    type NominationStatus,
  } from '$lib/nominationOutcomes';

  interface Props {
    nominationId: string;
  }
  let { nominationId }: Props = $props();

  let status = $derived(nominationStatus(plans.outcomesHistory, nominationId));
  let latest = $derived(
    latestRunForNomination(plans.outcomesHistory, nominationId),
  );

  function glyphChar(s: NominationStatus): string {
    switch (s) {
      case 'authored':
        return '✓'; // ✓
      case 'declined':
        return '✗'; // ✗
      case 'failed':
        return '⚠'; // ⚠
      case 'rate_limited':
        return '⧖'; // ⧖ (hourglass-like)
      case 'skipped':
        return '·'; // ·
      case 'legacy':
        return '!';
      case 'idle':
      default:
        return '◌'; // ◌
    }
  }

  function statusLabel(s: NominationStatus): string {
    switch (s) {
      case 'authored':
        return 'authored';
      case 'declined':
        return 'declined';
      case 'failed':
        return 'failed';
      case 'rate_limited':
        return 'rate-limited';
      case 'skipped':
        return 'skipped';
      case 'legacy':
        return 'legacy plan';
      case 'idle':
      default:
        return 'no fetch run yet';
    }
  }

  function glyphTitle(): string {
    if (!latest) {
      return statusLabel(status);
    }
    const at = new Date(latest.attempted_at);
    const when = isNaN(at.getTime())
      ? latest.attempted_at
      : at.toLocaleString();
    const lines: string[] = [`${statusLabel(status)} · ${when}`];
    if (latest.records_produced !== null) {
      lines.push(`records: ${latest.records_produced}`);
    }
    if (latest.failure_stage) {
      lines.push(`stage: ${latest.failure_stage}`);
    }
    if (latest.retry_after_seconds !== null) {
      const secs = Number(latest.retry_after_seconds);
      lines.push(`retry-after: ${secs}s`);
    }
    if (latest.message) {
      lines.push(latest.message);
    }
    return lines.join('\n');
  }
</script>

<span
  class="glyph"
  data-status={status}
  title={glyphTitle()}
  aria-label={glyphTitle()}
>
  {glyphChar(status)}
</span>

<style>
  .glyph {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1;
    cursor: help;
    user-select: none;
  }
  .glyph[data-status='authored'] {
    color: var(--signal-positive);
  }
  .glyph[data-status='failed'] {
    color: var(--signal-negative);
  }
  .glyph[data-status='rate_limited'] {
    color: var(--signal-warning);
  }
  .glyph[data-status='declined'] {
    color: var(--fg-tertiary);
  }
  .glyph[data-status='legacy'] {
    color: var(--fg-tertiary);
  }
  .glyph[data-status='skipped'] {
    color: var(--fg-quaternary);
  }
  .glyph[data-status='idle'] {
    color: var(--fg-quaternary);
  }
</style>
