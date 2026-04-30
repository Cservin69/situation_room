/**
 * Shared outcome-rendering helpers.
 *
 * Both `FetchReport.svelte` and `RecipesPanel.svelte` render the same
 * `RecipeOutcomeDto` discriminated union — the report panel as a list
 * of one row per outcome, the recipes panel as a per-recipe badge
 * matched to the most recent fetch run by `recipe_id`. Without a
 * shared helper module the two components would invent slightly
 * different label strings ("failed @ apply" vs "Failed: Apply", etc.)
 * and the visual language would drift.
 *
 * Plain `.ts` rather than `.svelte.ts` because there is no runes
 * state here — these are pure functions over a discriminated union.
 *
 * The tone vocabulary maps the wire's three `kind`s onto a closed set
 * of four UI states. `none` is the additional state the recipes panel
 * needs for "this recipe has no fetch outcome yet" — distinct from
 * `skipped` (the executor saw the recipe and decided not to run it)
 * and from `failed` (the executor tried and the run failed).
 *
 * Color mapping follows ADR 0006 ("color is a meaning, not
 * decoration") and uses only the canonical signal vars from
 * `global.css`. No hex fallbacks — drift from the design tokens
 * shows up as missing color rather than as embedded hex literals.
 */
import type { RecipeOutcomeDto } from '$lib/api/types/RecipeOutcomeDto';

export type OutcomeTone = 'ok' | 'skip' | 'fail' | 'none';

/**
 * Map a wire outcome to its UI tone. The recipes panel uses
 * [`outcomeForRecipe`] below to get an `Option<RecipeOutcomeDto>`;
 * `outcomeTone(undefined)` returns `'none'` so callers don't have
 * to branch on the option themselves.
 */
export function outcomeTone(o: RecipeOutcomeDto | undefined): OutcomeTone {
  if (!o) return 'none';
  if (o.kind === 'succeeded') return 'ok';
  if (o.kind === 'skipped') return 'skip';
  return 'fail';
}

/**
 * Short human label for the outcome. Designed to fit on a single line
 * of a recipe card header — keep it under ~24 chars so it doesn't
 * wrap awkwardly next to the source-id and recipe-id chrome.
 */
export function outcomeLabel(o: RecipeOutcomeDto | undefined): string {
  if (!o) return 'no fetch run yet';
  if (o.kind === 'succeeded') {
    return `${o.records_produced} record${o.records_produced === 1 ? '' : 's'}`;
  }
  if (o.kind === 'skipped') return 'skipped';
  return `failed @ ${o.stage}`;
}

/**
 * Long-form detail for the outcome — the failure message, the skip
 * reason, or the empty string for success. Callers are expected to
 * conditionally render based on whether this returns non-empty.
 *
 * The success case returns `''` rather than `null` so callers can
 * use `{#if outcomeDetail(o)}` without coercion ceremony.
 */
export function outcomeDetail(o: RecipeOutcomeDto | undefined): string {
  if (!o) return '';
  if (o.kind === 'skipped') return o.reason;
  if (o.kind === 'failed') return o.message;
  return '';
}

/**
 * Find the outcome for a given recipe id in a list of outcomes.
 * Returns `undefined` if the list is missing (no run has happened
 * for the selected plan yet) or if the recipe wasn't part of the
 * most recent run.
 *
 * This is O(n) per lookup; n is the number of recipes per plan,
 * which has historically been ≤ 5. If that ever grows to the point
 * where the linear scan matters, build a Map at the call site.
 */
export function outcomeForRecipe(
  recipeId: string,
  outcomes: RecipeOutcomeDto[] | undefined,
): RecipeOutcomeDto | undefined {
  if (!outcomes) return undefined;
  return outcomes.find((o) => o.recipe_id === recipeId);
}
