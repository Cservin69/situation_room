# Session 52 — Patch 1

The Session 51 patch landed two pieces (Bucket body height cap;
propose-URL prompt v1.0 → v1.1). The 2026-05-09 lithium re-run is
still in-flight as this session opens — Session 52 is the operator
asking the L2 landing for more polish while v1.1 runs against the
real plan.

The polish thread the operator picked was the highest-value of the
four shortlisted in conversation: surface per-nomination outcome
state *adjacent to the L1 expectation it serves*, instead of
forcing the operator to scan the bucket grid, then jump down to
RecipeOutcomesHeatmap and read the row-id back to figure out which
nomination produced which outcome. The surface costs almost
nothing (small glyph in the existing aside slot; expandable
chronology in the existing row-expand surface) and reframes the
Document bucket from "the plan-side view" to "the index page for
fetch outcomes."

This patch lands two pieces, both UI-only, both keyed off existing
wire data (`plans.outcomesHistory`, the same surface
RecipeOutcomesHeatmap already reads):

- **Piece A (status glyph):** each nomination row in the Document
  bucket now renders a small per-nomination outcome glyph in its
  aside slot — `✓` authored / `✗` declined / `⚠` failed / `⧖`
  rate-limited / `·` skipped / `!` legacy / `◌` no run yet — coloured
  by the existing signal palette. Hover surfaces a multi-line
  tooltip (status, attempted-at timestamp, records produced,
  failure stage, retry-after, full message).
- **Piece B (prior-attempts chronology):** when an operator clicks
  a nomination row to expand it, the expanded panel now shows a
  per-run chronology under the rationale — newest first — with
  `attempted_at`, `outcome_kind`, short run id, and a head of the
  message. The full message hovers in the line's `title=` for
  long decline strings (the propose-URL retry loop's `attempts:
  url1 — fetch failed: …; url2 — recipe author declined: …`
  composition).

The two pieces share a session because they read the same wire
data and surface in the same workflow — running a fetch on a real
plan and asking "what just happened, per L1 expectation."

## Why this surface earns its weight

The Session-46 RecipeOutcomesHeatmap already shows per-run
outcomes, and FetchReport shows the most recent run's outcomes.
The missing surface is per-nomination state *next to the L1 row*
the operator is reading. Pre-Session-52 the workflow was:

1. Read the Document bucket (seven nominations on the lithium
   plan).
2. Scroll past Bucket → past FetchReport → past
   RecipeOutcomesHeatmap to find the row for the nomination's
   `source_id`.
3. Read the heatmap row backwards to map `source_id` →
   nomination_id → which row in the bucket above.

Step 3 is the friction. `source_id` for a nomination is either
`nom:{nomination_id}` (decline) or
`nom:{nomination_id}:{bucket}:{index}` (recipe-bearing), and
neither shape carries the human description the operator's eye
last touched in the bucket panel. Bringing the outcome state up
to the row eliminates the scroll-and-cross-reference loop.

Piece B is the same logic applied to the post-Session-51 audit
question: "did the v1.1 propose-URL override actually pivot the
proposer to news/trade-press surfaces on this plan?" That answer
lives in the per-run message strings (the proposer's decline
text names the URLs it tried). Surfacing the chronology under
the row makes the override's behaviour observable inline against
the L1 expectation it was meant to satisfy, rather than buried
in heatmap cell tooltips.

## Apply

Files were edited in place. To verify (operator runs cargo /
svelte-check on Mac per the cargo-on-Mac workflow; sandbox can't
reach crates.io and the rollup native binary doesn't load on the
sandbox's linux-arm64 host):

```
cd ~/Documents/Claude/Projects/SituationRoom
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") | tee ../../ui-check.log
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

Both Rust workspaces are untouched (`cargo build` / `cargo test`
are run for completeness, not because anything changed there).
The change set is frontend-only — three new files, two surgical
edits to existing components. No new dependencies. No schema
change. No migration. No new IPC commands. No new ts-rs DTOs.
No new Tauri commands. No new Rust types.

## Files changed

### New: `apps/desktop/src/lib/nominationOutcomes.ts`

Pure-functions helper module that walks `plans.outcomesHistory`
and answers: which entries belong to a given `nomination_id`,
what's the latest run cell, what's the closed-set status slug for
the latest cell. Exposes:

- `entriesForNomination(history, nominationId)` — filters by
  source_id matching `nom:{uuid}` exactly OR `nom:{uuid}:`-prefix.
  Both shapes are emitted by `fetch_executor::compose_source_id`
  for the same nomination (decline vs recipe-bearing).
- `runsForNomination(history, nominationId)` — flattens matching
  entries' runs into a single newest-first list, sorted by
  `attempted_at`. Cells with unparseable timestamps land at the
  end rather than being dropped.
- `latestRunForNomination(history, nominationId)` — index 0 of
  the above, or `null`.
- `nominationStatus(history, nominationId)` — maps the latest
  cell's `outcome_kind` to the closed `NominationStatus` set
  (`'authored' | 'declined' | 'failed' | 'rate_limited' |
  'skipped' | 'legacy' | 'idle'`). `'idle'` is the
  no-run-yet slug the glyph component renders distinctly from
  `'skipped'`.

Plain `.ts` (no runes state, pure functions over wire DTOs) —
follows the existing `$lib/outcomes.ts` shape.

### New: `apps/desktop/src/components/panels/NominationStatusGlyph.svelte`

Reads the helper's `nominationStatus` and `latestRunForNomination`
via `$derived`, renders a single character in a 14×14 inline-flex
span:

```
✓  authored      — var(--signal-positive)
✗  declined      — var(--fg-tertiary)        (neutral-attention,
                                               not a runtime fail)
⚠  failed        — var(--signal-negative)
⧖  rate-limited  — var(--signal-warning)
·  skipped       — var(--fg-quaternary)
!  legacy        — var(--fg-tertiary)
◌  idle          — var(--fg-quaternary)
```

The colour mapping mirrors the FetchReport / RecipeOutcomesHeatmap
tone vocabulary so a glance across all three surfaces reads
consistently. The `title=` attribute carries a multi-line tooltip
(status + timestamp + records + stage + retry-after + message)
matching the heatmap cell's tooltip shape — operator gets the
same hover detail in both places.

Closed-vocabulary discipline: the glyph mapping is uniform across
all post-Session-39 nominations regardless of priority tier or
source class. No host names, no per-publisher routing.

### New: `apps/desktop/src/components/panels/NominationAttempts.svelte`

Reads `runsForNomination` and renders a `<ol class="entries">`
with one row per fetch run that touched this nomination, newest
first. Layout: timestamp · outcome_kind · short run-id · message
head (capped at 160 chars; full string in the row's `title=`).
Empty state ("no fetch attempts yet for this nomination") for
fresh-classified plans or pre-Session-46 runs.

The chronology is **cross-run**, not intra-run. The propose-URL
retry loop's intra-run URL attempts are summarised inline in the
decline `message` field by `fetch_executor.rs::author_one`:
`url proposer declined after N attempt(s): {reason}; attempts:
{url1, ...; url2, ...}`. That string surfaces verbatim in the
`.msg` column on declined rows, so the v1.1 override's behaviour
(which URLs the proposer tried, what each one returned) is
visible inline. Adding a separate intra-run wire surface would
require an IPC channel — out of scope for a polish session.

### Edited: `apps/desktop/src/components/panels/ExpectationRow.svelte`

Added an optional `extras?: Snippet` prop, rendered inside the
expanded panel below the rationale. The empty-state for
`extras=undefined` is "render nothing" — non-Document bucket
callers (Observation, Event, Entity, Relation, Assertion) are
unchanged because they don't pass the prop.

CSS adds a single `.extras { padding: 4px 6px 6px 6px; }` rule —
inset matches the rationale's left padding so the chronology
aligns under the row's text column rather than the row's hit
area.

### Edited: `apps/desktop/src/components/PlanReview.svelte`

Two changes:

- **Imports:** added `NominationStatusGlyph` and
  `NominationAttempts` from `$components/panels/`.
- **Document bucket nomination rows:** wired the glyph into the
  existing `aside` snippet (rendered before the priority-tier
  Chip), and the chronology into a new `extras` snippet. Doc-
  block above the `ExpectationRow` block names Session 52 as the
  scope and points at the surface for future readers.

The legacy `s.kind === 'legacy'` branch is untouched — legacy
plans don't have a `nomination_id` to thread through, so the
glyph and chronology don't apply. The legacy "warning" Chip
stays as-is.

## Design notes worth preserving

### Why aside-of-row, not below-the-row, for the glyph

The Document bucket is dense by design (six bucket types on a
CSS grid; tight typography). A new full-width row under each
nomination would balloon vertical density and force the bucket-
body height cap (Session 51) to scroll where it didn't before.
The aside slot already exists, already runs to the right of the
label, already carries the priority-tier Chip — adding one
character before the chip is a 14-pixel visual cost and zero
vertical cost. The chronology takes the vertical slot only when
the operator opts in by clicking the row.

### Why a closed-set of glyphs and not just a coloured dot

A coloured dot would convey "good" / "bad" / "neutral" at a
glance, but the operator's question is not "did this work" but
*"what specifically happened."* `declined` and `failed` are
both "bad" but mean different things — a decline is a deliberate
LLM read of the source ("this can't be authored"); a failure is
a runtime breakdown. Glyph + colour together (`✗` neutral-
tertiary vs `⚠` red) carries both axes in one cell. Same for
`⧖` (rate-limited, amber: source asked us to wait) vs `·`
(skipped, dim: executor declined the recipe).

### Why the chronology is cross-run, not intra-run

Intra-run URL attempts (the propose-URL retry loop's three URLs
per nomination) are not exposed as wire rows. They are
summarised inline in the decline `message` field by
`author_one` — `attempts: url1 — fetch failed: timeout after Ns;
url2 — recipe author declined: SPA; ...`. That string surfaces
verbatim in the `.msg` column of the decline row's chronology
entry. So the operator *sees* the intra-run behaviour through
the rendered string, even though the rows themselves are keyed
per-run. Adding a per-attempt wire surface would require:

- a new storage table (or a JSON column on
  `fetch_run_outcomes`),
- a new ts-rs DTO,
- a new IPC command + frontend store hook,
- and a decision about how to render attempts that succeeded vs
  declined within the same run.

That's its own session. The polish here is content-only relative
to the existing wire surface.

### Why `'idle'` is a separate status, not "absence"

A nomination that's never been fetched against and a nomination
that was fetched and `skipped` are observably different states
the operator wants to distinguish. `'idle'` (`◌`, dim quaternary)
reads "no run has reached this row yet — run a fetch" while
`'skipped'` (`·`, even dimmer) reads "the executor saw this and
chose not to author against it." Collapsing them into "no glyph"
would make the bucket panel look broken on a fresh plan; keeping
them distinct keeps the surface self-explanatory across the
plan lifecycle.

### Why this is a polish, not a "live state" thread

The Session 50 deferred work — Tauri event channel for
per-nomination live progress during a fetch — would let the
glyph flip `◌ → … → ✓` as the run progresses. That requires a
new IPC surface, lifecycle decisions, etc. The Session 52
polish reads only what's already persisted (`fetch_run_outcomes`
via `outcomesHistory`), so the glyph reflects the *most recent
completed run's* outcome — not live state. When the live channel
lands, the glyph becomes a single point of truth that animates
during a run; today it animates between runs. Both shapes use
the same closed-set vocabulary, so the future surface
substitutes cleanly.

### Why all-bucket polish is not part of this patch

Other buckets (Observation, Event, Entity, Relation, Assertion)
also have rows the operator might want to attach an outcome
glyph to. But: their bucket-source mapping is many-to-many. An
observation metric like `lithium_production_tonnes` can be
satisfied by *any* of the seven nominations — the recipe-author
decides which nomination's bytes carry that metric at recipe-
binding time. Surfacing per-row outcome state on a many-to-many
relation requires per-expectation provenance the wire surface
doesn't carry today (Session 23 architectural follow-up). The
Document bucket's nominations are 1:1 with `source_id` prefixes;
that's why the surface lands cleanly here first.

If the next live run shows the polish is valuable enough to
apply across the other buckets, the lift is the
expectation-binding architectural follow-up the Session 23
handoff already names.

### Why the chronology's empty state matters

A nomination that's never been fetched is the legitimate state
for a freshly-classified plan or a plan whose runs predate the
Session-46 outcomes-history migration. Without an explicit empty
state the chronology slot would be invisible, which would lead
the operator to think the row had no chronology surface —
confusing the row's affordance.

The italic-dim hint ("no fetch attempts yet for this nomination")
keeps the surface self-describing across the lifecycle: pre-
fetch, post-fetch-with-runs, and post-Session-52-on-pre-Session-46-
plans all render distinctly without the operator needing to
remember which is which.

## Test deltas

- `apps/desktop/src/lib/nominationOutcomes.ts` — no test deltas.
  The functions are pure over the wire DTO; the helpers' `source_id`
  parsing is pinned by the storage layer's
  `recipes_dedup_key_uses_full_nomination_id_session_47` test
  (and the executor's
  `compose_source_id_session_47_format` test) which exercise the
  same wire shape this module's filter/prefix logic mirrors. A
  wire-shape change there would surface as a frontend silent
  miss, and the existing live-run loop is the right place to
  catch it.
- `apps/desktop/src/components/panels/NominationStatusGlyph.svelte`
  — no test deltas. The component is a small read on a derived
  store value; the existing svelte-check / type-check run will
  catch any prop or token mistakes.
- `apps/desktop/src/components/panels/NominationAttempts.svelte`
  — no test deltas, same rationale.
- `apps/desktop/src/components/panels/ExpectationRow.svelte` —
  the new `extras` snippet is an additive optional prop;
  callers without the prop are unchanged. svelte-check pins
  the prop shape.
- `apps/desktop/src/components/PlanReview.svelte` — content-only
  edit (imports + Document bucket nomination row's snippet
  block). svelte-check pins the snippet shape.

Pipeline test count: unchanged. API test count: unchanged. Other
crates' counts unchanged. All ignored tests (12) remain the
existing `#[ignore]` live integration tests.

## What's intentionally not in this patch

- **Per-bucket-type glyph polish on Observation / Event / Entity
  / Relation / Assertion.** Those buckets' rows have many-to-many
  relations to nominations; the per-row outcome surface needs the
  Session 23 expectation-binding wire shape that doesn't exist
  yet. Defer.
- **Live per-nomination state during a run.** The Session 50
  deferred Tauri event channel still stands as the right shape;
  this patch reads only completed-run history. The glyph
  vocabulary substitutes cleanly when that channel lands.
- **Intra-run URL attempt history as separate wire rows.**
  Today's surface piggybacks on the decline `message` string
  composed by `author_one`. A first-class intra-run history
  surface would require a new storage table + DTO + IPC; out of
  scope for a polish session.
- **Host-backoff strip spacing + semantic color (#3 in
  conversation).** Defer to next session if the lithium re-run
  surfaces the strip's "BACKOFF1" wedge artifact again.
- **Sticky plan header (#4 in conversation).** Defer.
- **Decline rationale redaction / formatting beyond 160-char
  truncation.** Today the rendered string is the executor's
  composed decline message verbatim. If the operator wants
  paragraph breaks at the `attempts:` boundary or per-attempt
  bullets, that's a follow-up — the rendered shape is faithful
  to the wire shape today.
- **xAI Responses API migration.** Same posture as Sessions
  47–51.
- **Promotion pipeline (ADR 0004), Iterator Phase 2 (ADR 0016),
  charts on Observations / Events.** Same posture.
- **L1 / L2 prompt edits.** None this session — the Session 51
  v1.1 override is still being validated by the in-flight live
  run; no new evidence in conversation that warrants another
  bump. The "open-access vs paywall-gated" discussion remains
  parked pending live-run signal (paywall walls show as 200-OK
  with auth-gate HTML, surfacing as `recipe author declined: no
  extractable structure` rather than the 401/403 the v1.1 prompt
  already pivots on; a v1.2 hook is the right shape if observed).

## Hard rules carried over

Same as Sessions 41–51:

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. This patch adds none.
- ADR 0009: every HTTP call goes through `SecureHttpClient`.
  This patch adds none — frontend-only.
- Bounds checking on every IPC string input. This patch adds no
  IPC commands.
- Tauri commands return `CommandError`. This patch adds no
  Tauri commands.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; this patch adds no DTOs (the new `nominationOutcomes.ts`
  module reads existing DTOs only).
- ts-rs DTOs and pipeline / storage structs are intentionally
  separate. Mirror, don't share. This patch adds no Rust types.
- Components only use CSS vars from `global.css`. The new
  glyph and chronology styles use only existing design tokens
  (`--signal-positive` / `--signal-negative` / `--signal-warning`
  / `--fg-{primary,secondary,tertiary,quaternary}` /
  `--border-subtle` / `--font-mono` / `--font-sans`); no hex
  literals.
- Runes-using files end in `.svelte.ts`. The new helper is plain
  `.ts` because it carries no runes state — pure functions over
  wire DTOs. This matches the existing `$lib/outcomes.ts`
  posture.
- L1 prompt edits come from observed classifications, not
  speculation. None this session.
- L2 prompt edits come from observed authoring failures, not
  speculation. None this session — the v1.1 override is still
  being validated.
- **Stockpile prompts: principle-only language.** Unchanged this
  session.
- **Do not write code to pass tests.** No new tests.
- **Closed-vocabulary discipline.** The glyph mapping and
  chronology rendering apply uniformly to all post-Session-39
  nominations regardless of priority tier or source class. No
  host names, no scheme matchers, no domain strings in either
  the helper module or the components. The glyph closed set
  derives from the existing `outcome_kind` closed set; no new
  vocabulary.

End of patch.
