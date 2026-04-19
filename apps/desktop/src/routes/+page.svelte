<script lang="ts">
  // Phase 1 placeholder route — the real screen lands in Phase 4.
</script>

<main>
  <header>
    <h1>Stockpile</h1>
    <span class="subtitle">Phase 1 — workspace structure</span>
  </header>
  <p>
    The Rust workspace builds. The schema, sources, LLM router, pipeline,
    analytics, and API crates are scaffolded. The design system is documented
    in <code>src/lib/design/tokens.ts</code> and <code>global.css</code>.
  </p>
  <p>
    Run <code>cargo run --bin stockpile</code> to verify the workspace boots.
    The actual screen — six-panel commodity workstation, research command bar,
    anomaly indicators — lands in Phase 4.
  </p>
</main>

<style>
  main {
    max-width: 720px;
    margin: 80px auto;
    padding: 0 24px;
  }
  header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 24px; }
  h1 { font-size: 24px; font-weight: 600; margin: 0; letter-spacing: -0.01em; }
  .subtitle { color: var(--fg-tertiary); font-size: 12px; }
  p { color: var(--fg-secondary); line-height: 1.6; font-size: 13px; }
  code { font-family: var(--font-mono); color: var(--fg-primary); font-size: 12px; background: var(--bg-inset); padding: 1px 5px; border-radius: 2px; }
</style>
