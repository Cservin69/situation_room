<!--
  DocumentDrawer — full-body inspection modal for a Document record
  (Session 70).

  ## What this component answers

  The dashboard's KindCard shows a 120-char preview of a Document's
  body. Useful as "yes, this is what we fetched"; useless when the
  operator wants to read the actual article or inspect the raw JSON
  feed. The pre-Session-70 path was "open the SQLite store in a
  separate viewer" — a friction surface that hid Documents behind a
  third-party tool.

  This drawer makes Documents first-class on the dashboard: click any
  Document KindCard, modal opens, full body renders inline (pretty-
  printed when JSON-shaped, with the time-series chart inline at the
  top when one was detected).

  ## Why an in-app modal rather than a side-pane or new route

  - The dashboard is the operator's primary surface; opening a
    side-pane would shift the layout in a way that competes with
    other typed panels. A modal preserves the dashboard's reading
    order — close the drawer, you're back where you were.
  - A new SvelteKit route would put the Document URL into history
    and force the user through a back-button round-trip. The body
    payload is volatile (re-fetches accumulate) so deep-linking to
    a specific Document doesn't carry semantic weight today.

  ## What this component does NOT do

  - **No edits.** Documents are read-only on the dashboard, same
    posture as the rest of `RecordsDashboard`. Re-author / re-fetch
    affordances live on `RecipesPanel`.
  - **No diff against prior fetches.** Time-versioned page captures
    are stored without dedup; a future session can add "compare
    against last fetch" once the operator hits a case where it's
    load-bearing.
  - **No streaming for very large bodies.** Body cap is 32 KiB
    upstream (`document_synth::BODY_PREVIEW_CAP_BYTES`), so a single
    `<pre>` is bounded by construction.
-->
<script lang="ts">
  import type { DocumentDto } from '$lib/api/types/DocumentDto';
  import MiniSparkline from '$components/charts/MiniSparkline.svelte';
  import CopyButton from '$components/common/CopyButton.svelte';
  import { onMount } from 'svelte';

  interface Props {
    /** The Document record to inspect. Required; the parent only
     * renders this component when a Document has been selected. */
    document: DocumentDto;
    /**
     * Pre-computed time-series shape, if `RecordsDashboard`'s
     * detector found one. The dashboard already runs the detection
     * for the KindCard preview, so we pass the result through rather
     * than re-detecting. `null` = no chart shape, body renders as
     * pretty-printed text only.
     */
    chartSeries: {
      points: Array<{ x: number; y: number }>;
      label?: string;
      valueKey?: string;
    } | null;
    /** Close-the-drawer callback. Wired to backdrop click, Escape
     * key, and the explicit close button. */
    onClose: () => void;
  }
  let { document: doc, chartSeries, onClose }: Props = $props();

  // ---- Body rendering ---------------------------------------------

  /**
   * Format the body for inspection. JSON → pretty-printed with
   * 2-space indent. Everything else → as-is (HTML is already
   * tag-stripped upstream by `document_synth::body_preview` for
   * article-kind Documents).
   *
   * Returns `null` for empty bodies so the caller can render a
   * "(binary content — no inline preview)" hint rather than an
   * empty `<pre>`.
   */
  let prettyBody = $derived.by(() => {
    if (!doc.body || doc.body.length === 0) return null;
    const trimmed = doc.body.trim();
    if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
      try {
        const parsed = JSON.parse(trimmed);
        return JSON.stringify(parsed, null, 2);
      } catch {
        // JSON didn't parse — fall through to raw text. The body is
        // what was fetched; we don't try to "correct" malformed
        // payloads.
      }
    }
    return doc.body;
  });

  /** Short ISO date for the header. Same logic as RecordsDashboard's
   * `whenOf` but inline here so the drawer is self-contained. */
  let observedAt = $derived.by(() => {
    const env = doc.envelope;
    const raw = env.valid_at ?? env.observed_at;
    if (!raw) return '';
    const d = new Date(raw);
    if (Number.isNaN(d.valueOf())) return '';
    // Show the full ISO timestamp here (not just date) — the drawer
    // has the room, and "when was this fetched" is exactly the
    // question the operator opens the drawer to answer.
    return d.toISOString().replace('T', ' ').slice(0, 19) + ' UTC';
  });

  let sourceUrl = $derived(doc.envelope.provenance.source_url ?? '');

  // ---- Close-on-Escape --------------------------------------------

  // Mount-time keydown listener so Escape closes the drawer from any
  // focused element inside it. Cleaned up on unmount so we don't
  // accumulate handlers across open/close cycles.
  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });

  // Backdrop click handler — close only if the click was on the
  // backdrop itself, not on a child. This prevents accidental closes
  // when the operator clicks inside the modal panel.
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
  aria-label="document detail"
  tabindex={-1}
  onclick={onBackdropClick}
>
  <article class="drawer">
    <header class="head">
      <div class="head-main">
        <span class="kind">{doc.kind}</span>
        <span class="mime" title={doc.mime}>{doc.mime}</span>
        {#if observedAt}
          <span class="when" title="observed_at">{observedAt}</span>
        {/if}
      </div>
      <button
        class="close"
        type="button"
        aria-label="close document detail"
        onclick={onClose}
      >
        ×
      </button>
    </header>

    {#if sourceUrl}
      <div class="source-row">
        <a class="source-url" href={sourceUrl} target="_blank" rel="noopener noreferrer" title={sourceUrl}>
          {sourceUrl}
        </a>
        <CopyButton value={sourceUrl} />
      </div>
    {/if}

    {#if chartSeries && chartSeries.points.length > 0}
      <!-- Same MiniSparkline as the KindCard preview, but rendered
           at full-drawer width so the operator can read the trend
           shape instead of just glancing at it. -->
      <div class="chart-wrap">
        <div class="chart">
          <MiniSparkline points={chartSeries.points} />
        </div>
        <p class="chart-caption">
          {#if chartSeries.valueKey}{chartSeries.valueKey}{/if}
          {#if chartSeries.label} · {chartSeries.label}{/if}
          · {chartSeries.points.length} pt{chartSeries.points.length === 1 ? '' : 's'}
        </p>
      </div>
    {/if}

    <div class="body-wrap">
      {#if prettyBody === null}
        <p class="empty">
          (binary content — no inline preview available)
        </p>
      {:else}
        <pre class="body">{prettyBody}</pre>
      {/if}
    </div>
  </article>
</div>

<style>
  /* Backdrop covers the viewport and dims the dashboard behind. The
     opacity is tuned to keep the dashboard discernible so the
     operator remembers they're inspecting a Document, not navigated
     away from the plan. */
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
    gap: 12px;
    width: min(900px, 100%);
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
    padding-bottom: 10px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-main {
    display: flex;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .kind {
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-primary);
  }
  .mime {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
  }
  .when {
    font-family: var(--font-mono);
    font-size: 11px;
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
    transition: background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .close:hover,
  .close:focus-visible {
    background: var(--bg-elevated, var(--bg-panel));
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }

  .source-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .source-url {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
    text-decoration: none;
  }
  .source-url:hover,
  .source-url:focus-visible {
    color: var(--fg-primary);
    text-decoration: underline;
  }

  /* Chart sits between header and body so the operator can read the
     visual summary before scrolling the raw text. Same height as the
     KindCard chart × ~3 so it actually carries information at this
     scale. */
  .chart-wrap {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .chart {
    height: 140px;
    width: 100%;
  }
  .chart-caption {
    margin: 0;
    font-size: 11px;
    color: var(--fg-tertiary);
    text-transform: lowercase;
    letter-spacing: 0.02em;
  }

  /* Body wrap is the scrollable region. The drawer's max-height is
     90vh; the header / source-row / chart are fixed-height; the
     remaining space is the scrollable body. Vertical scroll within
     the body keeps the close button and metadata always visible. */
  .body-wrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
  }
  .body {
    margin: 0;
    padding: 8px 0;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-primary);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .empty {
    margin: 0;
    padding: 16px 0;
    font-size: 12px;
    font-style: italic;
    color: var(--fg-tertiary);
    text-align: center;
  }
</style>
