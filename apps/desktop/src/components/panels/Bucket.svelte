<!--
  Bucket — a single record-type panel in the plan-review screen.

  One of six (Observation, Event, Entity, Relation, Document, Assertion).
  Each bucket shows:
    - the bucket name and an expectations count
    - a list of expectation rows (one per metric / event type / entity
      kind / relation kind / source nomination)
    - rationales rendered inline at small size
    - the records produced for the plan that fall into this bucket
      (Session 22), rendered as record cards under the expectations

  Children compose via a snippet so the parent decides how each row
  renders (a metric row is different from an entity-kind row, and a
  record card is different again).

  ## Empty-state logic

  The "(no expectations for this type — by design)" line shows only
  when *both* expectations and records are empty for this bucket. If
  records exist without expectations (a recipe produced records the
  plan didn't anticipate), the bucket still renders so the operator
  can see what came in. The "by design" copy would be misleading in
  that case — the records contradict the assertion.

  When expectations exist but records don't (or haven't loaded yet),
  the parent renders the expectations rows and a separate "0 records
  yet" hint inline; that hint is not Bucket's responsibility because
  whether it should appear depends on the records-loaded sentinel
  (`plans.records !== null`), which only the parent has visibility
  into.

  ## Body height cap (Session 51)

  The body is height-capped with internal vertical scroll so a tall
  bucket (e.g. a Document panel with seven nominations and long
  descriptions, or any bucket once its records section is populated)
  cannot stretch the row of the six-bucket CSS grid and visually
  overflow into the panels below (RecipeOutcomesHeatmap, Sources
  Memory, etc.). The cap sits on the bucket body — not on the bucket
  itself — so the header (title + count) stays in view as the body
  scrolls. The cap applies uniformly to all six bucket types
  (closed-vocabulary discipline: no source-specific routing).
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  interface Props {
    title: string;
    /**
     * Number of expectations of this type from the plan. Drives
     * the count display in the header.
     */
    count: number;
    /**
     * Number of records of this type produced by the plan's recipes
     * (Session 22). Used for the empty-state decision: the "no
     * expectations by design" copy only appears when both this and
     * `count` are zero. Defaults to 0 for callers that haven't
     * loaded records yet (legacy + pending-plan paths).
     */
    recordsCount?: number;
    /** Snippet that renders the rows. Receives no arguments. */
    children?: Snippet;
  }
  let { title, count, recordsCount = 0, children }: Props = $props();
</script>

<section class="bucket">
  <header>
    <span class="title">{title}</span>
    <span class="count">{count}</span>
  </header>
  <div class="body">
    {#if count === 0 && recordsCount === 0}
      <p class="empty">(no expectations for this type — by design)</p>
    {:else if children}
      {@render children()}
    {/if}
  </div>
</section>

<style>
  .bucket {
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-secondary);
    border-bottom: 1px solid var(--border-subtle);
    padding-bottom: 6px;
  }
  .count {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-tertiary);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
    /*
      Session 51: cap the body height with internal scroll so a
      tall bucket (long expectations list, populated records
      section) does not stretch the six-bucket grid row and push
      sibling panels (RecipeOutcomesHeatmap, SourcesMemoryPanel)
      down the page. The clamp scales with viewport so wide
      monitors get more density without losing the cap on small
      windows.

      `scrollbar-gutter: stable` reserves space for the scrollbar
      so a tall bucket's content doesn't shift horizontally
      relative to sibling buckets that fit without scrolling.
      `scrollbar-width: thin` + `scrollbar-color` styles the
      Firefox scrollbar against the existing theme tokens; the
      `::-webkit-scrollbar` rules below do the same for
      Chromium/WebKit. No hex literals; design-token-only.
    */
    max-height: clamp(180px, 36vh, 420px);
    overflow-y: auto;
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--border-subtle) transparent;
  }
  .body::-webkit-scrollbar {
    width: 6px;
  }
  .body::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: 3px;
  }
  .body::-webkit-scrollbar-track {
    background: transparent;
  }
  .empty {
    margin: 0;
    color: var(--fg-quaternary);
    font-style: italic;
    font-size: 11px;
  }
</style>
