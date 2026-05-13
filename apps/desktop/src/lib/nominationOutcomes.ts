/**
 * Nomination → outcome helpers (Session 52).
 *
 * The Document bucket in `PlanReview.svelte` lists one row per
 * post-Session-39 nomination (`DocumentSourceNominationDto`). Each
 * nomination has a stable `nomination_id` (UUIDv7); each fetch run
 * either authors a recipe against it (`succeeded` / `failed` /
 * `rate_limited` / `skipped`) or surfaces a nomination-level
 * `declined` outcome carrying the propose-URL + recipe-author
 * decision text.
 *
 * The outcomes-history surface (`plans.outcomesHistory`) keys those
 * outcomes by `source_id`, which the executor stamps as either:
 *
 *   - `nom:{nomination_id}` for `declined` rows (no recipe was
 *     authored, no URL was committed-to long enough to derive a
 *     host-shaped id), OR
 *   - `nom:{nomination_id}:{bucket}:{index}` for recipe-bearing
 *     rows once a URL was authored against it (post-Session-47
 *     dedup-key shape — see `fetch_executor::compose_source_id`).
 *
 * This module's filter / sort helpers walk the history to map the
 * nomination's UUID back to the rows the executor produced. They
 * exist so the Document bucket can render a per-row status glyph
 * and prior-attempts chronology adjacent to the L1 expectation it
 * refers to, without each callsite re-discovering the source_id
 * shape.
 *
 * Plain `.ts` (no runes state) — pure functions over the wire DTO.
 */
import type { RecipeOutcomesHistoryEntryDto } from '$lib/api/types/RecipeOutcomesHistoryEntryDto';
import type { RecipeOutcomesHistoryRunCellDto } from '$lib/api/types/RecipeOutcomesHistoryRunCellDto';

/**
 * Closed status taxonomy for a nomination's most-recent outcome.
 *
 * Derived from `RecipeOutcomesHistoryRunCellDto.outcome_kind`
 * (which is the same closed set `RecipeOutcomeDto::kind` uses)
 * plus an `'idle'` slug for the "no fetch run yet" case the
 * status-glyph component needs to render distinctly from a
 * skipped or declined outcome.
 */
export type NominationStatus =
  | 'authored'
  | 'declined'
  | 'failed'
  | 'rate_limited'
  | 'skipped'
  | 'legacy'
  | 'idle';

/**
 * History rows whose `source_id` belongs to this nomination.
 *
 * Two shapes match (see module doc): the bare `nom:{uuid}` decline
 * shape and the `nom:{uuid}:{bucket}:{index}` recipe-bearing
 * shape. Both belong to the same nomination logically — the bucket
 * panel surfaces them together because the operator's question is
 * "what happened to *this* L1 nomination," not "what happened to
 * each per-bucket dedup slot."
 */
export function entriesForNomination(
  history: RecipeOutcomesHistoryEntryDto[],
  nominationId: string,
): RecipeOutcomesHistoryEntryDto[] {
  const exact = `nom:${nominationId}`;
  const prefix = `nom:${nominationId}:`;
  return history.filter(
    (e) => e.source_id === exact || e.source_id.startsWith(prefix),
  );
}

/**
 * All run cells for this nomination across matching entries,
 * sorted newest-first by `attempted_at`. The chronology component
 * renders this directly; the status-glyph component pulls index 0.
 *
 * Cells whose `attempted_at` fails `Date.parse` are pushed to the
 * end of the list rather than dropped — the data is still
 * surfaceable, just temporally unanchored.
 */
export function runsForNomination(
  history: RecipeOutcomesHistoryEntryDto[],
  nominationId: string,
): RecipeOutcomesHistoryRunCellDto[] {
  const all: RecipeOutcomesHistoryRunCellDto[] = [];
  for (const entry of entriesForNomination(history, nominationId)) {
    for (const cell of entry.runs) all.push(cell);
  }
  all.sort((a, b) => {
    const ta = Date.parse(a.attempted_at);
    const tb = Date.parse(b.attempted_at);
    const aBad = Number.isNaN(ta);
    const bBad = Number.isNaN(tb);
    if (aBad && bBad) return 0;
    if (aBad) return 1;
    if (bBad) return -1;
    return tb - ta;
  });
  return all;
}

/** Most-recent run cell for this nomination, or null. */
export function latestRunForNomination(
  history: RecipeOutcomesHistoryEntryDto[],
  nominationId: string,
): RecipeOutcomesHistoryRunCellDto | null {
  const runs = runsForNomination(history, nominationId);
  return runs[0] ?? null;
}

/**
 * Map the latest run cell's `outcome_kind` to the
 * status-glyph closed set. Returns `'idle'` when no run has
 * touched this nomination yet (fresh-classify, or pre-Session-46
 * plan with no `fetch_run_outcomes` rows).
 */
export function nominationStatus(
  history: RecipeOutcomesHistoryEntryDto[],
  nominationId: string,
): NominationStatus {
  const latest = latestRunForNomination(history, nominationId);
  if (!latest) return 'idle';
  switch (latest.outcome_kind) {
    case 'succeeded':
      return 'authored';
    case 'declined':
      return 'declined';
    case 'rate_limited':
      return 'rate_limited';
    case 'skipped':
      return 'skipped';
    case 'legacy_plan_cannot_author':
      return 'legacy';
    case 'failed':
    default:
      return 'failed';
  }
}
