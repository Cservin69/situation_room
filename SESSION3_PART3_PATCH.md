# Session 3 — Part 3 patch (3c.4 end-to-end demo)

Applies on top of the two fixes patches. Completes Phase 3c.4:
the full pipeline exercised end-to-end against a live source.

    cd /Users/aben/RustroverProjects/stockpile
    tar -xzvf ~/Downloads/stockpile_session3_part3_patch.tar.gz

## What this patch does

### Storage — recipes table (ADR 0007 artifact storage)

Recipes are the Level-2 output of the research function and are
stored alongside records but not *as* records (the six record
types are closed per ADR 0003). They live in their own table with
scalar columns for indexing and JSON blobs for the extraction spec
and bindings.

- migrations/0003_recipes.sql — the table. Indexes on dedup_key
  (for idempotent re-authoring lookups) and (plan_id, source_id)
  (the runtime's primary lookup path).
- crates/storage/src/migrate.rs — registers version 3.
- crates/storage/src/recipes.rs — insert_recipe, get_recipe,
  get_recipe_by_dedup_key (returns highest version when multiple
  exist), count_recipes. 5 unit tests.
- crates/storage/src/lib.rs — registers the module; re-exports
  RecipeRow and StoredRecipe.

Design note: storage stays record-centric. It accepts recipes as
pre-serialized JSON via the scalar RecipeRow shape, which means
storage does not reverse-depend on pipeline. The typed
FetchRecipe to RecipeRow marshalling lives in pipeline instead.

### Pipeline — typed recipe store helper

- crates/pipeline/src/recipes_store.rs — thin marshalling layer
  around storage. save_recipe, load_recipe,
  load_recipe_by_dedup_key. 2 roundtrip tests.
- crates/pipeline/src/lib.rs — registers recipes_store.

### The end-to-end demo

- apps/demo/Cargo.toml — adds a second bin target
  stockpile-e2e and the deps the new binary needs (pipeline, llm,
  serde_json, url, uuid, dotenvy).
- apps/demo/src/bin/e2e_demo.rs — the demo binary.

Source choice: World Bank Indicators API, Chile total population
2022. Auth-free, stable, structurally clean JSON. Maps naturally
to JsonPath extraction. Picked over USGS / MCS because
PdfTable extraction is explicitly NotImplemented in the
apply runtime today. The e2e demo has to use a supported path.
USGS demos wait for a focused session on positional PDF table
extraction.

The demo walks nine stages:

1. Open store plus migrate (applies 0001, 0002, 0003).
2. Build a hand-written ResearchPlan. Level-1 classification is
   its own future session; the demo labels the plan as a
   placeholder in the output.
3. Fetch a sample of the source through SecureHttpClient for
   the authoring excerpt.
4. Author a FetchRecipe via xAI. The --offline flag falls back
   to a hand-written recipe that exercises the same apply path.
5. Persist the recipe via recipes_store::save_recipe.
6. Fetch the source again. Deliberately a second call so the
   authoring path and the runtime path are visibly distinct.
7. recipe_apply::apply produces records deterministically,
   with zero LLM involvement.
8. Persist the records via store.insert_record.
9. Summarize: print the Observation with full provenance
   (world_bank_indicators and recipe id at v1), and the store's
   topic counts.

## Running

    cargo run -p stockpile-demo --bin stockpile-e2e
    cargo run -p stockpile-demo --bin stockpile-e2e -- --offline
    cargo run -p stockpile-demo --bin stockpile-e2e -- --db /tmp/stockpile-e2e.duckdb

Live path reads XAI_API_KEY from workspace .env. Offline path
uses a hand-written recipe and needs no LLM.

## Tests

    cargo test --workspace

Net new tests in this patch:
- 5 recipe storage tests (stockpile-storage::recipes)
- 2 typed recipe-store tests (stockpile-pipeline::recipes_store)

No new ignored live tests. The 3c.2 live xAI test already covers
the authoring path, and --offline gives deterministic coverage of
the full pipeline.

## Known loose ends for future sessions

- ResearchPlan has no id field. The demo generates a stand-in
  plan_id via Uuid::now_v7 so the recipe dedup_key rotates
  per run. Adding ResearchPlan::id: Uuid is a small core change
  that unblocks stable cross-run dedup.
- The demo fetches twice (authoring excerpt and apply). For
  production this is wasteful but kept visibly separate in the
  demo so the architecture is legible.
- PdfTable stays NotImplemented. The sibling stockpile-e2e-usgs
  demo will land when PDF positional extraction does.

## Files in this archive

    SESSION3_PART3_PATCH.md
    migrations/0003_recipes.sql
    crates/storage/src/migrate.rs
    crates/storage/src/recipes.rs
    crates/storage/src/lib.rs
    crates/pipeline/src/recipes_store.rs
    crates/pipeline/src/lib.rs
    apps/demo/Cargo.toml
    apps/demo/src/bin/e2e_demo.rs
