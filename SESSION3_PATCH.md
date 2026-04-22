# Session 3 patch — ADR-0010 cleanup + xAI provider (3c.1) + Recipe authoring (3c.2)

Extract this archive at your workspace root:

```bash
cd /Users/aben/RustroverProjects/stockpile
tar -xvf ~/Downloads/stockpile_session3_patch.tar.gz
```

All paths inside the archive are relative to the workspace root, so
`tar -x` will drop each file into the right spot and overwrite the
previous version where applicable.

## Files changed / added

### ADR-0010 cleanup (schema drift: commodity → topic)

- `crates/sources/src/traits.rs`
  `AuthoritativeDomain.commodity: Option<String>` → `topic: Option<Topic>`
- `crates/sources/src/adapters/usgs/mod.rs`
  Both call sites updated to the new field name.
- `config/vocab/authoritative_sources.toml`
  Header comment + `commodity = "Cu"` → `topic = "Cu"` to stay in sync
  with the Rust struct (the file isn't loaded by code today but will
  be when the promotion pipeline ships, and silent drift is worse
  than a noisy rename).

### 3c.1 — xAI provider

- `Cargo.toml`
  Added `dotenvy = "0.15"` to workspace deps — for test harness /
  binary use only. Libraries never load `.env`.
- `crates/secure/src/http.rs`
  Added `post_json_bytes` and `post_json` methods to
  `SecureHttpClient`. Same guards as GET (URL guard, literal-IP
  check, bounded response, timeout classification). Auth headers
  take `&SecretString` so `expose_secret()` is called at exactly
  one reviewable site, and reqwest marks the HeaderValue
  `.sensitive(true)` so its own logs redact it.
- `crates/llm/Cargo.toml`
  Added `dotenvy` as a dev-dep (test-only).
- `crates/llm/src/providers/grok.rs`
  Stub → full `XaiProvider`:
  - OpenAI-compatible chat completions via `SecureHttpClient` (no
    second HTTP client).
  - Configurable tier → model mapping via `XaiConfig`. Defaults set
    to the 2026-04-22 catalog you confirmed:
    - Frontier: `grok-4.20-0309-reasoning`
    - Workhorse / Cheap: `grok-4-1-fast-reasoning`
  - Structured output via `response_format: { type: "json_schema", ... }`.
  - Bounded prompts via `Bounds::LLM_PROMPT_BODY`.
  - 11 tests (9 unit + 2 `#[ignore]` live).
- `crates/llm/src/providers/mod.rs`, `crates/llm/src/lib.rs`
  Re-export `XaiProvider`, `XaiConfig`, `XAI_API_KEY_ENV`.

### 3c.2 — Recipe authoring

- `crates/pipeline/Cargo.toml`
  Added `schemars` and `stockpile-secure` to deps, `dotenvy` to
  dev-deps.
- `crates/pipeline/src/lib.rs`
  Registered new `recipe_author` module.
- `crates/pipeline/src/recipe_author.rs` (new)
  `author_recipe(&dyn LlmProvider, tier, template, plan, ctx)`:
  - Assembles the user prompt via `build_prompt()` (pure, testable).
  - Sends a structured-output request; the LLM schema is derived
    via `schemars` from a parallel `RecipeAuthoringOutput` type
    that mirrors the subset of `FetchRecipe` the LLM is allowed
    to fill in. Server fills in `id`, `authored_at`, `version`.
  - Validates the returned URL through `UrlGuard`, bounds-checks
    expectation indices against the plan, enforces structural
    limits (`MAX_BINDINGS = 20`, `MAX_FIELD_MAPPINGS_PER_BINDING = 50`),
    per-variant sanity (empty selector, `pdf_table` page-0, regex
    group-0, etc.), and rejects duplicate-expectation bindings.
  - ~20 tests including `mirror_matches_runtime` canaries that
    guard against the authoring shape drifting from `ExtractionSpec`.
  - One `#[ignore]` live xAI test that asserts *structurally*
    (≥1 binding, version=1, authored_by="xai") — not exact-match,
    per the Session 2 handoff's warning about LLM determinism.
- `config/prompts/recipe_author.md` (new)
  Versioned markdown prompt. Dev-editable without recompile.

## Running the tests

All existing 66 tests should stay green. New tests:

```bash
cargo check --workspace
cargo test -p stockpile-sources   # ADR-0010 ripple check
cargo test -p stockpile-secure    # post_json additions
cargo test -p stockpile-llm       # ~11 new unit tests
cargo test -p stockpile-pipeline  # ~20 new recipe_author tests
```

Live xAI tests (read `XAI_API_KEY` automatically from `.env` — you
don't have to paste it anywhere):

```bash
cargo test -p stockpile-llm --ignored live_xai
cargo test -p stockpile-pipeline --ignored live_author
```

## Notes for the next session

- Crate-level `#![allow(dead_code, unused_imports, unused_variables)]`
  still present in `crates/llm/src/lib.rs` and `crates/pipeline/src/lib.rs`.
  Was appropriate for Phase 1 stubs; will hide real warnings now that
  these crates have live code. Tightening this deserves its own
  narrow session once the remaining provider stubs are either live
  or deleted.
- `SecureHttpClient` still doesn't surface response headers. For xAI
  429s, `retry_after_seconds` is reported as 0 (= "unknown") and the
  router will need to apply its own backoff. Captured in
  `map_http_err` inside `grok.rs`.
- Anthropic / OpenAI / Gemini provider stubs left untouched per
  instruction — 3-line doc comments.
