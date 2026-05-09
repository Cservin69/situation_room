/**
 * Plans store — Svelte 5 runes-based reactive state.
 *
 * Exposes the application's view state as a single `$state` object that
 * components import and read from / mutate through the exported helpers.
 * Using runes (rather than `writable()` from `svelte/store`) keeps the
 * reactivity local to each consuming component without subscription
 * boilerplate.
 *
 * ## State shape
 *
 * - `recent`: list of `PlanSummary` rows shown in the listing pane.
 * - `selected`: full `ResearchPlanDto` for the currently-open plan, or
 *   `null` if nothing is selected.
 * - `statusFilter`: which lifecycle bucket the listing is showing.
 *   `'all'` collapses to `null` on the wire (the backend's "no
 *   filter"); the three concrete `PlanStatusDto` values pass through
 *   verbatim.
 * - `classifying`: true while a classify call is in flight. Drives the
 *   topic-input spinner.
 * - `loading`: true while listRecent / getPlan calls are in flight.
 *   Drives panel skeletons.
 * - `mutating`: true while accept/reject calls are in flight.
 *   Different from `loading` so the listing doesn't show "loading…"
 *   on every status change — the optimistic update has already done
 *   the visible work.
 * - `error`: the last surfaced error, if any. Cleared on next successful
 *   action.
 *
 * ## Why a single state object
 *
 * Multiple separate `$state` runes would force consumers to choose
 * which one to subscribe to. One object keeps reads cheap (Svelte's
 * fine-grained reactivity tracks per-property accesses inside the
 * proxy) and writes obvious.
 *
 * ## Why the filter persists in this store, not the URL
 *
 * The handoff (Session 7 §P1) explicitly chose runes-store persistence
 * over URL state. The single SPA route doesn't have a URL to share,
 * and the filter choice is per-window-session, not per-link. If
 * bookmarkable filtered listings ever land, they're additive.
 */
import {
  classify as apiClassify,
  listRecentPlans,
  getPlan,
  acceptPlan as apiAcceptPlan,
  rejectPlan as apiRejectPlan,
  reclassifyPlan as apiReclassifyPlan,
  runFetchForPlan as apiRunFetchForPlan,
  listFetchRuns as apiListFetchRuns,
  listRecipesForPlan as apiListRecipesForPlan,
  setRecipeFeedback as apiSetRecipeFeedback,
  listRecipeFeedbackForPlan as apiListRecipeFeedbackForPlan,
  reauthorRecipe as apiReauthorRecipe,
  recordsForPlan as apiRecordsForPlan,
  recipeOutcomesHistory as apiRecipeOutcomesHistory,
  expectationCoverage as apiExpectationCoverage,
  asCommandError,
} from '$lib/api/client';
import type { PlanSummary } from '$lib/api/types/PlanSummary';
import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
import type { PlanStatusDto } from '$lib/api/types/PlanStatusDto';
import type { CommandErrorDto } from '$lib/api/types/CommandErrorDto';
import type { FetchReportDto } from '$lib/api/types/FetchReportDto';
import type { FetchRunSummaryDto } from '$lib/api/types/FetchRunSummaryDto';
import type { RecipeDto } from '$lib/api/types/RecipeDto';
import type { RecipeFeedbackDto } from '$lib/api/types/RecipeFeedbackDto';
import type { RecordsByPlanDto } from '$lib/api/types/RecordsByPlanDto';
import type { RecipeOutcomesHistoryEntryDto } from '$lib/api/types/RecipeOutcomesHistoryEntryDto';
import type { ExpectationCoverageRowDto } from '$lib/api/types/ExpectationCoverageRowDto';

export type StatusFilter = PlanStatusDto | 'all';

interface PlansState {
  recent: PlanSummary[];
  selected: ResearchPlanDto | null;
  statusFilter: StatusFilter;
  classifying: boolean;
  loading: boolean;
  mutating: boolean;
  /**
   * True while a `run_fetch_for_plan` call is in flight. Drives the
   * RunFetchButton's spinner; kept distinct from `mutating` so that
   * a fetch run doesn't spuriously disable accept/reject buttons
   * (and vice versa).
   */
  fetching: boolean;
  /**
   * The most recent fetch report for the selected plan, or null if
   * the user hasn't run a fetch since opening the plan. Cleared on
   * `selectPlan` so a stale report from another plan doesn't bleed
   * across selections.
   */
  fetchReport: FetchReportDto | null;
  /**
   * Recent fetch-run summaries for the selected plan, newest first.
   * Refreshed alongside the plan selection and after each successful
   * `runFetch`. Empty until the first listing roundtrip lands.
   */
  fetchRuns: FetchRunSummaryDto[];
  /**
   * Recipes authored for the selected plan, newest first. Loaded
   * alongside the plan body and refreshed after each successful
   * `runFetch` (because the first run is what triggers Level-2
   * authoring; subsequent runs may add new recipes if the plan was
   * extended). Empty until the first listing roundtrip lands, or
   * if no recipes have been authored for the plan yet.
   *
   * Drives `RecipesPanel.svelte`. The `RecipeDto`'s `extraction` and
   * `produces` fields are typed as `unknown` on the wire — the
   * panel pretty-prints them as JSON.
   */
  recipes: RecipeDto[];
  /**
   * Operator-feedback notes attached to the selected plan, keyed by
   * `source_id`. ADR 0013. The recipe-inspection panel reads this
   * map to decide whether to render the indicator chip on each
   * recipe card; the flag dialog reads it to pre-fill an existing
   * note when the operator opens the dialog to edit.
   *
   * `Record<source_id, RecipeFeedbackDto>` rather than a Map because
   * Svelte 5 runes track plain-object property mutations through
   * proxies; Map mutations don't trigger reactivity without
   * `$state.raw` plus reassignment, and we want straightforward
   * `delete plans.recipeFeedback[id]` semantics on clear.
   */
  recipeFeedback: Record<string, RecipeFeedbackDto>;
  /**
   * Records produced by the selected plan's recipes, bucketed by
   * record type (Session 22). `null` means "we haven't asked yet"
   * (selection is fresh, or selection is a pending plan that can't
   * have records). Distinguishing `null` from an all-empty bucket
   * matters for the UI: a pending plan never shows "0 records yet"
   * empty-state copy because the question doesn't apply.
   *
   * Refreshed alongside the plan body and after each successful
   * `runFetch`. Cleared on `clearSelection` and at the start of
   * `selectPlan`.
   */
  records: RecordsByPlanDto | null;
  /**
   * Per-(recipe-or-source) outcome history across the plan's recent
   * fetch runs (Session 46). Drives the recipe-success heatmap.
   * Empty until the first roundtrip lands; refreshed alongside
   * `selectPlan` and after each successful `runFetch`. Cleared on
   * `selectPlan` and `clearSelection` so a stale plan's history
   * doesn't bleed across selections.
   */
  outcomesHistory: RecipeOutcomesHistoryEntryDto[];
  /**
   * Plan-expectation coverage matrix (Session 46). One row per
   * (bucket, index) the plan declares — observation_metric,
   * event_type, entity_kind, relation_kind — with the recipes that
   * bind to each. Surfaces the recipe-author prompt's "narrow
   * honest coverage" discipline so the operator sees which
   * expectations are uncovered without reading recipe JSON. `null`
   * means we haven't asked yet (the load is plan-status-gated like
   * `records`).
   */
  expectationCoverage: ExpectationCoverageRowDto[] | null;
  error: CommandErrorDto | null;
}

export const plans: PlansState = $state({
  recent: [],
  selected: null,
  // Default to Pending so the user lands on what needs review — the
  // session's purpose is triage, not archaeology. (Session 7 §P1.)
  statusFilter: 'pending' as StatusFilter,
  classifying: false,
  loading: false,
  mutating: false,
  fetching: false,
  fetchReport: null,
  fetchRuns: [],
  recipes: [],
  recipeFeedback: {},
  records: null,
  outcomesHistory: [],
  expectationCoverage: null,
  error: null,
});

function filterToWire(f: StatusFilter): PlanStatusDto | null {
  return f === 'all' ? null : f;
}

/**
 * Refresh the recent-plans list. Called on app boot, after every
 * successful classification, and after every accept/reject so the
 * listing reflects the new status. Honors the current filter.
 */
export async function refreshRecent(limit = 50): Promise<void> {
  plans.loading = true;
  plans.error = null;
  try {
    plans.recent = await listRecentPlans(limit, filterToWire(plans.statusFilter));
  } catch (e) {
    plans.error = asCommandError(e);
  } finally {
    plans.loading = false;
  }
}

/**
 * Change which lifecycle bucket the listing shows. Triggers a refresh
 * automatically — the filter and the data must move together or the
 * listing transiently shows the wrong rows.
 */
export async function setStatusFilter(filter: StatusFilter): Promise<void> {
  if (plans.statusFilter === filter) return;
  plans.statusFilter = filter;
  await refreshRecent();
}

/**
 * Run classification on a topic. Selects the resulting plan on success
 * and refreshes the recent list so the new plan shows up. Errors set
 * `state.error` and leave the previous selection alone — the user
 * doesn't lose context because of a transient gateway failure.
 *
 * A newly-classified plan is always Pending, so flipping the filter
 * to 'pending' (if it isn't there already) keeps the new plan visible
 * in the listing — otherwise classifying while filtered to Accepted
 * would silently hide the freshly-created plan.
 */
export async function classifyTopic(topic: string): Promise<void> {
  plans.classifying = true;
  plans.error = null;
  try {
    const plan = await apiClassify(topic);
    plans.selected = plan;
    if (plans.statusFilter !== 'all' && plans.statusFilter !== 'pending') {
      plans.statusFilter = 'pending';
    }
    await refreshRecent();
  } catch (e) {
    plans.error = asCommandError(e);
  } finally {
    plans.classifying = false;
  }
}

/**
 * Open a plan in the review pane. Called from the listing.
 *
 * Resets the per-plan fetch and recipe state so the previously-viewed
 * plan's history doesn't leak across the selection boundary, then
 * asynchronously refreshes the fetch-run history, the authored-
 * recipes list, the recipe-feedback map, and the records bucket for
 * the newly-selected plan. All refreshes are fire-and-forget;
 * failures are non-fatal and surface as an error banner without
 * blocking the plan body from rendering.
 *
 * Records are only refreshed when the plan is past the `pending`
 * lifecycle state — calling `records_for_plan` on a pending plan is
 * an InvalidInput error, by design (a plan that has never been
 * fetched can't have records, and the command surfaces that
 * lifecycle state explicitly rather than masking it as "empty").
 */
export async function selectPlan(id: string): Promise<void> {
  plans.loading = true;
  plans.error = null;
  plans.fetchReport = null;
  plans.fetchRuns = [];
  plans.recipes = [];
  plans.recipeFeedback = {};
  plans.records = null;
  plans.outcomesHistory = [];
  plans.expectationCoverage = null;
  try {
    const plan = await getPlan(id);
    plans.selected = plan;
    // Pull the recent fetch-run history alongside the plan body so
    // the review pane can render "we ran this 3 times" context
    // without a second user action. Failure to load the history is
    // not fatal — the report panel will just show empty state.
    void refreshFetchRuns(id).catch(() => {});
    // Same for the recipes — load them in the background so the
    // recipe-inspection panel populates as soon as the plan body
    // renders. Empty list is the legitimate state for a plan that
    // hasn't been fetched yet.
    void refreshRecipes(id).catch(() => {});
    // ADR 0013: load the per-(plan, source) feedback notes so the
    // recipe panel's indicator chips render in lockstep with the
    // recipes themselves. Empty map is the common case.
    void refreshRecipeFeedback(id).catch(() => {});
    // Session 22: load the records bucket if the plan is past
    // pending. For a pending plan, the records call is invalid (no
    // fetch has happened yet); leave `plans.records` as null so the
    // bucket panels render their "no expectations / no records"
    // states based purely on the plan's expectations.
    if (plan.status !== 'pending') {
      void refreshRecords(id).catch(() => {});
    }
    // Session 46: load the heatmap history and the expectation
    // coverage matrix alongside the rest of the plan's surfaces.
    // Both are pure reads against existing rows; both render an
    // explicit empty state when there's nothing yet (no fetches
    // run; no recipes authored; etc.) so failure to load is
    // non-fatal.
    void refreshOutcomesHistory(id).catch(() => {});
    void refreshExpectationCoverage(id).catch(() => {});
  } catch (e) {
    plans.error = asCommandError(e);
  } finally {
    plans.loading = false;
  }
}

/**
 * Clear the current selection without making a network call. The
 * listing remains; the review pane returns to its empty state.
 */
export function clearSelection(): void {
  plans.selected = null;
  plans.fetchReport = null;
  plans.fetchRuns = [];
  plans.recipes = [];
  plans.recipeFeedback = {};
  plans.records = null;
  plans.outcomesHistory = [];
  plans.expectationCoverage = null;
}

/**
 * Mark the currently-selected plan as accepted. Optimistic: updates
 * `plans.selected` immediately, then commits on the backend, then
 * refreshes the listing so the row's pill (and possibly its
 * visibility under a filter) updates. Rolls back the optimistic
 * status on backend error.
 *
 * Returning a boolean lets the caller (PlanReview's button handler)
 * disable its UI for the duration without subscribing to `mutating`.
 */
export async function acceptSelected(): Promise<boolean> {
  return await transitionSelected('accepted', apiAcceptPlan);
}

/**
 * Mark the currently-selected plan as rejected, optionally attaching
 * a reason. Same optimistic shape as `acceptSelected`. After a
 * successful reject under a filter that hides rejected plans
 * (Pending, Accepted), the row vanishes from the listing — that's
 * the soft-delete behaviour showing through.
 *
 * `reason` is the user's free-text note from the reject dialog.
 * `null` (or empty/whitespace string) records the rejection without
 * a note. The note is validated by the backend; backend rejection
 * surfaces as `plans.error` and the optimistic status is rolled
 * back. The dialog should still be considered "open" by its caller
 * so the user can edit and resubmit; this helper only returns the
 * boolean success result, leaving dialog state to the caller.
 */
export async function rejectSelected(
  reason: string | null = null,
): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;
  const previousStatus = current.status;
  plans.mutating = true;
  plans.error = null;
  // Optimistic: also project the reason locally so the review pane
  // can show "rejected with reason" immediately. Rolled back on
  // failure alongside the status.
  plans.selected = {
    ...current,
    status: 'rejected',
    rejection_reason: reason ?? '',
  };
  try {
    const updated = await apiRejectPlan(current.id, reason);
    plans.selected = updated;
    await refreshRecent();
    return true;
  } catch (e) {
    plans.error = asCommandError(e);
    if (plans.selected && plans.selected.id === current.id) {
      plans.selected = { ...current, status: previousStatus };
    }
    return false;
  } finally {
    plans.mutating = false;
  }
}

/**
 * Re-classify the currently-selected plan (which must be in
 * `rejected` status) using the rejection reason as additional
 * context for the classifier. Persists a fresh plan with status =
 * `pending` linked back to the predecessor via `reclassified_from`,
 * selects it on success, and refreshes the listing.
 *
 * `editedReason`, when supplied, replaces the stored rejection
 * reason for this single classification call. `null` (or empty)
 * uses the predecessor's stored reason as-is. The backend rejects
 * the call if neither yields any non-empty text after validation.
 *
 * On success: `plans.selected` becomes the new plan; the user
 * lands on the freshly-classified review pane. On failure:
 * `plans.selected` is unchanged; `plans.error` carries the
 * reason; the rejected predecessor remains the selection.
 *
 * Toggles `plans.classifying` (not `plans.mutating`) because this
 * is a Level-1 LLM call — same network footprint as a fresh
 * `classify`. The topic-input spinner reuses the same flag.
 */
export async function reclassifySelected(
  editedReason: string | null = null,
): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;
  if (current.status !== 'rejected') return false;
  plans.classifying = true;
  plans.error = null;
  try {
    const fresh = await apiReclassifyPlan(current.id, editedReason);
    plans.selected = fresh;
    // The new plan is Pending; flip the filter so the user sees
    // it land in the listing.
    if (plans.statusFilter !== 'all' && plans.statusFilter !== 'pending') {
      plans.statusFilter = 'pending';
    }
    await refreshRecent();
    return true;
  } catch (e) {
    plans.error = asCommandError(e);
    return false;
  } finally {
    plans.classifying = false;
  }
}

async function transitionSelected(
  optimistic: PlanStatusDto,
  call: (id: string) => Promise<ResearchPlanDto>,
): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;
  // Capture the pre-mutation status so we can roll back cleanly on
  // backend failure. Rust enforces idempotence so the only failures
  // we expect here are NotFound (id has been removed since the
  // listing fetch) and Storage (DB error).
  const previousStatus = current.status;
  plans.mutating = true;
  plans.error = null;
  // Optimistic update: the user sees the new status immediately.
  plans.selected = { ...current, status: optimistic };
  try {
    const updated = await call(current.id);
    plans.selected = updated;
    await refreshRecent();
    // Session 22: a fresh accept makes records *queryable* (the
    // command refuses pending plans). Kick a refresh so the bucket
    // panels stop showing "no fetch yet" empty-state and start
    // showing "expectations present, no records yet" empty-state
    // (or the actual records, after the first fetch).
    if (optimistic === 'accepted') {
      void refreshRecords(updated.id).catch(() => {});
    }
    return true;
  } catch (e) {
    plans.error = asCommandError(e);
    // Roll back. If the optimistic mutation was the only change, the
    // selection now matches what's on disk again.
    if (plans.selected && plans.selected.id === current.id) {
      plans.selected = { ...current, status: previousStatus };
    }
    return false;
  } finally {
    plans.mutating = false;
  }
}

/**
 * Format a wire `created_at` (ISO 8601 string from chrono) as a short
 * "YYYY-MM-DD HH:mm" string in local time. Used by the listing.
 */
export function formatCreatedAt(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const yyyy = d.getFullYear();
  const mm = String(d.getMonth() + 1).padStart(2, '0');
  const dd = String(d.getDate()).padStart(2, '0');
  const hh = String(d.getHours()).padStart(2, '0');
  const min = String(d.getMinutes()).padStart(2, '0');
  return `${yyyy}-${mm}-${dd} ${hh}:${min}`;
}

/**
 * Run the fetch executor against the currently-selected plan.
 *
 * Stores the resulting report under `plans.fetchReport` for the
 * `<FetchReport>` component to render, and refreshes the fetch-run
 * history strip so the new run shows up in the timeline. Per-recipe
 * failures live inside the report and don't surface as an error
 * banner; only wholesale failures (plan not accepted, executor
 * couldn't author, etc.) populate `plans.error`.
 *
 * No-op when nothing is selected — the button is hidden in that
 * state, but the guard makes the function safe to call defensively.
 */
export async function runFetch(): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;

  plans.fetching = true;
  plans.error = null;
  try {
    const report = await apiRunFetchForPlan(current.id);
    plans.fetchReport = report;
    // Refresh the runs list so the new entry appears at the top.
    // Failure to refresh is non-fatal — the report itself is
    // already showing in the UI.
    void refreshFetchRuns(current.id).catch(() => {});
    // Also refresh recipes — the first run is what triggers
    // Level-2 authoring, so the recipes panel goes from empty to
    // populated on that first call. Subsequent runs against an
    // already-authored plan are idempotent for the recipe list,
    // but refreshing is cheap and keeps the panel in sync if the
    // plan's bound sources ever expand.
    void refreshRecipes(current.id).catch(() => {});
    // Session 22: a fetch run is the only thing that produces
    // records, so refresh the bucket alongside the runs and recipes.
    // The records command is safe at this point — the plan must
    // have been Accepted for the fetch to have run.
    void refreshRecords(current.id).catch(() => {});
    // Session 46: a successful fetch writes outcome rows and may
    // author new recipes; refresh both heatmap history and coverage
    // matrix so the panels reflect what just happened.
    void refreshOutcomesHistory(current.id).catch(() => {});
    void refreshExpectationCoverage(current.id).catch(() => {});
    return true;
  } catch (e) {
    plans.error = asCommandError(e);
    return false;
  } finally {
    plans.fetching = false;
  }
}

/**
 * Refresh the fetch-run history strip for a plan. Pure read; can be
 * called freely. Doesn't toggle `plans.fetching` (that flag is
 * reserved for the active executor call) — a slow read shouldn't
 * disable the run button.
 */
export async function refreshFetchRuns(planId: string): Promise<void> {
  try {
    plans.fetchRuns = await apiListFetchRuns(planId, 10);
  } catch (e) {
    // Non-fatal: history is a nicety, not a precondition.
    plans.error = asCommandError(e);
  }
}

/**
 * Refresh the authored-recipes list for a plan. Pure read; called
 * alongside `selectPlan` and after each successful `runFetch`.
 *
 * Like `refreshFetchRuns`, this doesn't toggle any spinner — the
 * recipes panel renders its own empty state until the call returns,
 * and a slow read shouldn't visibly block the rest of the review
 * pane. Failure surfaces as an error banner but leaves the cached
 * `plans.recipes` array intact, so a transient network blip during a
 * background refresh doesn't blank a useful panel.
 */
export async function refreshRecipes(planId: string): Promise<void> {
  try {
    plans.recipes = await apiListRecipesForPlan(planId);
  } catch (e) {
    // Non-fatal: same rationale as refreshFetchRuns. Don't reset
    // plans.recipes here — preserving the previous list is more
    // useful than blanking it on a transient failure.
    plans.error = asCommandError(e);
  }
}

/**
 * Refresh the recipe-feedback map for a plan. ADR 0013. Pure read;
 * called alongside `selectPlan` and after each successful
 * `flagRecipe` / `clearRecipeFeedback` so the indicator chips stay
 * in sync with what's actually persisted.
 *
 * Wire shape is `RecipeFeedbackDto[]`; the store keeps it as a
 * `Record<source_id, RecipeFeedbackDto>` so per-recipe lookups in
 * the panel are O(1) and reactivity is property-grained.
 */
export async function refreshRecipeFeedback(planId: string): Promise<void> {
  try {
    const list = await apiListRecipeFeedbackForPlan(planId);
    const next: Record<string, RecipeFeedbackDto> = {};
    for (const fb of list) {
      next[fb.source_id] = fb;
    }
    plans.recipeFeedback = next;
  } catch (e) {
    // Non-fatal: same rationale as refreshFetchRuns / refreshRecipes.
    // Don't reset the map; preserving the previous state is more
    // useful than blanking it on a transient failure.
    plans.error = asCommandError(e);
  }
}

/**
 * Refresh the records bucket for a plan (Session 22). Pure read;
 * called alongside `selectPlan` (when the plan is past pending),
 * after a successful `runFetch`, and after a successful accept (to
 * flip the bucket panels from "no fetch yet" to "no records yet"
 * empty state).
 *
 * Like the other background refreshes, this doesn't toggle any
 * spinner. Failure surfaces as an error banner; the cached
 * `plans.records` is preserved on transient failure so a network
 * blip doesn't blank populated bucket panels.
 *
 * The backend refuses pending plans with InvalidInput. Callers are
 * expected to gate on plan status; this helper does not re-check
 * (and the backend's check is the canonical one in any case).
 */
export async function refreshRecords(planId: string): Promise<void> {
  try {
    plans.records = await apiRecordsForPlan(planId);
  } catch (e) {
    // Non-fatal: same rationale as refreshFetchRuns. Preserving the
    // previous bucket is more useful than blanking populated panels
    // because a background refresh hit a transient error.
    plans.error = asCommandError(e);
  }
}

/**
 * Flag a recipe by attaching a free-text operator note for the
 * (selected plan, source_id) pair. ADR 0013. Optimistic: updates
 * `plans.recipeFeedback[sourceId]` immediately so the chip / dialog
 * reflect the change without a refresh roundtrip; rolls back on
 * backend error.
 *
 * Returns `true` on success. The caller (the dialog's submit
 * handler) closes the dialog on `true`, leaves it open on `false`
 * so the user sees the error and can edit + resubmit.
 *
 * No-op when nothing is selected (the panel is hidden in that
 * state, but the guard makes the function safe to call defensively).
 */
export async function flagRecipe(
  sourceId: string,
  note: string,
): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;

  const trimmed = note.trim();
  if (trimmed.length === 0) {
    // Empty after trim → clear path. Mirror the backend's
    // single-command-collapse so the store has one entry point per
    // user intent.
    return clearRecipeFeedback(sourceId);
  }

  const previous = plans.recipeFeedback[sourceId];
  // Optimistic write. The created_at is approximate (the backend
  // stamps the canonical value); the chip cares about presence,
  // not exact timestamp.
  plans.recipeFeedback[sourceId] = {
    plan_id: current.id,
    source_id: sourceId,
    note: trimmed,
    created_at: new Date().toISOString(),
  };
  plans.mutating = true;
  plans.error = null;
  try {
    const persisted = await apiSetRecipeFeedback(current.id, sourceId, trimmed);
    if (persisted) {
      // Replace the optimistic row with the canonical persisted one
      // so subsequent reads see the backend's `created_at`.
      plans.recipeFeedback[sourceId] = persisted;
    }
    return true;
  } catch (e) {
    // Roll back the optimistic update on failure.
    if (previous) {
      plans.recipeFeedback[sourceId] = previous;
    } else {
      delete plans.recipeFeedback[sourceId];
    }
    plans.error = asCommandError(e);
    return false;
  } finally {
    plans.mutating = false;
  }
}

/**
 * Clear the operator-feedback note for a (selected plan, source_id)
 * pair. ADR 0013. Optimistic: removes the entry from
 * `plans.recipeFeedback` immediately, restores it on backend error.
 *
 * Returns `true` on success. Idempotent: clearing an already-cleared
 * source succeeds.
 */
export async function clearRecipeFeedback(sourceId: string): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;

  const previous = plans.recipeFeedback[sourceId];
  delete plans.recipeFeedback[sourceId];
  plans.mutating = true;
  plans.error = null;
  try {
    await apiSetRecipeFeedback(current.id, sourceId, null);
    return true;
  } catch (e) {
    // Roll back: restore the previous note if there was one.
    if (previous) {
      plans.recipeFeedback[sourceId] = previous;
    }
    plans.error = asCommandError(e);
    return false;
  } finally {
    plans.mutating = false;
  }
}

/**
 * Manually re-author a recipe — Track A, ADR 0012 amendment 1.
 *
 * Calls the backend `reauthor_recipe` command with the prior
 * recipe's id and the operator's optional note from the
 * ReauthorDialog. On success, refreshes the recipe list for the
 * selected plan so the new recipe (with `prior_recipe_id` populated
 * pointing back at the prior) appears in the inspection panel — the
 * lineage chip on the new card is the operator's confirmation that
 * the version chain extended.
 *
 * Returns `true` on success. The dialog closes on `true`; on `false`
 * it stays open so the operator sees `plans.error` and can retry or
 * cancel.
 *
 * No-op when nothing is selected (the panel hides the button in
 * that state, but the guard makes the function safe to call
 * defensively).
 */
/**
 * Refresh the recipe-success heatmap's outcome history (Session 46).
 * Pure read; safe to call freely. Like the other background
 * refreshes this doesn't toggle a spinner — the panel renders its
 * own empty state until the call returns.
 *
 * Failure surfaces as an error banner; the previous
 * `plans.outcomesHistory` is preserved so a transient network blip
 * doesn't blank a populated heatmap.
 */
export async function refreshOutcomesHistory(planId: string): Promise<void> {
  try {
    plans.outcomesHistory = await apiRecipeOutcomesHistory(planId, 20);
  } catch (e) {
    plans.error = asCommandError(e);
  }
}

/**
 * Refresh the expectation-coverage matrix (Session 46). Pure read;
 * called alongside `selectPlan` and after each successful
 * `runFetch` (because the first run authors the recipes that fill
 * the matrix; subsequent runs may extend coverage if the plan was
 * extended).
 *
 * Failure preserves the previous matrix so transient network blips
 * don't blank a populated coverage panel.
 */
export async function refreshExpectationCoverage(planId: string): Promise<void> {
  try {
    plans.expectationCoverage = await apiExpectationCoverage(planId);
  } catch (e) {
    plans.error = asCommandError(e);
  }
}

export async function reauthorRecipe(
  recipeId: string,
  operatorNote: string | null = null,
): Promise<boolean> {
  const current = plans.selected;
  if (!current) return false;

  // Pure write — no optimistic update available because the new
  // recipe id is server-assigned (UUIDv7 minted in the validator)
  // and the new extraction spec / produces bindings come back from
  // the LLM. The dialog spinner covers the latency window (5–10s
  // typical, sometimes 30s+ when xAI is slow).
  plans.mutating = true;
  plans.error = null;
  try {
    await apiReauthorRecipe(recipeId, operatorNote);
    // The new recipe lands in storage with a higher version on the
    // same dedup_key. Refresh the recipe list so the panel shows
    // both the new head and the lineage chip pointing back.
    await refreshRecipes(current.id);
    return true;
  } catch (e) {
    plans.error = asCommandError(e);
    return false;
  } finally {
    plans.mutating = false;
  }
}
