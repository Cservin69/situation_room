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
 * The tone vocabulary maps the wire's four `kind`s onto a closed set
 * of five UI states. `none` is the additional state the recipes panel
 * needs for "this recipe has no fetch outcome yet" — distinct from
 * `skipped` (the executor saw the recipe and decided not to run it),
 * from `failed` (the executor tried and the run failed), and from
 * `rate_limited` (the source threw 429 in a way the executor surfaced
 * to the operator rather than retrying through).
 *
 * Color mapping follows ADR 0006 ("color is a meaning, not
 * decoration") and uses only the canonical signal vars from
 * `global.css`. No hex fallbacks — drift from the design tokens
 * shows up as missing color rather than as embedded hex literals.
 *
 * ## Track D, Session 25 — `rate_limited`
 *
 * The new outcome variant gets its own tone (`limited`) which the
 * UI renders in warning amber. The semantic distinction matters:
 *
 *   - `failed` (red): the recipe is broken or the source is broken.
 *     Re-running with no other change won't help.
 *   - `rate_limited` (amber): the source asked us to back off.
 *     Re-running later — either after the surfaced `Retry-After`
 *     window or after switching sources — is the right next step.
 *
 * The label includes the formatted retry-after value when present
 * ("rate-limited; retry after 2m 30s") so the operator sees the
 * actual wait the server requested.
 */
import type { RecipeOutcomeDto } from '$lib/api/types/RecipeOutcomeDto';

export type OutcomeTone = 'ok' | 'skip' | 'fail' | 'limited' | 'none';

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
  if (o.kind === 'rate_limited') return 'limited';
  return 'fail';
}

/**
 * Short human label for the outcome. Designed to fit on a single line
 * of a recipe card header — keep it under ~24 chars so it doesn't
 * wrap awkwardly next to the source-id and recipe-id chrome.
 *
 * For `rate_limited`, the label includes the formatted retry window
 * when one was supplied; otherwise it says "rate-limited" with no
 * value. The `retry_after_seconds: number | null` shape matches the
 * generated DTO exactly.
 */
export function outcomeLabel(o: RecipeOutcomeDto | undefined): string {
  if (!o) return 'no fetch run yet';
  if (o.kind === 'succeeded') {
    return `${o.records_produced} record${o.records_produced === 1 ? '' : 's'}`;
  }
  if (o.kind === 'skipped') return 'skipped';
  if (o.kind === 'rate_limited') {
    const secs = o.retry_after_seconds;
    if (secs === null || secs === undefined) {
      return 'rate-limited';
    }
    return `rate-limited; retry after ${formatRetryAfter(secs)}`;
  }
  return `failed @ ${o.stage}`;
}

/**
 * Long-form detail for the outcome — the failure message, the skip
 * reason, the rate-limit guidance, or the empty string for success.
 * Callers are expected to conditionally render based on whether this
 * returns non-empty.
 *
 * The success case returns `''` rather than `null` so callers can
 * use `{#if outcomeDetail(o)}` without coercion ceremony.
 */
export function outcomeDetail(o: RecipeOutcomeDto | undefined): string {
  if (!o) return '';
  if (o.kind === 'skipped') return o.reason;
  if (o.kind === 'failed') return o.message;
  if (o.kind === 'rate_limited') {
    const secs = o.retry_after_seconds;
    if (secs === null || secs === undefined) {
      return 'The source returned 429 with no Retry-After header. Try again later, or pick a different source.';
    }
    return `The source asked to wait ${formatRetryAfter(secs)} before retrying. Re-run the fetch after that window.`;
  }
  return '';
}

/**
 * Format a `Retry-After` value (seconds, non-negative integer) as a
 * concise human string: "45s", "2m 30s", "1h 5m 0s". Mirrors the
 * Rust-side helper in `crates/pipeline/src/fetch_backoff.rs::format_duration`
 * — same shape, same boundaries — so log lines and UI copy match.
 *
 * The function is deliberately exported so other components can
 * format the same way without re-implementing the logic.
 */
export function formatRetryAfter(secs: number): string {
  if (secs < 0 || !Number.isFinite(secs)) return `${secs}s`;
  const total = Math.floor(secs);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
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
