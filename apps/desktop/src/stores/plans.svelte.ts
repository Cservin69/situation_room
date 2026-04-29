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
  runFetchForPlan as apiRunFetchForPlan,
  listFetchRuns as apiListFetchRuns,
  asCommandError,
} from '$lib/api/client';
import type { PlanSummary } from '$lib/api/types/PlanSummary';
import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
import type { PlanStatusDto } from '$lib/api/types/PlanStatusDto';
import type { CommandErrorDto } from '$lib/api/types/CommandErrorDto';
import type { FetchReportDto } from '$lib/api/types/FetchReportDto';
import type { FetchRunSummaryDto } from '$lib/api/types/FetchRunSummaryDto';

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
 * Resets the per-plan fetch state (`fetchReport`, `fetchRuns`) so
 * the previously-viewed plan's history doesn't leak across the
 * selection boundary, then asynchronously refreshes the fetch-run
 * history for the newly-selected plan.
 */
export async function selectPlan(id: string): Promise<void> {
  plans.loading = true;
  plans.error = null;
  plans.fetchReport = null;
  plans.fetchRuns = [];
  try {
    plans.selected = await getPlan(id);
    // Pull the recent fetch-run history alongside the plan body so
    // the review pane can render "we ran this 3 times" context
    // without a second user action. Failure to load the history is
    // not fatal — the report panel will just show empty state.
    void refreshFetchRuns(id).catch(() => {});
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
 * Mark the currently-selected plan as rejected. Same optimistic
 * shape as `acceptSelected`. After a successful reject under a
 * filter that hides rejected plans (Pending, Accepted), the row
 * vanishes from the listing — that's the soft-delete behaviour
 * showing through.
 */
export async function rejectSelected(): Promise<boolean> {
  return await transitionSelected('rejected', apiRejectPlan);
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
