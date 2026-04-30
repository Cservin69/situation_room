# Stockpile Session 11 Handoff

You are picking up after Session 10. Read this and ADR 0007's
"runtime path" + "Level 2" sections first. ADR 0011 still governs
the lifecycle gate. The Session 10 production-run findings in
`STOCKPILE_HANDOFF_SESSION10.md` are the diagnosis Session 10's
patch responds to; treat them as load-bearing context, not history.

## What Session 10 shipped — Option F (pre-fetch + endpoint_hint)

Session 10's job was to fix the "0 of 3 recipes produced a record"
bottleneck the production run on "bulgaria elections 2026" exposed.
The diagnosis was: the executor passed
`https://example.invalid/{source_id}` as both the sample URL and
a stub excerpt to Level-2 authoring, and the LLM faithfully echoed
the placeholder back into the recipe.

The fix is structural, not heuristic:

1. **`SourceDescriptor::endpoint_hint: Option<String>`** —
   `crates/pipeline/src/research_classifier.rs`. A new field on the
   classifier-side descriptor type, threaded through the existing
   `Vec<SourceDescriptor>` that the binary already loads from
   `config/sources.toml` and stuffs into `AppState`. Invisible to
   the classifier prompt by design — it's an executor-side concern.

2. **`ExecutorContext::sources: &[SourceDescriptor]`** —
   `crates/pipeline/src/fetch_executor.rs`. The executor now sees
   the descriptor list and uses it during `author_one` to look up
   the bound source's `endpoint_hint`.

3. **Pre-fetch in `author_one`** — same file. When the bound
   source has an `endpoint_hint`, the executor:
   - Parses the hint as a URL.
   - Calls `ctx.http.fetch_bytes(...)` (the same `HttpFetcher` the
     runtime uses for recipe execution — one client, ADR 0009).
   - Truncates the body at `PREFETCH_EXCERPT_BUDGET = 32 KiB`,
     UTF-8 lossy, with an explicit truncation marker.
   - Passes the real URL as `AuthoringContext::sample_url` and the
     prefetched bytes as `AuthoringContext::document_excerpt`.

   The fallback discipline is conservative. Any of *no descriptor*,
   *no endpoint_hint*, *unparseable endpoint_hint*, or *fetch
   failure* degrades cleanly to the pre-Session-10 behaviour
   (placeholder URL + stub excerpt) with a logged warning. When the
   pre-fetch fails but the URL is real, the stub still surfaces
   the URL as `Documented endpoint (pre-fetch failed; ...)` so the
   LLM has a real target.

4. **Recipe-author prompt v1.3** —
   `config/prompts/recipe_author.md`. New "URL discipline" section
   tells the LLM that `example.invalid` is a placeholder it must
   replace; same-host derivations of a documented endpoint are
   fine; synthetic hosts are forbidden. The output contract is
   unchanged (same schema) so existing authored recipes don't need
   re-authoring.

5. **`config/sources.toml`** — `endpoint_hint` added to five
   entries: `world_bank_indicators`, `gdelt`, `eur_lex`, `csv_demo`,
   `json_demo`. The schema docstring at the top of the file now
   documents the field. Sources without a hint (USGS MCS, SEC
   EDGAR, OFAC, IMF WEO, LME, Comtrade, RSS) work as before — the
   placeholder fallback path is still in place, no entry was
   forced to declare a hint preemptively.

6. **TOML loaders** — both `apps/desktop/src-tauri/src/main.rs`
   and `apps/situation_room/src/main.rs` parse `endpoint_hint` with
   `#[serde(default)]` and normalise whitespace-only strings to
   `None` so the executor's lookup path doesn't see a useless empty
   string.

7. **Cosmetic** — boot-banner string drift fixed
   (`apps/desktop/src-tauri/src/main.rs`: "Stockpile desktop boots."
   instead of "Stockpile desktop boots (Session 6 — GUI).").

### Tests added

- `crates/pipeline/src/fetch_executor.rs` — six new offline tests
  driving `author_one` indirectly through `run_fetch_for_plan` with
  a `RecordingProvider` test double that captures the prompt:
  - `author_one_uses_endpoint_hint_url_and_prefetched_excerpt` —
    happy path. Asserts the prompt contains the real hint URL, no
    `example.invalid`, and the prefetched body bytes.
  - `author_one_falls_back_to_placeholder_when_no_endpoint_hint` —
    descriptor present, hint absent. Placeholder URL appears; stub
    excerpt is taken.
  - `author_one_falls_back_when_descriptor_absent` — empty
    `sources` slice. Same fallback as missing hint.
  - `author_one_falls_back_when_prefetch_fails` — hint URL not in
    fetcher's fixture map. Real URL still in the prompt; stub
    excerpt with "Documented endpoint" marker.
  - `author_one_falls_back_when_endpoint_hint_unparseable` —
    `endpoint_hint = "not a url at all"`. Logged warn, placeholder
    fallback.
  - `author_one_truncates_oversized_prefetch_excerpt` — body
    larger than `PREFETCH_EXCERPT_BUDGET`. Asserts prefix survives,
    suffix doesn't, truncation marker present.

- `apps/situation_room/src/main.rs` — three new TOML loader tests:
  parses `endpoint_hint` when present, normalises empty strings to
  `None`, defaults to `None` when absent.

## Build / test state

**Not verified.** Session 10 was conducted in a sandbox without
`cargo` and without network — same constraint Session 6 hit. Every
file was type-checked-by-eyeball, brace-balance was scripted-checked,
and all 16 `ExecutorContext { ... }` literal sites were
mass-updated to include `sources` and verified to share a 5-field
shape. But none of it has been compiled.

Treat the first thing in Session 11 as **run the build and fix
what the compiler flags**. Session 6's continuation pattern
applies here too. The failure modes I'd expect to surface, in
rough order of likelihood:

1. **Borrow-checker on the `(u.clone(), Some(u))` pattern in
   `author_one`.** When the parsed `Url` is moved into both the
   tuple's first slot (clone) and second slot (move), Rust may
   object even with the explicit clone if the match arm's binding
   lifetime is doing something I didn't anticipate. The fix is
   `(u.clone(), Some(u.clone()))` or rebind via `let parsed = ...`
   first.

2. **`async_trait` macro hygiene on `RecordingProvider`.** The
   test double uses `#[async_trait]` and a `std::sync::Mutex<Vec<_>>`.
   The mutex is held only across synchronous code (push/pop) so it
   doesn't span an `.await`, which is the usual way std-mutex
   trips async tests. If clippy or the borrow checker objects, the
   fix is to switch to `tokio::sync::Mutex`.

3. **`PREFETCH_EXCERPT_BUDGET` constant scope.** I declared it at
   module level (above `author_one`) and reference it from the
   test module via `super::*;`. Should resolve, but if not, the
   fix is `pub(super) const ...` or qualifying as
   `super::PREFETCH_EXCERPT_BUDGET` in tests.

4. **`StaticFetcher` byte fixture for the truncation test.** The
   test uses `Vec<u8>` of size ~64 KiB. `StaticFetcher::with` takes
   `&[u8]`; passing `&body` should coerce. If not,
   `body.as_slice()`.

5. **The desktop binary's Tauri `invoke_handler!` macro** does not
   change — `run_fetch_for_plan` is still listed there — but the
   underlying `AppState.sources` field's type is unchanged
   (`Vec<PipelineSourceDescriptor>`), so I expect no friction at
   the IPC boundary.

The four ignored live tests (CSV, JSON, recipe author, classifier)
should still pass — none of them touch `author_one` (they all
pre-author the recipe) — but
`live_classify_topic_against_xai_produces_valid_plan` was already
flaky in Session 9 per Session 10's "known intermittent" note.

## What Session 11 should do

The recommendation has not changed from Session 10's "F first, E
second." F is now landed (modulo build verification); **E is next**.

### P1 — Run the build, fix what the compiler flags

Mechanical. `cargo check --workspace`, then `cargo test --workspace`,
then `cargo clippy --workspace --all-targets -- -D warnings`. If a
test fails, read the failure and fix it; the structural plan in this
document is correct, but type-level details may need a tweak. Don't
move on to E until this is green.

### P2 — Promote CssSelect (Option E)

Same shape as Session 9's JSON work. Flip the `ExtractionSpec::CssSelect`
arm in `run_one_recipe` from `Skipped` to a real `run_css_recipe`,
mirror `run_json_recipe`'s structure, mirror its tests. The
`recipe_apply` runtime already handles CssSelect (it's the third
of the five modes; only `PdfTable` and `RegexCapture` remain
unwired in apply). Replace the `_skips_unwired_extraction_modes`
test's representative from `CssSelect` to `RegexCapture`.

The Session 10 production findings showed CssSelect was the second
biggest gate on real-plan coverage (1 of 3 recipes on the bulgaria
elections plan). With F landed, eur_lex now has a real
`endpoint_hint`; with E added, eur_lex authoring should produce a
runnable recipe.

### P3 — Manual verification on a real plan

After P1 and P2 ship, classify a topic that exercises both the new
JSON wiring (Session 9), the CSV wiring (Session 8), and the new
CssSelect wiring. "EU AI Act enforcement" is a plausible candidate
— it should pull EUR-Lex (CssSelect, hinted), GDELT (JSON,
hinted), and possibly RSS feeds (no hint, fallback path
exercised). Run fetch, read the report, confirm the production-run
record-production rate is no longer zero.

### P4 — Coverage report (Option B)

Now defensible. With F+E shipped, real plans should start producing
records, and the all-gaps `CoverageReport` ADR 0007 specifies
becomes the right way to surface "did the plan get covered."
Plumbing exists in `research.rs`; the compute step lives in
`pipeline::research`. Roughly a session of work.

### Lower priority — D (per-recipe persistence), C (re-author on
failure)

Both still deferred. C in particular wants a failure-mode
taxonomy that we'll have more of after a few real runs against
the F+E executor; revisit in 2–3 sessions.

## Things I'd flag for review

1. **The `"null"` Apply-failure design question** carries forward
   from Session 10 unchanged. World Bank's API returns the literal
   string `"null"` for missing data, the runtime correctly fails
   apply on it. Whether to (a) keep current behaviour, (b) silent-
   skip at field level, or (c) introduce a per-field absent
   marker is still a discussion point, not a fix to ship.

2. **Empty-string vs `None` for `endpoint_hint`.** The TOML loader
   trims whitespace and treats empty/whitespace-only strings as
   `None` so the executor's lookup path doesn't see a useless empty
   string. This is consistent with the "empty-string is the strict-
   mode 'absent' wire form" convention from Session 5, but the
   normalisation happens at load time rather than use time. If
   that bothers anyone, the alternative is to keep the empty
   string in `Option<String>` and check `.as_deref().filter(|s|
   !s.is_empty())` at use time. I went with the load-time option
   because it's simpler and the field is descriptor metadata, not
   a transmitted payload.

3. **The recipe-author prompt's URL discipline section may
   over-constrain.** v1.3 says "same host" for derived URLs, which
   is the right default but might bite in edge cases — e.g. World
   Bank documents indicators on `worldbank.org` but serves them
   from `api.worldbank.org`. The prompt allows derivation "on the
   same host as the documented endpoint", and the
   `world_bank_indicators` `endpoint_hint` already points at
   `api.worldbank.org`, so this case is fine. But if a future
   source has documentation and data on different subdomains, the
   prompt may need a small loosening. Watch this on the first
   real run and adjust if needed — Session 5's discipline ("prompt
   revisions are small, prompted by a real plan that came back
   weak, never speculative") applies.

4. **Carried forward from prior sessions.** Anthropic provider
   stub, apply-runtime permissive deserialization, PdfTable
   unimplemented, authoring latency 30–60s, `SecureHttpClient`
   doesn't surface response headers, crate-level `#![allow(...)]`
   suppressions still need a sweep.

## Hard rules (carry-over)

- LLM-free at runtime. The pre-fetch in `author_one` is **not**
  a runtime call — it runs only during Level-2 authoring, which
  itself is conditional on no recipes existing for the plan yet.
  Once recipes are persisted, runs are deterministic and cheap.
  The `UnreachableProvider` discipline in tests still enforces
  this for the recipe-execution path.
- DuckDB ALTER trap: never `ADD COLUMN ... NOT NULL DEFAULT ...`
  in one statement, and never use the split form on a table with
  indexes. Fresh tables in `CREATE TABLE` are fine.
- Patch packaging: tar.gz extracted at `~/Downloads/`, applied via
  `tar -xzf ~/Downloads/patch.tar.gz --strip-components=1 -C .`
  from the repo root. **No `#`-prefixed hashtag comments in
  copyable shell commands** — they break zsh.
- One `SecureHttpClient`. Never `reqwest::Client::new()`.
- Six record types. No seventh. (ADR 0003)
- Topic is the universal subject tag. (ADR 0010)
- Closed enum of 5 extraction modes. Adding a sixth needs an ADR.
- UUIDv7 + dedup_key for identity.
- Code validates format, prompt teaches content. The LLM is trusted
  for what to put in the recipe; the code is responsible for what
  shape it must take.

## What Session 11 is explicitly NOT (predictively)

- **Re-authoring on failure** (Option C). Still deferred per
  ADR 0011; warrants ADR 0012 when undertaken.
- **Per-recipe outcome persistence** (Option D). Useful, not
  urgent; today's synchronous report is enough UX.
- **Coverage report** (Option B). Defensible after F+E ship and
  produce records, but not before.
- **Background scheduling.** One plan, one call, user-initiated.
- **Promoting PdfTable.** PdfTable is `NotImplemented` per ADR
  0007 Session-3 review note; needs its own focused session.
- **Promoting RegexCapture.** Long tail; the F+E pair gets us to
  three of five modes wired, which covers the majority of
  authoritative sources.

## Files to read first when starting Session 11

In order of importance:

1. This file.
2. `STOCKPILE_HANDOFF_SESSION10.md` — the production-run findings
   and the original Option F specification.
3. `crates/pipeline/src/fetch_executor.rs` — `author_one`,
   `prefetch_excerpt`, `placeholder_url`, `stub_excerpt`,
   `PREFETCH_EXCERPT_BUDGET`. The Session 10 changes are the
   model for Session 11's CssSelect promotion.
4. `crates/pipeline/src/research_classifier.rs::SourceDescriptor`
   — the new field's home and its rationale comment.
5. `config/prompts/recipe_author.md` v1.3 — the new "URL
   discipline" section.
6. `crates/pipeline/src/recipe_apply.rs` — find the existing
   CssSelect apply implementation; this is what `run_css_recipe`
   will call into.
7. `docs/adr/0007-research-function.md` — the runtime-path and
   closed-enum-of-five-modes architectural commitments.

## Continuity note

The Session 9 production run, Session 10's diagnosis, and Session
10's patch are a textbook example of how this codebase is supposed
to work: a real run revealed a structural gap, the next session
fixed the gap surgically, the change is small and the tests are
real. Resist the temptation in Session 11 to widen scope. Build,
fix flag-finds, ship CssSelect, do a manual real-plan run, write
the next handoff.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.
