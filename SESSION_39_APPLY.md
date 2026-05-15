# Session 39 — apply instructions

Patch shape: tarball of files to overwrite. Apply from the repo root:

```
tar -xzf ~/Downloads/session39.tar.gz --strip-components=1 -C .
```

The `--strip-components=1` strips the leading `./` from the
archive entries; the directory layout inside the archive matches
the workspace exactly so files land at the right paths.

## What this patch does

Splits URL discovery out of the Level-1 classifier into a
dedicated Level-2 propose-URL step that runs inside a
per-nomination retry loop.

The L1 classifier now emits descriptions only (no URLs, no
`known_id`). For each accepted plan, the fetch executor:

1. Calls a new propose-URL LLM step (Cheap tier) to commit to a URL
   given the nomination's description and the prior-attempts history.
2. Fetches that URL through `SecureHttpClient`.
3. Hands the bytes to the recipe author (Workhorse tier) — same
   prompt as before.
4. On fetch failure or recipe-author decline, records the URL +
   reason and goes back to step 1 with the failure visible to the
   next propose-URL call.

Bounded by `MAX_AUTHORING_ATTEMPTS_PER_SOURCE = 3` and a per-source
deadline of 240 seconds. Whichever bound bites first surfaces the
nomination as `RecipeOutcome::Declined` with the full attempt
history baked into the reason.

## What's in the tarball

**Type-shape changes (compile-blocking)**
- `crates/pipeline/src/research.rs` — `DocumentSourceNomination` is
  now `{ nomination_id: Uuid, description: String, priority_tier:
  PriorityTier }`. URL fields removed.
- `crates/pipeline/src/research_classifier.rs` —
  `AuthoredDocumentSourceNomination` is `{ description,
  priority_tier }`. `convert_one_nomination` server-stamps
  `nomination_id` with `Uuid::now_v7()`. Drops `UrlGuard` import.
- `crates/pipeline/src/recipe_apply.rs`,
  `recipe_author.rs`, `research_plans_store.rs` — test fixture
  literals updated to new shape.

**New code**
- `crates/pipeline/src/propose_source_url.rs` — new module, ~480
  lines, 11 unit tests. `propose_source_url(provider, tier,
  prompt, plan, nomination, prior_attempts) -> ProposalOutcome`
  with `Url` or `Declined` variants. URL discipline via `UrlGuard`.
- `crates/pipeline/src/lib.rs` — registers the new module.
- `crates/pipeline/src/fetch_executor.rs` — `author_one` rewritten
  as the retry loop. `host_verifies_known_id` deleted.
  `derive_source_id_for_decline` rewritten to use
  `nomination_id`. `dedup_key` is now `{plan_id}:{nomination_id}`
  (stable across attempts and runs). All 30 `ExecutorContext` test
  fixtures gain `propose_url_prompt`. `MAX_AUTHORING_ATTEMPTS_PER_SOURCE`
  and `PER_SOURCE_DEADLINE_SECS` constants added.

**Wiring**
- `crates/api/src/commands.rs` — `AppState` gains
  `propose_url_prompt`. `AppState::new()` signature changed
  (one new positional parameter). `run_fetch_for_plan` command
  threads the field into `ExecutorContext`.
- `apps/desktop/src-tauri/src/main.rs` — loads
  `propose_source_url.md` via `include_str!`, passes the constant
  into `AppState::new()`.
- `apps/situation_room/src/main.rs` — test that asserted
  `CLASSIFIER_PROMPT` contains `{{SOURCES_MEMORY}}` updated; the
  v2.0 prompt no longer carries that placeholder.

**Prompts**
- `config/prompts/research_classifier.md` — bumped to v2.0. Drops
  the `{{SOURCES_MEMORY}}` placeholder block, drops URL fields
  from the schema and all three worked examples, rewrites the
  source-nomination section around description quality (publisher
  → dataset → shape). Changelog entry added.
- `config/prompts/propose_source_url.md` — new, v1.0. Loaded by
  the executor; consumed by `propose_source_url`.

**Frontend**
- `apps/desktop/src/components/PlanReview.svelte` — nomination
  rendering simplified: description as the primary line, priority
  tier as an info chip, nomination_id (8-char prefix) as the
  rationale line. URL chip and `known: ...` chip removed.
- `apps/desktop/src/lib/api/types/DocumentSourceNominationDto.ts`,
  `DocumentSourceEntryDto.ts` — regenerated for the new shape.
  ts-rs would emit these on first `cargo test --package
  situation_room_api`; bundled here so the frontend dev loop has
  the right types from the moment of extract.

## What it doesn't do

- No storage migration. `research_plans` stores plans as opaque
  JSON; the on-disk shape change rides through the existing
  `#[serde(untagged)]` `DocumentSourceEntry`. Plans classified
  pre-Session-39 (carrying `endpoint_url`) fall through to the
  `Legacy` variant and surface as `LegacyPlanCannotAuthor` on
  fetch — same path Session 37 used. To migrate an old plan:
  reject it, re-classify the same topic, accept the new plan.
- No ADR. We agreed offline that the architectural shape lands
  cleanly enough as a delta on ADR 0015 without a new ADR.
  ADR 0015 itself is still missing as a docs file (referenced by
  ADR 0016 and the classifier module); back-fill is its own
  small task, unrelated to this patch.

## Running it

```bash
cargo build --workspace
cargo test --workspace --lib

# Frontend
cd apps/desktop && pnpm install && pnpm dev   # (or npm)
```

For an empirical end-to-end check, classify the same "sustainable
tuna fishing" topic that produced 0/0/0 in the Session 38 live
test. The new flow will run propose-URL three times against each
of the 5 source descriptions (15 LLM calls plus authorings),
some of which should commit to fetchable URLs (FAOSTAT bulk
download paths, NOAA FOSS endpoints, WCPFC report PDFs, etc.)
rather than the agency-homepage-shaped URLs L1 picked before.

Watch fetch-run logs for:
- `URL proposed; pre-fetching` — propose-URL committed
- `recipe author declined this URL; will retry with different URL`
  — first or second URL got past propose but recipe author saw a
  page it couldn't extract from
- `recipe authored from retry-loop attempt` — happy path
- `exhausted N attempts without producing a recipe` — surfacing as
  declined with the full URL history in the reason

If most sources still decline through 3 attempts, the propose-URL
prompt's anti-pattern guidance needs tightening — that's an
empirical revision based on what URLs the model actually picked,
same discipline as v1.6 → v2.0 was for the L1 prompt.

## What I'd expect to fail on first build

This patch was assembled without a Rust toolchain available, so
the first `cargo build` will surface anything I missed. Likely
candidates, in rough order of probability:

- A test file in `crates/storage` or one of the unread crates
  that builds a `DocumentSourceNomination` literal I didn't find.
  Search: `grep -rn "DocumentSourceNomination\\s*{" --include='*.rs'`
  and update each literal to drop `endpoint_url` / `known_id` and
  add `nomination_id: Uuid::now_v7()`.
- An import I missed that refers to `host_verifies_known_id` or
  `derive_effective_source_id` (both deleted).
- The fetch-executor tests that hand-assemble recipes for
  pre-authored happy-path checks may need their `dedup_key`
  field updated to use a `nomination_id`-shaped key rather than a
  `host`-shaped one. Storage round-trip should still work with
  any string, but the executor's "load latest by dedup_key" path
  may not match if a fixture's recipe has a stale shape.
- `apps/desktop/src/components/PlanReview.svelte` may have other
  call sites (in `aside` snippets etc.) that read `s.endpoint_url`
  — I only updated the main render block. Search the file for
  `endpoint_url` / `known_id` and remove.

For each, the fix is mechanical. Push the build output back and
I'll either turn the next patch or apply directly if it's a
single line.
