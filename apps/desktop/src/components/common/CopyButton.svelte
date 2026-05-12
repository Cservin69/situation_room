<!--
  CopyButton — small affordance that copies a string to the
  clipboard on click and flashes a "copied" state for visual
  confirmation (Session 63).

  ## Why a component, not an inline `<button onclick={…}>`

  The dashboard surfaces source URLs in two places — `MetricCard`'s
  observations footer and `KindCard`'s non-Observation footer.
  Inlining the copy logic in both means duplicating the
  copy-then-flash state machine, the icon swap, the aria-label
  update, and the timer cleanup. A shared component keeps the
  affordance visually and behaviourally identical across the
  dashboard, and a future "copy citation" / "copy provenance string"
  affordance just sets a different `value`.

  ## Why `navigator.clipboard` directly, not a Tauri plugin

  Per ADR 0009 the frontend cannot shell out or take new capability
  surfaces. The web Clipboard API (`navigator.clipboard.writeText`)
  works inside the Tauri 2 webview on a user-click gesture without
  adding `tauri-plugin-clipboard-manager` — so the boundary stays
  literal: no new Rust command, no new permission, no new ADR
  amendment. The only surface gained is "renderer can place a string
  on the operator's clipboard on a click event", which is strictly
  narrower than open-in-browser would have been.

  ## Visual behaviour

  - Idle: copy icon, foreground colour `--fg-tertiary`.
  - Hover: foreground brightens to `--fg-secondary`.
  - Just-copied: icon swaps to a checkmark, colour to
    `--signal-positive`, aria-label updates to "URL copied". The
    state auto-clears after `COPIED_FLASH_MS` so the next
    operator-glance sees the copy affordance again.
  - Empty `value`: button is `disabled` so the eye doesn't read a
    hover affordance that can't take. The parent should usually not
    render the component at all when the URL is empty, but the
    disabled-state guard is cheap.

  ## Failure mode

  Clipboard writes can fail in unusual cases (permissions denied by
  the browser layer, user-gesture stale by the time the promise
  resolves). The catch branch silently swallows — the absence of the
  checkmark flash is the operator's signal that the click didn't
  land. No error banner; the failure is local to one click and
  retrying is cheap.
-->
<script lang="ts">
  interface Props {
    /** The string to put on the clipboard. Empty → button disabled. */
    value: string;
    /**
     * Accessible label for the idle state. Defaults to "Copy URL"
     * since the dashboard's primary use is copying source URLs;
     * future callers (citations, recipe ids, …) override.
     */
    label?: string;
    /**
     * Accessible label shown immediately after a successful copy,
     * mirrored in the visual tooltip. Override when `label` is also
     * customised; the default reads naturally for the URL case.
     */
    copiedLabel?: string;
  }
  let { value, label = 'Copy URL', copiedLabel = 'URL copied' }: Props = $props();

  /**
   * How long the checkmark flash stays before reverting to the copy
   * icon. 1.5s is long enough to register at a glance, short enough
   * that a second copy (e.g., a different card) immediately afterward
   * doesn't read as "still showing the previous click's state."
   */
  const COPIED_FLASH_MS = 1500;

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function onClick() {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      copied = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        copied = false;
        timer = null;
      }, COPIED_FLASH_MS);
    } catch {
      // Silent failure — see header docstring. The lack of a flash
      // is the operator-visible signal.
    }
  }
</script>

<button
  type="button"
  class="copy-btn"
  class:copied
  disabled={!value}
  aria-label={copied ? copiedLabel : label}
  title={copied ? copiedLabel : label}
  onclick={onClick}
>
  {#if copied}
    <!-- Checkmark glyph. 12×12 viewBox keeps it pixel-sharp at the
         button's 12px slot regardless of OS DPR. -->
    <svg
      class="icon"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <polyline points="2.5 6.5 5 9 9.5 3.5" />
    </svg>
  {:else}
    <!-- Two-overlapping-rectangles copy glyph — the conventional
         "copy" icon across editors and chat surfaces (matches the
         user's mental model from code-cards in Claude). -->
    <svg
      class="icon"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <rect x="4" y="4" width="6.5" height="6.5" rx="1" />
      <path d="M2 8H1.6A0.6 0.6 0 0 1 1 7.4V1.6A0.6 0.6 0 0 1 1.6 1H7.4A0.6 0.6 0 0 1 8 1.6V2" />
    </svg>
  {/if}
</button>

<style>
  .copy-btn {
    appearance: none;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 2px;
    padding: 2px;
    color: var(--fg-tertiary);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: color var(--duration-ui) var(--ease),
                background var(--duration-ui) var(--ease),
                border-color var(--duration-ui) var(--ease);
    flex: 0 0 auto;
  }
  .copy-btn:hover:not(:disabled) {
    color: var(--fg-secondary);
    background: var(--bg-panel-alt);
    border-color: var(--border-subtle);
  }
  .copy-btn:focus-visible {
    outline: 1px solid var(--border-accent);
    outline-offset: 0;
  }
  .copy-btn:disabled {
    cursor: default;
    opacity: 0.3;
  }
  .copy-btn.copied {
    color: var(--signal-positive);
    /* Hold the visible state at full opacity even when the parent's
       hover-reveal would otherwise fade it out — the operator needs
       to see the confirmation even if they've already moved the
       mouse off the card. */
    opacity: 1 !important;
  }
  .icon {
    width: 12px;
    height: 12px;
    display: block;
  }
</style>
