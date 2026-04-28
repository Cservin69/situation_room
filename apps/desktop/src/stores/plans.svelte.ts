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
 * - `classifying`: true while a classify call is in flight. Drives the
 *   topic-input spinner.
 * - `loading`: true while listRecent / getPlan calls are in flight.
 *   Drives panel skeletons.
 * - `error`: the last surfaced error, if any. Cleared on next successful
 *   action.
 *
 * ## Why a single state object
 *
 * Multiple separate `$state` runes would force consumers to choose
 * which one to subscribe to. One object keeps reads cheap (Svelte's
 * fine-grained reactivity tracks per-property accesses inside the
 * proxy) and writes obvious.
 */
import { classify as apiClassify, listRecentPlans, getPlan, asCommandError } from '$lib/api/client';
import type { PlanSummary } from '$lib/api/types/PlanSummary';
import type { ResearchPlanDto } from '$lib/api/types/ResearchPlanDto';
import type { CommandErrorDto } from '$lib/api/types/CommandErrorDto';

interface PlansState {
  recent: PlanSummary[];
  selected: ResearchPlanDto | null;
  classifying: boolean;
  loading: boolean;
  error: CommandErrorDto | null;
}

export const plans: PlansState = $state({
  recent: [],
  selected: null,
  classifying: false,
  loading: false,
  error: null,
});

/**
 * Refresh the recent-plans list. Called on app boot and after every
 * successful classification so a new plan appears in the sidebar.
 */
export async function refreshRecent(limit = 50): Promise<void> {
  plans.loading = true;
  plans.error = null;
  try {
    plans.recent = await listRecentPlans(limit);
  } catch (e) {
    plans.error = asCommandError(e);
  } finally {
    plans.loading = false;
  }
}

/**
 * Run classification on a topic. Selects the resulting plan on success
 * and refreshes the recent list so the new plan shows up. Errors set
 * `state.error` and leave the previous selection alone — the user
 * doesn't lose context because of a transient gateway failure.
 */
export async function classifyTopic(topic: string): Promise<void> {
  plans.classifying = true;
  plans.error = null;
  try {
    const plan = await apiClassify(topic);
    plans.selected = plan;
    // Fire-and-await so the listing reflects the new plan immediately.
    await refreshRecent();
  } catch (e) {
    plans.error = asCommandError(e);
  } finally {
    plans.classifying = false;
  }
}

/**
 * Open a plan in the review pane. Called from the listing.
 */
export async function selectPlan(id: string): Promise<void> {
  plans.loading = true;
  plans.error = null;
  try {
    plans.selected = await getPlan(id);
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
