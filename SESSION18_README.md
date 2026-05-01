# Session 18 patch

Combined patch covering items 1–9 from the Session 18 TODO list
agreed at the top of the session:

1. Recipe-author prompt v1.4 → v1.5 (Endpoint discipline + Coverage discipline + EUR-Lex CELEX anti-example)
2. Recipe-author prompt v1.5 → v1.6 (Hunt the URL end-to-end + search-skeleton anti-example)
3. Failure-case writeup verification block split (v1.5 attempt / v1.6 attempt subsections)
4. ADR 0012 location normalization (`adr/` → `docs/adr/`)
5. Static_payload — schema + storage (migration 0008 + RecipeRow/StoredRecipe + 3 tests)
6. Static_payload — pipeline (FetchRecipe field + recipes_store threading + RecipeAuthoringOutput + build_validated_recipe validation + four-site fetch_executor short-circuit + sample-helper updates + 8 tests)
7. Static_payload — API + frontend (RecipeDto field + 2 tests + RecipeDto.ts + RecipesPanel BAKED badge + payload preview)
8. ADR 0007 Amendment 3 (~135 lines: design rationale, why option (b), validation discipline, code references)
9. Recipe-author prompt v1.6 → v1.7 (Strategy for PDF sources section + `static_payload` field in "What to produce" + v1.7 changelog)

## Apply

From the repo root:

    tar -xzf ~/Downloads/session18_patch.tar.gz --strip-components=1 -C .

Then **delete the now-empty top-level `adr/` directory** if it
exists in your tree (item 4 — the move from `adr/` to `docs/adr/`
leaves the source directory empty; the tarball cannot represent
deletions):

    rm -rf adr/

(Tarball-vs-deletion is the standard limitation. Confirming
`docs/adr/0012-reauthor-on-failure.md` exists post-apply is the
positive check.)

## Pre-flight gate

Per the project's standing pre-flight discipline:

    cargo test --workspace
    cargo test --workspace --doc
    cargo clippy --workspace --all-targets -- -D warnings
    cd apps/desktop && npm run check

Expected test count: **340** (327 baseline + 13 new):

- `+3` storage (`recipe_round_trips_with_no_static_payload`,
  `recipe_round_trips_with_static_payload`,
  `recipes_for_plan_carries_static_payload_through`)
- `+3` pipeline / recipes (`static_payload_is_optional_and_omits_when_absent`,
  `fetch_recipe_with_static_payload_roundtrips`,
  `legacy_recipe_without_static_payload_field_deserializes`)
- `+4` pipeline / recipe_author (`build_validated_recipe_collapses_empty_static_payload_to_none`,
  `build_validated_recipe_collapses_whitespace_static_payload_to_none`,
  `build_validated_recipe_accepts_well_formed_static_payload`,
  `build_validated_recipe_rejects_non_empty_static_payload_that_is_not_json`)
- `+1` pipeline / fetch_executor (`run_fetch_for_plan_uses_static_payload_without_calling_http`)
- `+2` api (`recipe_dto_static_payload_is_none_when_absent`,
  `recipe_dto_static_payload_round_trips_from_stored`)

(Existing tests' `RecipeRow` / `StoredRecipe` / `FetchRecipe` /
`RecipeAuthoringOutput` literals were updated to include the new
field with `None` / empty-string defaults; no test count loss
expected.)

The `RecipeDto.ts` is hand-written to match the format ts-rs
emits. On first `cargo test --package situation_room-api`, ts-rs
will overwrite it; the regenerated file is canonical.

## Compiler verification

This patch was **not** compiler-verified in the sandbox (no Rust
toolchain available). The most likely failure modes, by
likelihood:

1. **`unused_imports` from `serde_json::Value`** in
   `recipe_author.rs` — the validation step calls
   `serde_json::from_str::<Value>(trimmed)` and `Value` is
   already imported at the top of the file (`use serde_json::Value;`).
   Verified by `grep`. No risk expected, but if clippy flags an
   unused import elsewhere from the changes, the fix is local.

2. **`row.get(10)` on `Option<String>`** in
   `crates/storage/src/recipes.rs::row_to_stored` — relies on
   duckdb's `FromSql` impl for `Option<T>` mapping NULL to None.
   Pattern matches the rest of the storage crate (see
   `research_plans.rs::rejection_reason: Option<String>` reading
   via `row.get`). No risk expected.

3. **`recipe.static_payload.as_ref()` short-circuit shape** in
   the four `fetch_executor::run_X_recipe` functions — borrows
   the `Option<String>` and yields `Option<&String>`, which
   `as_bytes().to_vec()` is called on. Standard Rust; works.

4. **TS `static_payload: string | null`** — the hand-written
   `RecipeDto.ts` matches ts-rs's `Option<String>` output
   convention. Verified against the existing `dedup_key:
   string | null` shape on the same DTO.

If the build flags something I haven't anticipated, the patch's
file boundaries are clean enough that the localized fix-up
should be one or two lines.

## Files changed (16)

    config/prompts/recipe_author.md                                          v1.4 → v1.7 (3 changelog entries, ~340 new lines)
    apps/desktop/failure_cases/recipe_author/
      2026-05-01-eur-lex-celex-instance-naive-selector.md                    Verification block split, Status header updated
    migrations/0008_recipes_static_payload.sql                               NEW (nullable column, full DuckDB-ALTER discipline comment)
    crates/storage/src/migrate.rs                                            register migration 8
    crates/storage/src/recipes.rs                                            RecipeRow.static_payload + StoredRecipe.static_payload + INSERT/SELECT SQL + row_to_stored col 10 + 3 tests
    crates/pipeline/src/recipes.rs                                           FetchRecipe.static_payload (Option<String>, serde-default + skip-if-none) + 3 tests
    crates/pipeline/src/recipes_store.rs                                     recipe_to_row / stored_to_recipe thread the field; sample() updated
    crates/pipeline/src/recipe_author.rs                                     RecipeAuthoringOutput.static_payload (empty-string-as-absent) + build_validated_recipe step 6 (collapse + JSON-parse-validate) + 4 tests
    crates/pipeline/src/recipe_apply.rs                                      sample helper updated; apply() does NOT branch
    crates/pipeline/src/normalize.rs                                         sample helper updated
    crates/pipeline/src/fetch_executor.rs                                    short-circuit at all 4 run_X_recipe sites (inlined per Session 9 rule) + 6 fixture-helper updates + 1 new test
    crates/api/src/types_export.rs                                           RecipeDto.static_payload + from_stored threading + 2 new tests + 2 existing-test StoredRecipe fixture updates
    apps/desktop/src/lib/api/types/RecipeDto.ts                              hand-mirror of ts-rs output (will be regenerated)
    apps/desktop/src/components/RecipesPanel.svelte                          BAKED badge in recipe head + collapsible payload preview block + module-doc extension + CSS using --signal-warning canonical var
    docs/adr/0007-research-function.md                                       Amendment 3 appended (~135 lines)
    docs/adr/0012-reauthor-on-failure.md                                     MOVED from adr/0012-reauthor-on-failure.md (delete adr/ directory after apply)

## ADR cross-references

This patch affects the following architectural commitments. None
are violated; one is extended.

- **ADR 0003 (six record types)** — unchanged.
- **ADR 0007 (research function: two-level LLM architecture)** —
  *extended* via Amendment 3. The closed extraction-mode enum
  stays at five. The new field is recipe-level, orthogonal to
  extraction. The runtime stays LLM-free; baked bytes are
  produced at *authoring* time by the LLM and stored verbatim,
  served deterministically at refresh time.
- **ADR 0009 (security posture)** — unchanged. No new HTTP
  callers; the static_payload short-circuit *removes* one HTTP
  call per baked recipe. `SecureHttpClient` discipline holds.
- **ADR 0010 (topic-based subjects)** — unchanged.
- **ADR 0011 (plan lifecycle and fetch executor)** — unchanged.
  The fetch executor's contract (accepted plans only, audit row
  per run, deterministic apply) holds; the byte-acquisition step
  gains one branch, and the rest of the pipeline is identical.
- **ADR 0012 (re-author on failure)** — unchanged. *Location*
  moved from `adr/` to `docs/adr/` to align with the rest of the
  ADR series. No content change; the gate conditions still aren't
  met (item 4 is housekeeping, not policy).

## Discipline notes

- Per Session 9's rule, the four `run_X_recipe` short-circuits
  are duplicated-with-comments rather than extracted to a
  helper. Each carries an ADR cross-reference. Future session:
  if a unification pass is justified, the helper should not
  obscure the dispatch contract from `run_one_recipe`.
- `apply()` does **not** branch on `static_payload`. The
  executor decides byte provenance before calling apply. ADR
  0007 Amendment 3 §"bytes' provenance is orthogonal to
  extraction mode" — load-bearing for the `pdf_table`-or-not
  story to stay coherent.
- The wire form for `static_payload` on
  `RecipeAuthoringOutput` is `String` with empty-string-as-
  absent. xAI's structured-output schema rejects top-level
  `Option<T>` for some shapes; the same idiom is used elsewhere
  in the authoring path (`unit_hint`, `assertion_guidance`,
  `display`).
- The `RecipesPanel.svelte` BAKED badge uses
  `var(--signal-warning, var(--fg-secondary))` with explicit
  fallback — verified `--signal-warning` is defined in
  `apps/desktop/src/lib/design/global.css:26`. ADR 0006 §"color
  is meaning, not decoration" honored: warning tone because
  baked recipes deserve attention without being bad.

## Transparency note — concurrent external work observed

While this patch was being authored against the clean Session 16
tree (`/home/claude/stockpile/code` extracted from
`clean_code.zip`, baseline 327 tests green), files in that tree
were observed to receive external edits mid-session — a
parallel `0008_recipes_static_payload.sql` and modifications to
five other files appeared at timestamps after the work tree was
forked, with content that looks like a separately-authored
Session 17 implementation.

This patch is built **only from the work tree** (`session18`
implementation authored against the clean Session 16 baseline
during this session). The external concurrent implementation in
`code/` was not used or merged. If your local working tree
already carries those external edits, **this patch will conflict
on the affected files** — they are: `migrations/0008_recipes_static_payload.sql`,
`crates/storage/src/migrate.rs`, `crates/storage/src/recipes.rs`,
`crates/pipeline/src/recipes.rs`, `crates/pipeline/src/recipes_store.rs`,
`config/prompts/recipe_author.md`,
`apps/desktop/failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`.

If your tree is at the clean 327-test baseline (no Session 17
work landed), the patch applies cleanly. If you've already
applied a parallel Session 17 patch, **review whether to take
the changes here over yours, or vice versa** — both targets are
ADR 0007 Amendment 3, and the implementations may diverge in
naming, comment density, and test set even where the
architectural shape agrees.

## Items NOT in this patch (deferred per session scope)

- Endpoint_hint coverage sweep on `config/sources.toml` (7 of 12
  sources still without an `endpoint_hint`). Per Session 16's P2
  description, this is reading source documentation — better as
  a user-driven sweep with pairing than an LLM session.
- Per-recipe rejection feedback (Session 17 handoff P3). Gated
  on observing the static_payload path live first.
- `pdf_table` removal decision (Session 17 handoff P4). Per the
  handoff's own gate condition: "defer until at least one full
  session goes by where the LLM, given v1.7's strategy, never
  authors a `pdf_table` recipe."
- Per-expectation SatisfactionPanel (Session 16 handoff P4 /
  Session 17 imperfection). Independent UI work.
- Verification of v1.6 prompt against the C.2 plan
  ("EU AI Act high-risk system enforcement timeline"). The
  failure-case writeup is set up to record the result; the run
  itself is the user's first move post-apply.
- Repo-root cleanup of accumulated `SESSION*` patch READMEs and
  `*.broken-2026-05-01` DB files. Low-priority; risk of deleting
  something the user has reason to keep. Flagged for a deliberate
  housekeeping pass.
