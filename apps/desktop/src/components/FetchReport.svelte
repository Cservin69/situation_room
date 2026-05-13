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

  Session 30 — flag-from-decline (ADR 0013, Session 28/29 follow-up)
  ------------------------------------------------------------------

  Declined outcomes were the only failure shape the operator could
  *see* but couldn't *flag*: failed-apply outcomes had a recipe row in
  RecipesPanel where the existing flag button lived, but declines
  produce no recipe (Track B, ADR 0007 amendment 4) so there was no
  surface to attach feedback. That made the recipe-feedback channel
  invisible for exactly the case where it matters most — the LLM said
  "I cannot," the operator wants to say "actually you can, here's how,"
  and the next authoring run sees the note via `{{RECIPE_FEEDBACK}}`.

  This component now mounts the same `RecipeFlagDialog` used by
  RecipesPanel (single source of truth — both panels write through
  `flagRecipe` in the runes store, both read from
  `plans.recipeFeedback`). For a declined outcome row we render either:

    - a `flag` button (fresh: no note exists for this source/plan), or
    - a `FLAGGED` indicator chip (a note already exists; clicking
      reopens the dialog to edit, an empty submit clears).

  Both paths route into the same `openFlagDialog(source_id)` helper.
  `authoredFrom` is fixed at `'unknown'` for declines because no recipe
  was ever authored — the dialog's stub-hint banner (ADR 0014) is only
  meaningful when a stub-authored recipe exists, which this case
  doesn't.

  Two dialogs in the same app are fine: `position: fixed; z-index: 100`
  on `.backdrop` is set in RecipeFlagDialog itself, and only one
  instance is ever non-null at a time per panel — RecipesPanel and
  FetchReport each track their own `flagDialogSourceId`. A user could
  in principle open one, then open the other before submitting; the
  modals would stack visually but each would write through the same
  store helper, so the worst case is a confusing UX, not a state bug.
  Lifting the dialog state into the runes store would be the cleaner
  next-step refactor when a third panel needs it; the local pattern
  matches RecipesPanel's existing shape and keeps this patch surgical.

  Session 60 — re-author-from-failure (A direction pick)
  -----------------------------------------------------

  Pre-Session-60 the operator's path from "I see a failed outcome in
  the FetchReport" to "let me re-author this recipe" was: notice the
  failure here, navigate to RecipesPanel, manually scan for the
  matching recipe row, click *its* re-author button, re-type the
  failure context into the dialog's note textarea. Four-step,
  context-losing.

  Session 60 collapses that to one click. Each `failed` outcome row
  now carries a `re-author` button that opens the same ReauthorDialog
  RecipesPanel uses, pre-filled with:

    - the prior recipe's id (from the outcome's `recipe_id`),
    - the source id (from the outcome's `source_id`),
    - the verbatim failure message (from the outcome's `message`),
    - the latest captured bytes excerpt for that recipe (loaded via
      `latestAttemptForRecipe`).

  Both panels write through the same `reauthorRecipe` runes-store
  helper, so the lineage chip in RecipesPanel updates without a
  separate refresh roundtrip — same pattern as the flag-from-decline
  affordance (Session 30, ADR 0013).

  The dialog is mounted at the bottom of this component alongside
  the `RecipeFlagDialog` mount. Two open dialogs from one component
  is fine: each tracks its own state (`flagDialogSourceId` vs.
  `reauthorOutcome`) and the dialogs themselves are backdrop-modal
  with their own z-index, so even if both were somehow opened at
  once the worst case is a confusing UX, not a state bug. (Same
  containment argument as Session 30.)

  Why this isn't on `declined` outcomes too: a decline carries no
  recipe — there's nothing to re-author. The flag-button affordance
  is the right surface for declines; the operator's note flows into
  the next *initial* authoring attempt for the source via the
  `{{RECIPE_FEEDBACK}}` channel (ADR 0013), not via re-authoring.

  Session 50 — running-now scaffold
  ---------------------------------

  Pre-Session-50 the panel rendered nothing during a run: the user
  hit "run", `plans.fetching` flipped true, the run-fetch IPC call
  ran for 5–15 minutes, and only when it returned did a populated
  report show up. That was 20+ sessions of staring at a void
  (operator's words). Session 50 adds a running-now scaffold that
  pre-populates the nomination list from `plans.selected.expectations.
  document_sources` the moment the run starts. Each row reads
  "queued" because the existing `run_fetch_for_plan` IPC command is
  synchronous and returns one big `FetchReportDto` at the end; we
  don't yet have a per-nomination event stream to flip rows from
  "queued" to "fetching" to "authoring" to "outcome" live.

  The deferred Right path — a Tauri event channel
  (`fetch_run_progress`) emitting per-nomination stage transitions
  the executor already has internal state for — is documented in
  the Session 50 patch doc as the Session 51+ thread. The right
  shape requires a new IPC surface, a new DTO that mirrors the
  executor's stage taxonomy without becoming a parallel state
  surface that drifts from `RecipeOutcome`, and channel-lifecycle
  decisions (backpressure, disconnect, replay-on-reconnect). Worth
  its own session.
-->
<script lang="ts">
  import { plans, flagRecipe, reauthorRecipe } from '$stores/plans.svelte';
  import type { FetchRunSummaryDto } from '$lib/api/types/FetchRunSummaryDto';
  import type { PriorityTierDto } from '$lib/api/types/PriorityTierDto';
  import type { RecipeFetchAttemptDto } from '$lib/api/types/RecipeFetchAttemptDto';
  import type { RecipeOutcomeDto } from '$lib/api/types/RecipeOutcomeDto';
  import { outcomeTone, outcomeLabel, outcomeDetail, outcomeKey } from '$lib/outcomes';
  import { latestAttemptForRecipe, asCommandError } from '$lib/api/client';
  import RecipeFlagDialog from '$components/dialogs/RecipeFlagDialog.svelte';
  import ReauthorDialog from '$components/dialogs/ReauthorDialog.svelte';

  // Session 60 — re-author-from-failure (A direction pick).
  //
  // Narrow alias for the `failed` arm of RecipeOutcomeDto so the
  // dialog's source state has a single concrete type and TypeScript
  // narrows correctly across the two read sites (open handler +
  // dialog render block). Using the discriminated union's narrowing
  // here keeps RecipeOutcomeDto as the wire DTO and avoids leaking
  // a parallel "failed-outcome shape" into the rest of the file.
  type FailedOutcome = Extract<RecipeOutcomeDto, { kind: 'failed' }>;

  // Session 50: a tight label for the running-now nomination list's
  // tier badge. The full PriorityTierDto strings are too long for
  // the inline column slot the running-list reserves; a 4-char abbr
  // matches the existing badge convention in PlanReview's source-
  // priority chips.
  function tierShortLabel(t: PriorityTierDto): string {
    switch (t) {
      case 'authoritative_primary': return 'P1';
      case 'authoritative_secondary': return 'P2';
      case 'industry_trade_press': return 'TP';
      case 'general_news': return 'GN';
      default: return '??';
    }
  }

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

  // Session 30 — flag-from-decline (ADR 0013). Local state mirrors
  // the pattern in RecipesPanel: one source-id at a time, a transient
  // submitting flag, fresh-mount-per-open. The store's `flagRecipe`
  // helper does the actual write and reflects into
  // `plans.recipeFeedback` so the FLAGGED indicator updates without
  // a separate refresh roundtrip.
  let flagDialogSourceId: string | null = $state(null);
  let flagSubmitting = $state(false);

  function openFlagDialog(sourceId: string) {
    flagDialogSourceId = sourceId;
  }
  function closeFlagDialog() {
    flagDialogSourceId = null;
  }
  async function onFlagSubmit(note: string) {
    if (!flagDialogSourceId) return;
    flagSubmitting = true;
    try {
      const ok = await flagRecipe(flagDialogSourceId, note);
      if (ok) flagDialogSourceId = null;
      // On failure, leave the dialog open. The store sets
      // plans.error which the parent surfaces; the user can edit
      // the note and resubmit, or cancel.
    } finally {
      flagSubmitting = false;
    }
  }

  // Session 60 — re-author-from-failure (A direction pick).
  //
  // State for the ReauthorDialog mounted on `failed` outcome rows.
  // Mirrors RecipesPanel's pattern (which keys by `RecipeDto`); here
  // the FetchReport surface only has the outcome (no full recipe row
  // in scope) so we key by the failed outcome directly. The dialog
  // needs source_id + recipe_id (both on the outcome) + the failure
  // message (also on the outcome) + the bytes excerpt (loaded async).
  //
  // `reauthorOutcome === null` ⇒ no dialog open. Setting it to a
  // FailedOutcome opens the dialog and kicks off the bytes load; the
  // dialog itself tolerates `bytesExcerpt: ''` while the load is in
  // flight (ReauthorDialog's `bytesEmpty` branch renders an honest
  // "no bytes captured" placeholder so the operator isn't staring at
  // a blank panel).
  let reauthorOutcome: FailedOutcome | null = $state(null);
  let reauthorAttempt: RecipeFetchAttemptDto | null = $state(null);
  let reauthorLoadingAttempt = $state(false);
  let reauthorSubmitting = $state(false);

  async function openReauthorDialog(o: FailedOutcome) {
    reauthorOutcome = o;
    reauthorAttempt = null;
    reauthorLoadingAttempt = true;
    try {
      reauthorAttempt = await latestAttemptForRecipe(o.recipe_id);
    } catch (e) {
      // The capture-load is non-fatal — the dialog opens with empty
      // bytes and the failure-message panel from the outcome above.
      // Surface the error in the global banner so the operator sees
      // it but can still proceed with the failure-message-only
      // re-author. Same fallback as RecipesPanel's wiring.
      plans.error = asCommandError(e);
    } finally {
      reauthorLoadingAttempt = false;
    }
  }

  function closeReauthorDialog() {
    reauthorOutcome = null;
    reauthorAttempt = null;
    reauthorLoadingAttempt = false;
  }

  async function onReauthorSubmit(note: string | null) {
    if (!reauthorOutcome) return;
    reauthorSubmitting = true;
    try {
      // Session 68 follow-up: pass the outcome's failure message as
      // the override so fetch-stage failures (status 4xx/5xx,
      // timeouts) — which the executor doesn't capture into
      // `recipe_fetch_attempts` — can still re-author. Apply-stage
      // failures ignore the override (the captured row remains
      // authoritative on the backend).
      const outcome = await reauthorRecipe(
        reauthorOutcome.recipe_id,
        note,
        reauthorOutcome.message,
      );
      // Session 66: three-state outcome. `ok` and `declined` both
      // close the dialog (the IPC resolved cleanly in either case);
      // only a real `error` keeps the dialog open. The decline
      // reason is already in `plans.recipeReauthorDeclines` by the
      // time we get here — the failed-apply row's per-row badge
      // picks it up on the next render.
      if (outcome.kind === 'ok' || outcome.kind === 'declined') {
        reauthorOutcome = null;
        reauthorAttempt = null;
      }
      // On real `error`: dialog stays open, plans.error surfaces in
      // the banner, the operator can edit + resubmit. Same contract
      // as the flag-submit handler above and as RecipesPanel's.
    } finally {
      reauthorSubmitting = false;
    }
  }
</script>

<section class="fetch-report">
  {#if plans.fetching && plans.selected}
    <!--
      Session 50 — running-now scaffold. Pre-populates the nomination
      list from the selected plan's `document_sources` the instant
      `plans.fetching` flips true, so the operator sees what the
      pipeline is iterating instead of a blank panel.

      Each row reads "queued" because the existing
      `run_fetch_for_plan` IPC command returns one bundled report at
      the end of the run. Per-nomination stage transitions — what
      would let rows flip from "queued" to "fetching" / "authoring"
      live — require a new event channel (Session 51 thread).

      The previous run's report (if any) keeps rendering below this
      block while a new run is in flight. That preserves the
      operator's at-a-glance "did this plan ever produce records"
      context during multi-run sessions.
    -->
    {@const docs = plans.selected.expectations.document_sources}
    <div class="running-now">
      <header class="head">
        <span class="label">running now</span>
        <span class="summary">
          <span class="kv">
            <span class="k">nominations</span>
            <span class="v">{docs.length}</span>
          </span>
          <span class="running-pulse" aria-hidden="true">working…</span>
        </span>
      </header>
      {#if docs.length === 0}
        <p class="running-empty">
          this plan declares no document_sources — the executor will
          return an empty report. (Re-classify if you expect document
          coverage.)
        </p>
      {:else}
        <ul class="running-list">
          {#each docs as entry, i (entry.kind === 'nomination' ? entry.nomination_id : `legacy-${i}`)}
            <li class="running-row">
              <span class="row-marker">queued</span>
              {#if entry.kind === 'nomination'}
                <span class="priority-tier" data-tier={entry.priority_tier}>
                  {tierShortLabel(entry.priority_tier)}
                </span>
                <span class="description" title={entry.description}>
                  {entry.description}
                </span>
              {:else}
                <span class="priority-tier" data-tier="legacy">LEG</span>
                <span class="description">
                  legacy entry — re-classify the plan to make this
                  nomination authorable
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <p class="running-explainer">
        Each nomination runs propose-URL (up to 3 attempts) followed
        by recipe authoring against any URL that prefetches. Per-row
        stage isn't surfaced live yet — Session 51 thread.
      </p>
    </div>
  {/if}

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
        Case 1 from the empty-state taxonomy above. After ADR 0015
        (Session 37) the most common cause of this state is a plan
        whose document_sources is empty (the classifier produced
        zero nominations). Pre-ADR-0015 plans land in case 1's
        sibling — outcomes populated with `legacy_plan_cannot_author`
        rather than empty — handled below.
      -->
      <div class="empty empty-no-bindings">
        <p class="empty-headline">No recipes were attempted.</p>
        <p class="empty-explainer">
          The plan's document_sources didn't surface any nominations
          for the executor to author against. Re-classify the topic
          if you expect this plan to populate documents — the
          classifier emits source URLs from its training-distribution
          knowledge of authoritative sources for the topic.
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
            {:else if o.kind === 'legacy_plan_cannot_author'}
              <!--
                ADR 0015 / Session 37: pre-Session-37 plans surface
                one outcome per `preferred_source_id` they previously
                carried; no recipe exists, no recipe_id. Same
                blank-column-with-marker convention as `decl·` so
                the row stays scannable.
              -->
              <span class="recipe-id decl-marker">leg·</span>
            {:else}
              <span class="recipe-id">{shortId(o.recipe_id)}</span>
            {/if}
            <span class="source-id">{o.source_id}</span>
            <span class="status">{outcomeLabel(o)}</span>
            {#if outcomeDetail(o)}
              <span class="detail">{outcomeDetail(o)}</span>
            {/if}
            {#if o.kind === 'failed'}
              {@const declineReason = plans.recipeReauthorDeclines[o.recipe_id]}
              <!--
                Session 60 — re-author-from-failure (A direction pick).
                Failed outcomes carry recipe_id + source_id + the
                verbatim failure message; together they're enough to
                open ReauthorDialog directly without the operator
                hopping to RecipesPanel first. The bytes excerpt
                loads async after the dialog opens (the dialog has
                its own empty-bytes placeholder so the open is not
                blocked on the load).

                Visual grammar mirrors the declined-row flag affordance
                below: same `.actions` row spanning grid-column 1/-1,
                right-aligned single button. The button itself uses
                `.reauthor-button` styling — a constructive (re-author
                is fixing the recipe) affordance with subordinate
                chrome to the primary failure-status text.

                Session 66 — when the LLM has *already declined* a
                re-author for this recipe (the prior attempt landed
                `CommandError::ReauthorDeclined`), surface that here
                as a `[declined: <reason>]` badge instead of the
                button. The reason is the LLM's prose verbatim;
                renders inline so the operator sees what the LLM said
                without re-opening the dialog. Clicking re-author
                again would just produce the same decline; the badge
                replacing the button is the honest signal.
              -->
              {#if declineReason}
                <span
                  class="decline-badge"
                  title={`LLM declined to re-author: ${declineReason}`}
                >
                  declined: {declineReason}
                </span>
              {:else}
                <span class="actions">
                  <button
                    type="button"
                    class="reauthor-button"
                    title="Open the re-author dialog pre-filled with this failure's recipe, message, and the bytes the runtime saw."
                    onclick={() => openReauthorDialog(o)}
                  >re-author</button>
                </span>
              {/if}
            {:else if o.kind === 'declined'}
              {@const feedback = plans.recipeFeedback[o.source_id]}
              <!--
                Session 30 (ADR 0013 follow-up) — flag-from-decline.
                Declined outcomes have no recipe row in RecipesPanel,
                so this is the only surface for attaching a feedback
                note. The note flows into `{{RECIPE_FEEDBACK}}` on
                the next authoring run for this (plan, source) pair
                — the operator's "I disagree the source is undoable;
                here's how to extract from it" lives here.

                We render exactly one of two affordances per row:
                  * `flag` button when no note exists yet,
                  * `FLAGGED` chip when a note already exists; the
                    chip's title attribute exposes the note's full
                    text on hover, mirroring the chip in
                    RecipesPanel for visual continuity.

                Both call `openFlagDialog(o.source_id)`; the dialog
                pre-fills with the existing note via the `initial`
                prop when one exists.

                The `@const feedback = ...` binding is for type-
                narrowing — accessing `plans.recipeFeedback[id]`
                twice would re-look-up via the proxy and TypeScript
                wouldn't carry the narrowed type across the two
                accesses. Same pattern RecipesPanel uses.
              -->
              <span class="actions">
                {#if feedback}
                  <button
                    type="button"
                    class="flagged-chip"
                    title={feedback.note}
                    onclick={() => openFlagDialog(o.source_id)}
                  >FLAGGED</button>
                {:else}
                  <button
                    type="button"
                    class="flag-button"
                    title="Tell the recipe author why this source admits a recipe — your note feeds into the next authoring attempt."
                    onclick={() => openFlagDialog(o.source_id)}
                  >flag</button>
                {/if}
              </span>
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

{#if flagDialogSourceId !== null}
  <!--
    Session 30 — flag dialog mount for the FetchReport panel. Same
    component as RecipesPanel uses; both panels write through the
    same `flagRecipe` runes-store helper, both read the same
    `plans.recipeFeedback` map, so the indicator stays in sync
    regardless of which panel opened the dialog.

    `authoredFrom` is hard-coded to `'unknown'` for declines: no
    recipe was authored, so the `'stub_excerpt'` hint banner (ADR
    0014) doesn't apply. The dialog's default-prop fallback of
    `'unknown'` would also work; we pass it explicitly so future
    contributors don't wonder if the omission was intentional.

    `initial` reads from the live store map so an open-then-close-
    then-reopen cycle picks up any update made meanwhile (which is
    not currently reachable, but the cost of the live read is zero
    and the failure mode of an eager-captured initial would be a
    silent stale value — not the kind of bug that surfaces in
    review).
  -->
  <RecipeFlagDialog
    sourceId={flagDialogSourceId}
    initial={plans.recipeFeedback[flagDialogSourceId]?.note ?? ''}
    authoredFrom={'unknown'}
    submitting={flagSubmitting}
    onSubmit={onFlagSubmit}
    onCancel={closeFlagDialog}
  />
{/if}

{#if reauthorOutcome !== null}
  <!--
    Session 60 — re-author-from-failure (A direction pick).

    Mounts the same ReauthorDialog RecipesPanel uses. Both panels
    write through the `reauthorRecipe` store helper, so the new
    recipe's lineage chip (Session-31 reauthored-from) appears in
    RecipesPanel after submit without a separate refresh.

    `failureMessage` comes straight from the outcome (`o.message`)
    — apply-stage failures always carry one; the dialog's prose
    panel renders it verbatim above the bytes excerpt. The
    `bytesExcerpt` loads asynchronously after the dialog opens;
    while `reauthorAttempt` is `null` the dialog shows its honest
    "no bytes captured" placeholder rather than blocking the open.

    `submitting` ORs the two pending states (load + submit) so the
    primary button stays disabled while either is in flight. Same
    pattern RecipesPanel uses.
  -->
  <ReauthorDialog
    sourceId={reauthorOutcome.source_id}
    priorRecipeShortId={shortId(reauthorOutcome.recipe_id)}
    failureMessage={reauthorOutcome.message}
    bytesExcerpt={reauthorAttempt?.bytes_excerpt ?? ''}
    submitting={reauthorSubmitting || reauthorLoadingAttempt}
    onSubmit={onReauthorSubmit}
    onCancel={closeReauthorDialog}
  />
{/if}

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
  /* Session 30 — flag-from-decline (ADR 0013). The action row sits
     under .detail (or directly under the row-1 columns when there's
     no detail) and right-aligns the single flag affordance. The
     grid-column span keeps it independent of the three-column row-1
     layout; only the declined variant renders this row, so the
     other outcome shapes are unchanged.

     Sized matched to the chrome in RecipesPanel — same .flag-button
     and .flagged-chip rules below — so an operator who has used the
     RecipesPanel flag affordance recognizes the same control here. */
  .actions {
    grid-column: 1 / -1;
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    margin-top: 2px;
  }
  /* Mirrors RecipesPanel .flagged-chip (ADR 0013). Same sizing,
     hue, and hover behaviour so the indicator reads identically
     across both panels. */
  .flagged-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    border: 1px solid var(--signal-info, var(--border-subtle));
    background: var(--bg-canvas);
    cursor: pointer;
    align-self: center;
    transition: filter var(--duration-ui) var(--ease);
  }
  .flagged-chip:hover {
    filter: brightness(1.15);
  }
  /* Mirrors RecipesPanel .flag-button. Subordinate chrome — the
     declined status text is the primary affordance; the flag is
     the "add context" follow-on. */
  .flag-button {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 2px;
    color: var(--fg-tertiary);
    background: transparent;
    border: 1px solid var(--border-subtle);
    cursor: pointer;
    align-self: center;
    transition: border-color var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .flag-button:hover {
    border-color: var(--signal-info, var(--border-accent));
    color: var(--fg-primary);
  }

  /* Session 60 — re-author button on failed-apply outcome rows.
     Visually distinct from `.flag-button` so the operator scanning
     the outcomes list doesn't confuse the two affordances: re-author
     uses the same `--signal-info` accent as the dialog's primary
     button (constructive — "we're fixing the recipe") and a filled
     subtle background so it stands as a primary affordance within
     the row, while flag stays as a subordinate outline. Shape
     otherwise matches `.flag-button` for column-rhythm consistency. */
  .reauthor-button {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 10px;
    border-radius: 2px;
    color: var(--signal-info, var(--fg-secondary));
    background: var(--bg-canvas);
    border: 1px solid var(--signal-info, var(--border-subtle));
    cursor: pointer;
    align-self: center;
    transition: background var(--duration-ui) var(--ease),
                color var(--duration-ui) var(--ease);
  }
  .reauthor-button:hover {
    background: var(--signal-info, var(--bg-panel-alt));
    color: var(--fg-inverse, var(--fg-primary));
  }

  /* Session 66 — decline-badge replaces the re-author button on a
     failed-apply row once the LLM has declined to re-author it. Same
     row position as `.reauthor-button` but italicized + muted color
     so it reads as "informational, not actionable" — the operator
     should not click re-author again expecting a different answer
     from the same source-bytes-prompt triple. The title attribute
     carries the full reason verbatim for hover discovery; the
     visible text shows a truncated preview via single-line ellipsis. */
  .decline-badge {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: lowercase;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 2px;
    color: var(--fg-tertiary);
    background: var(--bg-inset, var(--bg-panel-alt));
    border: 1px dashed var(--border-subtle);
    align-self: center;
    max-width: 60ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: help;
    font-style: italic;
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

  /* Session 50 — running-now scaffold. Visually distinct from the
     final-report block so the operator sees at a glance that this
     panel reflects "what's being worked on right now" vs. "what came
     back from the most recent run". The dashed border-left mirrors
     the run-history strip's pending state for a consistent visual
     dialect of "in-flight". */
  .running-now {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 10px;
    border-left: 2px dashed var(--signal-info, var(--border-accent));
    background: var(--bg-panel-alt);
    border-radius: 2px;
  }
  .running-pulse {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-tertiary);
    letter-spacing: 0.06em;
    /* No CSS animation — keeping it static avoids motion noise for
       operators who prefer reduced motion, and a static label is
       enough to convey "this is working" alongside the dashed
       border which already signals in-flight status. */
  }
  .running-empty {
    margin: 0;
    color: var(--fg-tertiary);
    font-size: 11px;
    line-height: 1.5;
  }
  .running-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .running-row {
    display: grid;
    grid-template-columns: 60px 32px 1fr;
    column-gap: 10px;
    align-items: baseline;
    padding: 3px 6px;
    font-size: 12px;
    font-family: var(--font-mono);
  }
  .row-marker {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--fg-quaternary);
  }
  .priority-tier {
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: var(--fg-tertiary);
    text-align: center;
    padding: 1px 3px;
    border: 1px solid var(--border-subtle);
    border-radius: 2px;
    align-self: center;
  }
  .priority-tier[data-tier="authoritative_primary"]   { border-color: var(--signal-positive); }
  .priority-tier[data-tier="authoritative_secondary"] { border-color: var(--signal-info, var(--border-accent)); }
  .priority-tier[data-tier="industry_trade_press"]    { border-color: var(--fg-quaternary); }
  .priority-tier[data-tier="general_news"]            { border-color: var(--border-subtle); }
  .priority-tier[data-tier="legacy"]                  { border-color: var(--signal-warning); }
  .description {
    color: var(--fg-secondary);
    /* Long nomination descriptions are common; truncate-then-tooltip
       keeps the row scannable without losing the full text. */
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .running-explainer {
    margin: 0;
    color: var(--fg-quaternary);
    font-size: 10px;
    line-height: 1.5;
    font-style: italic;
  }
</style>
