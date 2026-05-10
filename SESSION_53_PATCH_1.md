# Session 53 — Patch 1

Six pieces, applied in one commit per the operator's authorisation.
The 2026-05-09 18:11 lithium re-run is the observation; pieces A–F
are the response. All code is in place; verification is the operator
running cargo / svelte-check on Mac per the cargo-on-Mac workflow.

## Pieces landed

### Piece A — propose-URL prompt v1.1 → v1.2

`config/prompts/propose_source_url.md`. Header bumped to v1.2. Three
additive amendments:

1. **"Reasonable shot" principle** under "How to weight
   `priority_tier`". Distinguishes parameter-fabrication on opaque
   auth-primary endpoints (forbidden) from proposing a major coverage
   publisher's standard tag/topic/listing path (allowed when
   auth-primary attempts have exhausted). Class-only language; no
   host names, no scheme matchers. Closes the "don't-guess defeats
   pivot" gap from the v1.1 verification.
2. **Two-step host pivot** in the `403/401` and `timeout/5xx`
   bullets. When the news/trade-press host itself blocks (paywalled
   search, CDN-blocked listing, SPA front-page), the same "pivot off
   the host" rule applies recursively. Closes the "news-paywalls-too"
   gap (Reuters returned 401 → no pivot to a different news host on
   v1.1).
3. **Authored bytes ≠ useful bytes** in the
   `recipe author declined: no extractable structure` bullet. When
   prior attempts on the same host produced an `extractable
   structure` decline on an overview/landing/hub page, the host's
   flagship document was not selected precisely; first try a focused
   surface on the same host (single-chapter PDF, press release,
   data-explorer export URL), then pivot off-host. Closes the IEA
   "overview pages fetched but didn't author" gap.

Plus the new prior-attempt shape bullet `recipe authored but apply
failed: <stage> · <message head>` so the proposer reads Piece C's
new entries with the right pivot heuristic ("the path's data shape
doesn't match; pivot to a different path or off-host").

### Piece B — recipe-author author-time shape validator

`crates/pipeline/src/recipe_apply.rs` adds
`validate_recipe_shape_against_bytes(recipe, bytes, plan)` —
strict superset of `validate_recipe_against_bytes`: runs the
runtime's full `extract → build_record` path against the prefetched
bytes, surfacing `ContentAssembly` / `Binding` / `FieldMapping`
failures at authoring time. `crates/pipeline/src/recipe_author.rs`
swaps the `validate_recipe_against_bytes` call for the new one and
threads the plan through.

Catches the `pubs.usgs.gov` "Argentina → f64" and
`www.worldbank.org` "missing field 'value'" classes before the
recipe is persisted. Three observations across two runs is enough —
this is recurrent, not one-off.

Tests pin the contract: numeric extraction validates; string in f64
slot declines with `expected f64`; missing required field declines
with `missing field 'value'`; the validator is a strict superset of
the structural validator (selector-mismatch errors flow through
unchanged).

### Piece C — apply-stage failures fed into prior-attempts

`crates/storage/src/fetch_run_outcomes.rs` adds
`apply_failures_for_nomination(plan_id, nomination_id) ->
Vec<ApplyFailureForProposer>`. Joins `fetch_run_outcomes` ⨝
`recipes` on `recipe_id`; filters to `outcome_kind = 'failed' AND
failure_stage = 'apply'`; matches the nomination via
`recipes.dedup_key LIKE '{plan_id}:{nomination_id}:%'`. Dedupes by
`source_url`, oldest-first.

`crates/pipeline/src/fetch_executor.rs::author_for_nomination`
seeds `prior_attempts` from this query before the retry loop. Each
seed entry renders as `recipe authored but apply failed: <stage> ·
<message head>` — head-truncated at 120 chars, suffix ellipsis when
truncated. Until Piece B catches every shape bug (it won't —
selector behaviour against unseen bytes is unbounded), these
apply-stage failures need to be visible to run N+1's proposer.

Tests pin: apply-failure surfaces on matching nomination; per-target
declines (different bucket/index siblings) join correctly; dedupes
by URL keeping most recent; filters out fetch and insert stages;
orders oldest-first.

### Piece D — numeric-format normalizer in recipe-apply

`crates/pipeline/src/recipe_apply.rs::parse_extracted_scalar`
extended via new `normalize_numeric_candidate` and
`strip_thousand_separator_commas` helpers. Bounded strip order:
EU-locale gate → estimate prefixes (`est. `, `est `, `~`, `≈`,
careful `e`-matcher) → currency markers (`$ € £ ¥ USD EUR`) →
internal whitespace → ASCII thousand-separator commas (only at
canonical positions) → trailing-unit fallback (preserves the
pre-Session-53 contract for `49,000 t` and `12.5%`). Scientific
notation (`1.5e9`) is preserved because direct `f64::parse`
short-circuits before the normalizer fires.

Tests pin nine fixture cases: `74,700` → `74700`; `$1,234.56` →
`1234.56`; `est. 1,200` → `1200`; `~5000` → `5000`; `1.5e9` →
`1500000000`; `1.234,56` → `String("1.234,56")` (EU-locale, refused);
`abc` → `String("abc")`; malformed comma positions fall through to
conservative leading-prefix; trailing-unit shapes preserved.

### Piece E — UI polish

**E.1**: `apps/desktop/src/components/HostBackoffStatus.svelte`.
Head reads `host backoff · 1 host · this session` (visible separator
between label and count fixes the "BACKOFF1" wedge). Status dot
replaces the text `state` column, using `--signal-positive` /
`--signal-warning` / `--signal-negative` tokens — same dot
vocabulary as FetchReport row borders. `wait: <N>s` token only
renders when an active backoff is counting down.

**E.2**: `apps/desktop/src/components/PlanReview.svelte`.
`<header class="head">` is now `position: sticky; top: 0` with
`background: var(--bg-panel)` and `z-index: 1`, padded flush against
the `.review` container's gutter. Topic + accept/reject + run-fetch
controls stay visible across the whole scroll surface so the operator
doesn't lose orientation reading the bottom of the bucket grid.

CSS-only on both components. No prop changes, no new DTOs.

### Piece F — reasoning_effort escalation for stuck nominations

`crates/storage/src/fetch_run_outcomes.rs` adds
`decline_count_for_nomination(plan_id, nomination_id) -> usize`.
Counts `Declined` outcomes whose `source_id LIKE 'nom:{nom}%'` —
covers both nomination-level and per-target decline shapes.

`crates/pipeline/src/propose_source_url.rs::propose_source_url`
accepts a new `effort_override: Option<ReasoningEffort>` parameter,
threaded into the `CompletionRequest::reasoning_effort` field.

`crates/pipeline/src/fetch_executor.rs::author_for_nomination`
queries the decline count once before the retry loop and pins the
override to `Some(Medium)` when count ≥ 3. Logs one info-level line
per nomination naming the chosen effort tier so the operator's run
log shows the escalation decision. Escalation ceiling stops at
Medium (Workhorse); Frontier is reserved for deliberate
operator-driven re-runs.

Tests pin: zero count when no declines recorded; counts both
nomination-level and per-target shapes; filters by plan and
nomination; ignores non-Declined outcomes.

## Files touched

```
config/prompts/propose_source_url.md                   (v1.2)
crates/pipeline/src/propose_source_url.rs              (effort_override)
crates/pipeline/src/recipe_apply.rs                    (Pieces B, D)
crates/pipeline/src/recipe_author.rs                   (Piece B call site)
crates/pipeline/src/fetch_executor.rs                  (Pieces C, F)
crates/storage/src/fetch_run_outcomes.rs               (Pieces C, F)
crates/storage/src/lib.rs                              (re-export)
apps/desktop/src/components/HostBackoffStatus.svelte   (Piece E.1)
apps/desktop/src/components/PlanReview.svelte          (Piece E.2)
```

No new DTOs. No new IPC commands. No schema migrations. Existing
recipes (and the lithium plan's outcomes from the 2026-05-09 18:12
run) flow through the new code unchanged — Piece C reads existing
`fetch_run_outcomes` rows; Piece F reads existing `decline` rows;
Pieces B + D are pure code-path widening.

## Apply

Files were edited in place. To verify (operator runs cargo /
svelte-check on Mac per the cargo-on-Mac workflow):

```sh
cargo build 2>&1 | tee /tmp/situation_room-build.log
cargo test  2>&1 | tee /tmp/situation_room-test.log
( cd apps/desktop && pnpm check ) 2>&1 | tee /tmp/situation_room-check.log
```

Then live-test by re-running the same lithium classify+fetch on a
fresh DB and observing:

- **Piece A** — proposer rationales on auth-primary declines
  reference the "reasonable shot" principle on second/third attempts;
  news-host blocks pivot to a different news publisher.
- **Piece B** — `pubs.usgs.gov` and `www.worldbank.org` recipes
  surface as `RecipeOutcome::Declined` with "string in numeric slot"
  / "missing field" reasoning *at authoring time*, not as
  apply-stage failures.
- **Piece C** — second run on the same plan: proposer's prompt's
  `prior_attempts` block contains `recipe authored but apply failed:
  apply · …` entries from prior runs.
- **Piece D** — USGS MCS recipes that previously declined on
  comma-formatted-numbers grounds now author and apply, producing
  `74700` from `74,700`.
- **Piece E.1** — host-backoff strip reads `host backoff · 1 host
  · this session`; recovering hosts show no `wait: —` token; status
  dot reads consistently against FetchReport row borders.
- **Piece E.2** — the plan header stays pinned at the top of the
  review pane while the bucket grid scrolls underneath.
- **Piece F** — a nomination with ≥3 declines logs
  `propose-URL effort escalated for stuck nomination` on the next
  run; xAI provider receives `reasoning_effort: "medium"` in the
  body for that nomination's calls only.

End of patch.
