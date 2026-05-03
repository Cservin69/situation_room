<!--
  RecordCard — one record line under a Bucket (Session 22).

  Renders a compact card with:
    - the per-type summary (formatted by `recordSummary.ts`)
    - a small "recipe" chip showing the recipe id parsed from
      `envelope.provenance.recipe_id` (truncated to first 8 chars,
      full id in the title attribute on hover) — empty-string
      `recipe_id` (legacy provenance) hides the chip
    - an expand caret that reveals the raw envelope and content as
      pretty-printed JSON for inspection

  ## Why JSON for the expanded view, instead of a typed renderer

  Each record type has different content shape and rendering needs.
  A typed renderer would mean six per-type sub-components — each
  consuming its own subset of the content fields. That work is
  appropriate for a session focused on records *rendering*, not for
  this session, which is focused on records *appearing at all*. The
  JSON view is honest about the content (what the database holds is
  what you see) and lets the operator verify their plan's recipes
  produced what was expected without us guessing which fields matter
  per type. A future pass can replace this with per-type renderers.

  ## Why the props are a string discriminator + union, not a tagged enum

  Svelte 5's `$props()` rune is destructured by every other component
  in this codebase. A discriminated-union props shape would force the
  non-destructured `let props = $props()` form, breaking idiom. The
  string `kind` discriminator + union `record` type, with explicit
  per-kind dispatch in `summary()`, gets the same runtime behaviour
  with codebase-consistent prop syntax.
-->
<script lang="ts">
  import type { ObservationDto } from '$lib/api/types/ObservationDto';
  import type { EventDto } from '$lib/api/types/EventDto';
  import type { EntityDto } from '$lib/api/types/EntityDto';
  import type { RelationDto } from '$lib/api/types/RelationDto';
  import type { DocumentDto } from '$lib/api/types/DocumentDto';
  import type { AssertionDto } from '$lib/api/types/AssertionDto';
  import {
    summarizeObservation,
    summarizeEvent,
    summarizeEntity,
    summarizeRelation,
    summarizeDocument,
    summarizeAssertion,
  } from '$lib/api/recordSummary';

  type RecordKind =
    | 'observation'
    | 'event'
    | 'entity'
    | 'relation'
    | 'document'
    | 'assertion';

  type AnyRecord =
    | ObservationDto
    | EventDto
    | EntityDto
    | RelationDto
    | DocumentDto
    | AssertionDto;

  interface Props {
    kind: RecordKind;
    record: AnyRecord;
  }
  let { kind, record }: Props = $props();
  let expanded = $state(false);

  /**
   * Per-kind dispatch to the right summarizer. The cast inside each
   * branch is safe because the parent (`PlanReview`) always pairs
   * `kind` with the matching DTO when invoking — the discriminator
   * is ours, set at the call site, not the wire's. If we ever pull
   * records into the card from a heterogeneous source (e.g. a
   * "search across all types" view), the parent there needs to set
   * `kind` correctly for each item.
   */
  function summary(k: RecordKind, r: AnyRecord): string {
    switch (k) {
      case 'observation':
        return summarizeObservation(r as ObservationDto);
      case 'event':
        return summarizeEvent(r as EventDto);
      case 'entity':
        return summarizeEntity(r as EntityDto);
      case 'relation':
        return summarizeRelation(r as RelationDto);
      case 'document':
        return summarizeDocument(r as DocumentDto);
      case 'assertion':
        return summarizeAssertion(r as AssertionDto);
    }
  }

  /**
   * Truncate a uuid to the first 8 hex chars for the chip display.
   * Matches the convention used in the recipes panel for visual
   * brevity. The full id stays in the chip's title attribute so
   * hovering surfaces it.
   */
  function shortId(uuid: string): string {
    if (!uuid) return '';
    const dash = uuid.indexOf('-');
    return dash > 0 ? uuid.slice(0, dash) : uuid.slice(0, 8);
  }

  // Derived values: read off `record` reactively so a parent that
  // mutates the props (uncommon but legal) re-renders.
  let envelope = $derived(record.envelope);
  let summaryLine = $derived(summary(kind, record));
  let recipeId = $derived(envelope.provenance.recipe_id || '');
  let recipeShort = $derived(shortId(recipeId));
  let prettyJson = $derived(JSON.stringify(record, null, 2));
</script>

<div class="card" class:expanded>
  <button class="head" type="button" onclick={() => (expanded = !expanded)} aria-expanded={expanded}>
    <span class="summary">{summaryLine}</span>
    {#if recipeId.length > 0}
      <span class="recipe-chip" title="recipe {recipeId}">recipe {recipeShort}</span>
    {/if}
    <span class="caret" aria-hidden="true">{expanded ? '−' : '+'}</span>
  </button>
  {#if expanded}
    <pre class="detail">{prettyJson}</pre>
  {/if}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 4px;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    background: var(--bg-canvas);
  }
  .card.expanded {
    background: var(--bg-panel-alt);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    padding: 5px 8px;
    background: transparent;
    border: 0;
    text-align: left;
    cursor: pointer;
    color: var(--fg-primary);
    font-family: var(--font-sans);
    font-size: 12px;
    border-radius: 2px;
    transition: background var(--duration-ui) var(--ease);
  }
  .head:hover {
    background: var(--bg-panel-alt);
  }
  .head:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .summary {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .recipe-chip {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    padding: 1px 4px;
    letter-spacing: 0.03em;
  }
  .caret {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    color: var(--fg-tertiary);
    width: 10px;
    text-align: center;
  }
  .detail {
    margin: 0;
    padding: 6px 8px 8px 8px;
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.45;
    color: var(--fg-secondary);
    white-space: pre-wrap;
    word-break: break-word;
    border-top: 1px solid var(--border-subtle);
  }
</style>
