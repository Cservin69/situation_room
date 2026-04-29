<!--
  TopicInput — the classifier entry point. P3 in the handoff.

  Single text box, submit button, status line. While classification is
  in flight, the status line surfaces what's happening (calling xAI,
  validating, persisting); on failure the line surfaces the error.
  Per the handoff: 5–10 second classification, no theatrical spinner;
  the status line *is* the kinetic moment.
-->
<script lang="ts">
  import { plans, classifyTopic } from '$stores/plans.svelte';

  let topic = $state('');
  // Pure derivations; no extra reactive state needed.
  let canSubmit = $derived(topic.trim().length > 0 && !plans.classifying);
  let statusText = $derived.by(() => {
    if (plans.classifying) return 'classifying — calling xAI, validating, persisting…';
    if (plans.error?.kind === 'classification_failed') return `error: ${plans.error.message}`;
    if (plans.error?.kind === 'invalid_input') return `${plans.error.field}: ${plans.error.message}`;
    if (plans.error?.kind === 'storage') return `storage: ${plans.error.message}`;
    return '';
  });

  async function submit() {
    if (!canSubmit) return;
    const t = topic.trim();
    await classifyTopic(t);
    if (!plans.error) topic = '';
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }
</script>

<form class="bar" onsubmit={(e) => { e.preventDefault(); submit(); }}>
  <input
    type="text"
    placeholder="research a topic — e.g. lithium supply chain"
    bind:value={topic}
    onkeydown={onKeydown}
    disabled={plans.classifying}
    aria-label="research topic"
    spellcheck="false"
    autocomplete="off"
  />
  <button type="submit" disabled={!canSubmit}>
    {plans.classifying ? 'classifying…' : 'classify'}
  </button>
</form>
{#if statusText}
  <p class="status" class:error={!plans.classifying && plans.error}>{statusText}</p>
{/if}

<style>
  .bar {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  input {
    flex: 1 1 auto;
    background: var(--bg-inset);
    color: var(--fg-primary);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    padding: 8px 10px;
    font-family: var(--font-sans);
    font-size: 13px;
    outline: none;
    transition: border-color var(--duration-ui) var(--ease);
  }
  input:focus { border-color: var(--border-accent); }
  input:disabled { opacity: 0.6; }
  input::placeholder { color: var(--fg-quaternary); }

  button {
    flex: 0 0 auto;
    padding: 0 16px;
    background: var(--bg-panel-alt);
    color: var(--fg-primary);
    border: 1px solid var(--border-strong);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 12px;
    cursor: pointer;
    transition: background var(--duration-ui) var(--ease);
  }
  button:hover:not(:disabled) { background: var(--border-subtle); }
  button:disabled { opacity: 0.4; cursor: not-allowed; }

  .status {
    margin: 6px 0 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-tertiary);
  }
  .status.error { color: var(--signal-negative); }
</style>
