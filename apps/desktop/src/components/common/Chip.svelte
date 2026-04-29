<!--
  Chip — a small inline label.

  Used for:
    - topic tags in the plan-review header
    - geo-scope labels in the geographic-scope strip
    - preferred-source-id badges in the document-sources panel

  Visual treatment per ADR 0006: chrome stays in the charcoal range —
  panel-alt background with subtle border. No color unless `tone` is
  explicitly set (and the chip is then a categorical signal, not chrome).
-->
<script lang="ts">
  interface Props {
    label: string;
    /** Secondary text shown after the label in tertiary color (e.g. usage count). */
    aside?: string;
    /** Categorical color, used sparingly. Default is chrome. */
    tone?: 'default' | 'positive' | 'warning' | 'info';
    /** When the chip carries a machine code separate from its display label
        (e.g. GeoScope), the title attribute carries it for hover-discovery. */
    title?: string;
  }
  let { label, aside = '', tone = 'default', title = '' }: Props = $props();
</script>

<span class="chip tone-{tone}" {title}>
  <span class="label">{label}</span>
  {#if aside}<span class="aside">{aside}</span>{/if}
</span>

<style>
  .chip {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    padding: 2px 8px;
    background: var(--bg-panel-alt);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    font-size: 11px;
    color: var(--fg-secondary);
    white-space: nowrap;
  }
  .label {
    font-family: var(--font-mono);
    color: var(--fg-primary);
  }
  .aside {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
  }
  /* Tones — used for categorical signal, not chrome. ADR 0006. */
  .tone-positive { border-color: var(--signal-positive); }
  .tone-positive .label { color: var(--signal-positive); }
  .tone-warning  { border-color: var(--signal-warning); }
  .tone-warning  .label { color: var(--signal-warning); }
  .tone-info     { border-color: var(--signal-info); }
  .tone-info     .label { color: var(--signal-info); }
</style>
