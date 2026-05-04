<!--
  FetchReport — renders the result of a single `runFetch` call.

  Three sections:

    - Summary line: "X/Y recipes succeeded, Z records produced."
    - Outcomes list: one row per recipe, with per-stage iconography
      for failures.
    - History strip (below): the `plans.fetchRuns` array — at-a-
      glance "we ran this plan N times before, here's how each
      went." Empty until at least one run has been completed.

  The component reads from the runes store directly; no props. Mounts
  as a child of PlanReview when there's a plan selected.

  Empty-state taxonomy (Session 13 P5)
  -------------------------------------

  Three legitimate "nothing useful happened" states the panel must
  distinguish, because they imply different user actions:

    1. recipes_attempted == 0, outcomes empty
       → No recipe was authored against any source. The plan's
         document_sources didn't bind to any registered descriptor
         in `config/sources.toml`. The fix is editorial — either add
         a source, or re-classify with a topic the registry covers.
         Surface this with a dedicated message that names the cause.
         (This was the "footgun" from the Session 12 hungary's-frozen-
         EU-funds run: 0 recipes attempted, no signal why.)

    2. recipes_attempted > 0, outcomes empty
       → Defensive. Shouldn't happen in practice (every attempted
         recipe lands in `outcomes`), but if invariants ever drift
         the panel should still say *something*. Falls into the
         generic "no outcomes" message below; not flagged specially.

    3. recipes_attempted > 0, outcomes populated
       → The normal case. Render the outcomes list as before.

  The two zero-attempt-empty cases above were collapsed into a single
  "no recipes" message before this session; they're separated now
  because case 1 is always actionable and case 2 is always a bug.

  Design-token discipline
  -----------------------

  Up to Session 12 this component used `var(--signal-ok, #5b9c5e)`
  and `var(--signal-error, #c83c3c)` — the named vars don't exist in
  global.css, so the hex fallbacks were what actually painted. That
  drifted from the design tokens (`--signal-positive`,
  `--signal-negative`) and embedded hex literals in component CSS,
  both of which the handoff hard rules forbid. This file now uses the
  canonical vars throughout. ADR 0006.
-->
<script lang="ts">
  import { plans } from '$stores/plans.svelte';
  import type { FetchRunSummaryDto } from '$lib/api/types/FetchRunSummaryDto';
  import { outcomeTone, outcomeLabel, outcomeDetail, outcomeKey } from '$lib/outcomes';

  function shortId(id: string): string {
    // UUIDv7s are too long for inline display; first 8 chars are
    // unique enough for a single plan's recipe list.
    return id.slice(0, 8);
  }

  function formatRunStarted(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    const hh = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    const ss = String(d.getSeconds()).padStart(2, '0');
    return `${hh}:${min}:${ss}`;
  }

  function runStatusTone(r: FetchRunSummaryDto): 'ok' | 'partial' | 'fail' | 'pending' {
    if (!r.finished_at) return 'pending';
    if (r.error_summary) return 'fail';
    if (r.recipes_succeeded === r.recipes_attempted && r.recipes_attempted > 0) return 'ok';
    if (r.recipes_succeeded > 0) return 'partial';
    return 'fail';
  }
</script>

<section class="fetch-report">
  {#if plans.fetchReport}
    {@const report = plans.fetchReport}
    <header class="head">
      <span class="label">last run</span>
      <span class="summary">
        <span class="kv"><span class="k">attempted</span><span class="v">{report.recipes_attempted}</span></span>
        <span class="kv"><span class="k">succeeded</span><span class="v">{report.recipes_succeeded}</span></span>
        <span class="kv"><span class="k">records</span><span class="v">{report.records_produced}</span></span>
      </span>
    </header>

    {#if report.error_summary}
      <p class="top-error">{report.error_summary}</p>
    {/if}

    {#if report.recipes_attempted === 0 && report.outcomes.length === 0}
      <!--
        Case 1 from the empty-state taxonomy above. Distinct from the
        generic "no outcomes" message: this happens when the plan's
        document_sources hints didn't bind to any registered source
        descriptor, so `load_or_author_recipes` had nothing to do.
        Adjacent to ADR 0007's deferred CoverageReport; the Session
        13 handoff §P5 chose to address it locally rather than wait
        for the broader coverage design.
      -->
      <div class="empty empty-no-bindings">
        <p class="empty-headline">No recipes were attempted.</p>
        <p class="empty-explainer">
          The plan's document sources didn't bind to any registered
          source in <code>config/sources.toml</code>. Either add a
          matching source descriptor, or re-classify the topic in
          terms the registry covers.
        </p>
      </div>
    {:else if report.outcomes.length === 0}
      <!--
        Case 2: defensive. Shouldn't happen in practice (the executor
        records every attempt), but if the invariant ever slips, we
        say something rather than render a blank panel.
      -->
      <p class="empty">no outcomes were recorded — this is unexpected; check the logs.</p>
    {:else}
      <ul class="outcomes">
        {#each report.outcomes as o (outcomeKey(o))}
          <li class="outcome" data-tone={outcomeTone(o)}>
            {#if o.kind === 'declined'}
              <!--
                Track B (Session 28, ADR 0007 amendment 4): a declined
                outcome carries no recipe_id (no recipe was created).
                The first column shows a literal `decl·` marker so
                the operator scan-reading the list sees the column
                slot is intentionally blank, not "the recipe id is
                missing." source-id and label sit in their normal
                columns; the LLM's verbatim reason flows into the
                detail row beneath.
              -->
              <span class="recipe-id decl-marker">decl·</span>
            {:else}
              <span class="recipe-id">{shortId(o.recipe_id)}</span>
            {/if}
            <span class="source-id">{o.source_id}</span>
            <span class="status">{outcomeLabel(o)}</span>
            {#if outcomeDetail(o)}
              <span class="detail">{outcomeDetail(o)}</span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if plans.fetchRuns.length > 0}
    <div class="history">
      <span class="label">history</span>
      <ul class="runs">
        {#each plans.fetchRuns as r (r.id)}
          <li class="run" data-tone={runStatusTone(r)}>
            <span class="time">{formatRunStarted(r.started_at)}</span>
            <span class="counts">
              {r.recipes_succeeded}/{r.recipes_attempted}
              <span class="dot">·</span>
              {r.records_produced}r
            </span>
            {#if r.error_summary}
              <span class="run-error" title={r.error_summary}>!</span>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</section>

<style>
  .fetch-report {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 12px;
    background: var(--bg-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-tertiary);
  }
  .summary {
    display: flex;
    gap: 14px;
    font-family: var(--font-mono);
    font-size: 11px;
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .k  { color: var(--fg-quaternary); text-transform: uppercase; letter-spacing: 0.06em; }
  .v  { color: var(--fg-primary); }

  .top-error {
    margin: 0;
    padding: 6px 8px;
    background: var(--bg-panel-alt);
    border-left: 2px solid var(--signal-negative);
    color: var(--fg-primary);
    font-size: 12px;
    font-family: var(--font-mono);
  }

  .empty {
    margin: 0;
    color: var(--fg-tertiary);
    font-size: 12px;
  }

  /* Case-1 empty state — has its own small block treatment because
     the message is two paragraphs and the second one references a
     filename. The border-left in warning amber distinguishes it
     visually from the generic "empty" grey. */
  .empty-no-bindings {
    padding: 8px 10px;
    background: var(--bg-panel-alt);
    border-left: 2px solid var(--signal-warning);
    border-radius: 2px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .empty-headline {
    margin: 0;
    color: var(--fg-primary);
    font-size: 12px;
    font-weight: 500;
  }
  .empty-explainer {
    margin: 0;
    color: var(--fg-secondary);
    font-size: 11px;
    line-height: 1.5;
  }
  .empty-explainer code {
    font-family: var(--font-mono);
    font-size: 11px;
    background: var(--bg-inset);
    padding: 1px 4px;
    border-radius: 2px;
    color: var(--fg-primary);
  }

  .outcomes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .outcome {
    display: grid;
    grid-template-columns: 80px 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 10px;
    row-gap: 2px;
    align-items: baseline;
    padding: 4px 6px;
    border-left: 2px solid transparent;
    font-size: 12px;
    font-family: var(--font-mono);
  }
  .outcome[data-tone="ok"]   { border-left-color: var(--signal-positive); }
  .outcome[data-tone="skip"] { border-left-color: var(--fg-quaternary); }
  .outcome[data-tone="fail"] {
    border-left-color: var(--signal-negative);
    background: var(--bg-panel-alt);
  }
  /* Track D, Session 25 — `rate_limited` outcomes get warning amber.
     The visual distinction from `fail` is load-bearing: red signals
     "the recipe is broken; re-running won't help"; amber signals
     "the source asked us to wait; re-running later is the
     remediation." Same border-treatment otherwise so the outcomes
     list reads as a uniform vertical scan. */
  .outcome[data-tone="limited"] {
    border-left-color: var(--signal-warning);
    background: var(--bg-panel-alt);
  }
  /* Track B (Session 28, ADR 0007 amendment 4) — `declined`
     outcomes get their own border treatment. The semantic
     distinction from `failed` (red) and `rate_limited` (amber) is
     load-bearing: a decline means no recipe was ever created. The
     remediation is editorial (drop the source, escalate the model
     tier, find an alternative) — re-running with no other change
     gets the same decline. We render in `--fg-tertiary` (the same
     dimmed neutral as `skip`) with the panel-alt background of
     `fail`/`limited` to mark it as "outcome that needs attention,
     but not red-alarm." Distinct from `skip` (no background) so
     the LLM-decided cases stand apart from executor-decided
     skips. */
  .outcome[data-tone="declined"] {
    border-left-color: var(--fg-tertiary);
    background: var(--bg-panel-alt);
  }
  /* Defensive: outcomeTone returns 'none' when the outcome is
     undefined, which can't happen for items inside the outcomes
     list (each list item is by construction a defined outcome).
     Style anyway so the type-checker is satisfied if tone ever
     widens. */
  .outcome[data-tone="none"] { border-left-color: var(--border-subtle); }

  .recipe-id { color: var(--fg-quaternary); }
  /* Track B (Session 28): declined rows have no recipe_id; we put
     a literal "decl·" marker in the slot so the column doesn't go
     blank (which would visually misalign the row) and so the
     operator sees that the absence is intentional rather than
     missing data. Italic + slightly dimmed to distinguish from a
     real id without grabbing focus. */
  .recipe-id.decl-marker {
    color: var(--fg-quaternary);
    font-style: italic;
  }
  .source-id { color: var(--fg-secondary); }
  .status    { color: var(--fg-primary); justify-self: end; }
  .detail {
    grid-column: 1 / -1;
    font-size: 11px;
    color: var(--fg-tertiary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .history {
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-top: 1px solid var(--border-subtle);
    padding-top: 8px;
  }
  .runs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .run {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    padding: 2px 6px;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-secondary);
  }
  .run[data-tone="ok"]      { border-color: var(--signal-positive); }
  .run[data-tone="partial"] { border-color: var(--signal-warning); }
  .run[data-tone="fail"]    { border-color: var(--signal-negative); }
  .run[data-tone="pending"] { border-style: dashed; }
  .time   { color: var(--fg-tertiary); }
  .counts { color: var(--fg-primary); }
  .dot    { color: var(--fg-quaternary); margin: 0 2px; }
  .run-error {
    color: var(--signal-negative);
    font-weight: 600;
    cursor: help;
  }
</style>
