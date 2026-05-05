# STOCKPILE — Session 26 handoff

You are starting Session 26.

Session 25 landed Track D end-to-end: `SecureHttpClient` now exposes
response headers through a bounded-accessor wrapper, the executor
honors `Retry-After` on 429 responses with a short-window inline
backoff, and the UI surfaces rate-limited outcomes in warning amber
distinct from broken-recipe red. ADR 0009 has an Updates section
documenting the response-side boundary; tests are in for the header
parser, the leak-prevention assertion, and the backoff policy paths.

**Tracks A, B, and C from the Session-25 handoff are NOT yet done.**
They should be the focus of Session 26.

## What landed in Session 25

### Track D (complete)

- **`crates/secure/src/headers.rs`** (new) — `SecureHeaderMap`
  newtype wrapping `reqwest::HeaderMap` with an allow-list of
  accessors. Crate-private constructor; custom `Debug` impl prints
  names but not values; tests assert no public accessor returns
  `Authorization`/`Cookie`/`Set-Cookie`/`x-api-key` values when
  those headers are present.
- **`crates/secure/src/http.rs`** (modified) — added
  `SecureHttpResponse { status, headers, body }`, the
  `*_with_headers` methods, and `HttpError::StatusWithHeaders`. The
  legacy `get_bytes` / `post_json_bytes` / `get_json` / `post_json`
  methods keep their signatures and behaviour exactly; existing
  callers unchanged.
- **`crates/secure/src/lib.rs`** (modified) — exposes the new
  module, re-exports `SecureHeaderMap` and `SecureHttpResponse`.
- **`crates/secure/Cargo.toml`** (modified) — adds workspace
  `chrono` dep for the `Retry-After` HTTP-date parser.
- **`crates/pipeline/src/http_fetcher.rs`** (modified) —
  `FetchError::RateLimited { retry_after_seconds: Option<u64> }`
  variant; `From<HttpError>` lifts 429-with-headers into it; the
  blanket impl on `SecureHttpClient` routes through
  `get_with_headers` so the value reaches the executor.
- **`crates/pipeline/src/fetch_backoff.rs`** (new) —
  `fetch_with_backoff(http, url, context)` + `BackoffOutcome` enum
  + `format_retry_after`. Three-case policy: ≤60s sleep+retry once,
  >60s surface, absent surface. Pulled out so the policy lives in
  one place; each `run_X_recipe` retains its own visible call
  site.
- **`crates/pipeline/src/fetch_executor.rs`** (modified) — added
  `RecipeOutcome::RateLimited { recipe_id, source_id,
  retry_after_seconds }` and a `fetch_recipe_bytes` helper that
  consolidates the four-way duplicated fetch arm. `prefetch_excerpt`
  routes through the same backoff helper. Run-loop counter logic
  treats `RateLimited` as its own category — not a success, not a
  failure-stage.
- **`crates/api/src/types_export.rs`** (modified) — added
  `RecipeOutcomeDto::RateLimited` variant and the `From` arm.
  Extended the per-variant serialization test to cover both `Some`
  and `None` retry-after values.
- **`apps/desktop/src/lib/api/types/RecipeOutcomeDto.ts`**
  (regenerated) — matches what `cargo test -p situation_room-api`
  will emit. The first thing to do this session is run that test
  and confirm the shape converges.
- **`apps/desktop/src/lib/outcomes.ts`** (modified) — added
  `'limited'` tone, `formatRetryAfter` helper. Mirror of the Rust
  `format_duration` so log lines and UI copy match.
- **`apps/desktop/src/components/FetchReport.svelte`** (modified)
  — added `data-tone="limited"` styling using
  `var(--signal-warning)`.
- **`docs/adr/0009-security-posture.md`** (modified) — appended an
  `## Updates` section: 2026-05-03 entry documenting the
  `SecureHeaderMap` newtype, "the rule extended" ("no path by
  which `reqwest`'s primitives leak past the secure boundary"),
  and the code references.

### Tests added

- 14 in `secure::headers::tests` (parser cases, leak-prevention
  assertions, accessor pass-through).
- 7 in `pipeline::fetch_backoff::tests` (formatter cases, policy
  paths through `fetch_with_backoff`).
- 1 extension to `api::types_export::tests::recipe_outcome_dto_serializes_with_kind_tag_per_variant`
  covering both `Some(120)` and `None` retry-after shapes.

Net: +22 tests landing at ~424 from the Session 24 baseline of
402. The actual count is what `just check` says; this is sizing
not a target.

## First thing to do in Session 26

1. Read this file. Read the Session-25 handoff still — Tracks A, B,
   C are quoted there verbatim; nothing about them is stale.
2. Run `just check`. Confirm the 424ish-test count and that
   nothing regressed in the existing 402.
3. Run `cargo test -p situation_room-api` so ts-rs regenerates the
   TS files. The hand-written `RecipeOutcomeDto.ts` in the patch
   should converge with the regenerated form to the byte; if it
   doesn't, the regenerated form wins (it's the canonical) and
   the only adjustment is updating the frontend imports if a
   field name moved (none should have).
4. Run a live fetch against a known-rate-limited source to verify
   the end-to-end path. GDELT has been the recurring trigger;
   classify a "global protests last 24 hours" topic, accept it,
   run fetch. Expect to see either the inline-retry succeed
   silently (logs show "rate-limit retry succeeded") or the
   `RateLimited` outcome surface with the warning-amber treatment.
5. Pick A as the next track. Manual re-author UI; Session-25
   handoff §"Track A" describes the storage migration, the
   command surface, the dialog component, and the lineage chip in
   detail.

## Track ordering reminder (carry-over from Session 25)

Original recommendation: D → A → B → C. D is now done, so the
remaining order is **A → B → C**.

- **A** depends on the storage and command surface that already
  exists; mostly frontend + one new command + one migration. ~1
  session.
- **B** is a prompt revision; needs A's re-author flow to validate
  against. ~½–1 session.
- **C** (`pdf_table`) is the most self-contained. Lands in
  parallel or last. ~1–1½ sessions.

Total remaining: ~3 working sessions of compressed work.

## What Session 26 is explicitly NOT (carry-over)

Same list as Session 25:

- No multi-tenant / multi-user.
- No renegotiating ADR 0012's 10-shape gate downward. Track A
  raises evidence-capture velocity; the gate stays at 10.
- No second LLM provider beyond xAI/Anthropic.
- No PDF-table approach (b) (Tabula-style dep). Track C v1 is the
  layout heuristic.

## Hard rules that DO still apply

Carried over verbatim from Session 25 — these are the safety rails
nothing about Track D changes:

- ADR 0009 §"The rule" extended in Session 25: no fresh
  `reqwest::Client::new()`, AND no path by which `reqwest`'s
  primitives leak past the secure boundary. The `SecureHeaderMap`
  newtype is the canonical surface for response headers; the raw
  `HeaderMap` never crosses the boundary. Adding a new accessor is
  a deliberate review step.
- API keys never read, written, logged, or echoed. The
  `SecureHeaderMap` accessor allow-list specifically excludes
  `Authorization`, `Cookie`, `Set-Cookie`, `x-api-key`. The
  `forbidden_header_values_never_appear_in_any_accessor_output`
  test is the regression backstop.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError` — extend with new variants
  for new failure shapes.
- Two mandatory edits for every new `#[tauri::command]` (define +
  register).
- Generated TS files are written by ts-rs; never hand-edit. The
  hand-written `RecipeOutcomeDto.ts` in this patch is a
  best-effort match for what ts-rs will emit; running the api test
  is the canonical regeneration step.
- DTOs and pipeline structs are mirrored, not shared.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`.
- Migrations: read the prior migration's comment block.

Standing-order priority: **security > generalisation > simplicity**.

## Continuity note

The operator is rigorous about security, prefers honesty over false
confidence, and reacts well to direct disagreement. Session 25's
handoff explicitly authorized the four tracks with PRO MAX budget;
Session 26's mandate is the same — A and B and C, not deferred,
not chunked. If you find yourself writing "I'll just do A and
stop," stop and re-read Session 25's §"Why one big session."

End of Session 26 handoff.
