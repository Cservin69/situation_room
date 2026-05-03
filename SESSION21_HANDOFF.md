# STOCKPILE — Session 21 handoff

You are starting Session 21. Session 20 was a security fix only:
`sanitize_for_fence` rewritten to walk `s` directly instead of an
aliased lowercased copy, fixing a panic + sanitizer-bypass +
corruption defect on Unicode inputs whose lowercase has different
byte length from the original (`İ` U+0130, `K` U+212A, `Å` U+212B).
Both classifier and recipe_author copies fixed in parallel. +8
regression tests, 358 → 366. **No P1/P2/P3 work from the Session 20
handoff landed.** Those priorities carry forward unchanged.

Read this whole document before writing any code. ADR 0009 (security
posture), ADR 0013 (recipe-feedback channel), and ADR 0007 (research
function — especially the provenance thread) are the rulebook for
everything below.

## What works today (verified live, end of Session 20)

The first end-to-end live run was performed on a real classified
plan and observed in the desktop UI:

- Topic classified (`south Korea latest election` → plan
  `019de792-b9b5-7182-b764-e9934b119432`), persisted, accepted via
  the UI button.
- `run_fetch_for_plan` invoked from the UI. Fetch executor opened a
  run row, authored two recipes (one per registered source from the
  plan's `document_sources` hints), executed both.
- One recipe (`rss_feeds`, regex-capture against Google News RSS)
  succeeded and produced 1 record.
- One recipe (`gdelt`, json_path `$.articles[0].title`) failed
  `@ fetch` with a status 429 from GDELT.
- The `FetchReport` panel rendered both outcomes: `attempted=2
  succeeded=1 records=1`, failed recipe surfaced with the
  `failed @ fetch` chip, succeeded one with the `1 record` chip.
- The history pill (`09:27:17  1/2  1r`) rendered.
- The recipe-flag dialog mounted (Svelte emitted a warning while
  doing so — see P4).

Session 19 + Session 20 are both shipped and exercised.

## Session 21 priorities

Two carry-forwards from Session 20 + two new findings from the live
run.

### P1 — finish the Session 20 P1 (live xAI verification of v1.8)

Three slots, named in the Session 20 handoff:

- HTML-equivalent USGS MCS page,
- CELEX re-run (the EUR-Lex selector failure case),
- BAKED PDF source.

The operator has a network-enabled machine; this is the work that
was blocked from the Session 20 sandbox. Each slot is one classify
+ one accept + one `run_fetch`. Capture the output diffs and the
authored recipe shapes. If any of the three produces a structurally
surprising recipe, the recipe-flag dialog (Session 19) is now the
path for feeding that observation back as `recipe_feedback` for the
next authoring run.

**Cost-saving alternative worth considering first.** Plan
`019de792-…` already has a recipe (`rss_feeds`) that succeeded once
and a recipe (`gdelt`) that failed `@ fetch` with a 429.
Re-running fetch on that same plan when GDELT isn't rate-limited
costs zero LLM tokens — the recipes are already authored and
persisted; only the fetch step re-runs. Worth doing first to see
whether the authored gdelt recipe's `$.articles[0].title` is in
fact correct against the real response shape. (See P3 for why this
matters.)

### P2 — finish the Session 20 P2 (EUR-Lex failure-case banner)

Top-of-file deferral banner on
`apps/desktop/failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`.

This file was **not** in the `clean_code.zip` archive shipped to
Session 20, which is what blocked it. Two possibilities:

- the file is `.gitignore`'d (failure_cases as a research-artifact
  directory rather than committed prose) — in which case the
  banner work is local-only and `git archive` will never surface it,
- the file simply hadn't been committed at the time of the Session
  20 archive — in which case it's reachable now.

First step is to confirm which. If `.gitignore`'d, decide whether
failure-case files should be committed (probably yes — they're
load-bearing context for future sessions) or kept local-only with
a different propagation mechanism. Either decision is small;
deciding without the operator's input would be guessing.

### P3 — provenance signal for "recipe authored from stub excerpt"

**New, surfaced by the live run.**

Concrete observation. The `gdelt` recipe was authored after the
WARN log line `endpoint_hint pre-fetch failed; authoring will fall
back to stub excerpt`. The recipe (`$.articles[0].title`) is
therefore a *guess at GDELT's response shape*, not a shape the LLM
has actually seen. The recipe is now persisted in the `recipes`
table and will be re-used on every subsequent fetch run for this
plan, with no signal anywhere — not in the recipe row, not in
`FetchReport`, not in the UI — that distinguishes "authored from
real bytes" from "authored from stub".

For a system whose central architectural claim is "every claim is
traceable to its origin", this is a real provenance gap. A user
looking at a successful future fetch from this recipe has no way
to know whether to trust the data shape or whether the recipe
guessed lucky / guessed wrong / produced empty results from a
misaligned path that happened not to throw.

This is **architectural**, not a drop-in patch. Design discussion
before any code — write an ADR. Options to weigh:

- `recipes.authored_from` enum column (`fetched_bytes` |
  `stub_excerpt`), surfaced in the recipe-card UI and on
  `FetchReport.recipe_outcomes[].provenance`. Minimum surface
  change.
- Distinct treatment in the recipe-flag dialog: a stub-authored
  recipe surfaces a "verify against real fetch" hint independently
  of whether the user thinks it's wrong.
- The cleanest invariant: stub-authored recipes are tagged at
  authoring time and **silently re-authored from real bytes on the
  first successful fetch**, regardless of whether the stub-authored
  recipe also succeeded. The system self-heals to real-bytes
  provenance. Most code; most architecturally satisfying.

The third option likely needs its own ADR amendment to ADR 0007.
Don't pick the option in this handoff — that's the operator's call
informed by however much weight they put on "self-healing" vs
"explicit user gesture".

If P3 lands in Session 21, migration is **v10** (v9 was the Session
19 `recipe_feedback` table per the live log). The DuckDB `ALTER
TABLE` constraint trap from Session 7 still applies; read migration
`0009`'s comment block before writing `0010`.

### P4 — Svelte 5 runes warning in RecipeFlagDialog

`apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte:101:22`
triggers:

```
This reference only captures the initial value of `initial`.
Did you mean to reference it inside a closure instead?
https://svelte.dev/e/state_referenced_locally
```

This is a real bug, not a style nag. The line reads a `$state`
value into a function-local at component-mount time, capturing the
snapshot. If `initial` changes during the dialog's lifetime, the
read is stale. The fix depends on what line 101 actually does —
likely `$derived(...)` or moving the read into the relevant handler
closure. Five-minute fix; do it whenever you next touch the file
during P1 or P3.

## What Session 21 is explicitly NOT

- **Not new fetch-executor extraction modes.** json_path,
  regex_capture, css_select, csv_cell are the four working modes
  (Session 8 + extensions). pdf_table is still deferred per Session
  16's pdf-removal call. Adding modes is its own session.
- **Not multi-source orchestration changes.** The current "author
  all → execute all" sequence works; sources fail independently;
  one 429 doesn't poison the run. Don't redesign.
- **Not a recipe-versioning system.** Session 19's `flag` button +
  recipe-feedback channel is the chosen mechanism for marking a
  recipe as wrong; re-authoring on next run is the current healing
  path. If P3 lands, "stub-authored recipes are eligible for silent
  re-authoring on first real-bytes fetch" is a *sub-question
  inside* P3 — answer it there, don't generalize beyond.
- **Not GDELT-specific anything.** GDELT happened to be the source
  that 429'd; the provenance gap is general. Whatever P3 produces
  must work for any source.
- **Not bumping the classifier prompt past v1.8.** Same rule from
  Sessions 5, 6, 7, 18: prompt bumps come from observed
  classifications, not speculation. P1 might surface material that
  justifies v1.9; if so, that's its own deliberate change.

## Hard rules (carry-over, unchanged from Sessions 5–20)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All HTTP
  through `SecureHttpClient`.
- Bounds checking on every IPC string input. `check_user_text` is
  the gate for user-supplied text.
- Tauri commands return `CommandError`, not internal error types.
  Add a variant if a new failure mode genuinely doesn't fit
  existing kinds.
- Generated TS files in `apps/desktop/src/lib/api/types/` written
  by ts-rs via `cargo test -p situation_room_api`. Never hand-edit.
  Ship regenerated files in any patch.
- ts-rs DTOs and pipeline structs are mirrored, not shared.
  Pipeline crate does not depend on ts-rs.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Migrations: read the prior migration's comment block before
  writing the next one. The DuckDB `ALTER TABLE` constraint trap
  is real.
- xAI API key never read, written, or referenced anywhere visible.
  `ApiKey::from_env` only.

Standing-order priority: **security > generalisation > simplicity**.
Session 20 invoked this to deviate from a stated priority list and
ship a security fix instead. The bar for invoking it again is the
same: a real, reachable defect with a concrete consequence path,
against a published ADR's invariant.

## First thing to do in Session 21

1. Read this file.
2. Read ADR 0013 (recipe-feedback channel) and ADR 0007 (research
   function, especially the "provenance" thread) — both load-bearing
   for P3.
3. Look at `crates/pipeline/src/fetch_executor.rs` around the WARN
   log line `endpoint_hint pre-fetch failed; authoring will fall
   back to stub excerpt` — that's the call site where the
   provenance fork happens. The decision about how to thread the
   signal forward begins there.
4. Decide P1-vs-P3 ordering with the operator. P1 is field-work
   (run topics, capture output, possibly bump prompt). P3 is
   architectural (an ADR's worth of design before any code). They
   can interleave but probably shouldn't be done in the same patch.

Build incrementally if P3 is in scope:

1. ADR draft (paper before code).
2. Migration v10 + storage round-trip + tests. `cargo test
   --workspace` passes.
3. `RecipeOutcomeDto` + provenance field in DTOs. ts-rs
   regenerates. `cargo check --workspace` passes.
4. Frontend: recipe-card chip, dialog hint copy, FetchReport panel
   chip.
5. Live verification: classify a topic where one source is
   reachable and one 429s, confirm the chip appears on the right
   recipe.

That order is so every step has a green build behind it. Do not
write the entire session and then run cargo check at the end.

## Continuity note

Session 20's continuity note still applies. The operator is
rigorous about security ("paranoid about security" — earned, not
affected), prefers honesty about uncertainty over false confidence,
reacts well to direct disagreement when warranted, and has
explicitly asked for "do not deviate" discipline.

Session 20's deviation (security fix instead of P1/P2/P3) was
justified post-hoc by the standing-order priority and accepted by
the operator without pushback. The standing posture for future
sessions is unchanged: stick to the plan; deviation requires a real
defect against a published ADR's invariant, not a personal opinion
that something else seems more important.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.
