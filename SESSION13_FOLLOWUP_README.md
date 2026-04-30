# Session 13 follow-up — A + B + C

Three changes prompted by the three real runs we did against the
S13 polish patch:

- **A**: promote `RegexCapture` from skipped to wired, fourth of the
  five extraction modes. Was triggered by the EU AI Act run authoring
  a sensible regex against EUR-Lex's RSS feed and being skipped.
- **B**: env-driven model overrides + truncation retry. Was triggered
  by the eur_lex `EOF while parsing a string at line 1 column 519`
  failure on Workhorse-tier authoring, and by the user wanting to
  experiment with frontier-tier without a recompile.
- **C**: logging gaps. Was triggered by a 1m25s silent stretch during
  multi-source authoring that made the GUI look frozen.

Apply on top of the green Session 13 polish build:

    tar -xzf ~/Downloads/session13_followup.tar.gz --strip-components=1 -C .
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

No migrations, no DTO/wire changes, no frontend changes. Pure Rust
edits in two files plus this README.

## What this patch does

### A — Promote RegexCapture (`fetch_executor.rs`)

`crates/pipeline/src/fetch_executor.rs`:

- New helper `run_regex_recipe`. Structurally identical to
  `run_csv_recipe`, `run_json_recipe`, and `run_css_recipe` — the
  dispatch on `ExtractionSpec` happens inside `apply()`, not here.
- `run_one_recipe` dispatch arm replaced: `ExtractionSpec::RegexCapture`
  now routes to `run_regex_recipe` instead of `RecipeOutcome::Skipped`.
- The unwired-mode canary test (`run_fetch_for_plan_skips_unwired_extraction_modes`)
  is repointed from RegexCapture (now wired) to PdfTable (the only
  remaining unwired mode).
- Two new tests mirror the CSS pair from S12:
  - `run_fetch_for_plan_succeeds_against_regex_recipe_without_calling_llm`
  - `run_fetch_for_plan_reports_apply_failure_on_unmatched_regex_pattern`
- New test helper `working_regex_recipe` mirrors the CSV/JSON/CSS
  helpers.
- Module docstring's "Extraction-mode policy" section updated:
  RegexCapture is now wired; PdfTable is the only remaining
  `Skipped` mode and probably stays that way for several sessions.

The runtime side of RegexCapture has been implemented in
`recipe_apply.rs` since Session 3 (via the `regex` crate, three
passing tests). What was missing until this patch was the
executor-level dispatch + apply-and-insert plumbing — exactly the
same shape as the S12 CssSelect promotion.

### B — Env-driven model overrides + truncation retry (`grok.rs`)

`crates/llm/src/providers/grok.rs`:

**Env-driven model overrides.** Three new optional env vars:

    XAI_FRONTIER_MODEL
    XAI_WORKHORSE_MODEL
    XAI_CHEAP_MODEL

Each falls back to the corresponding `XaiConfig::default()` value if
unset, empty, or whitespace-only. Wired through
`XaiProvider::from_env` so a `.env` file at the workspace root can
swap any tier without recompiling. New `XaiConfig::from_env()`
factory; new `env_or` helper for the empty/whitespace normalization.
Logged at INFO once at provider construction so operators who set
the var can confirm the override took effect.

Use case: when the workhorse tier truncates structured output (the
S13 eur_lex Class-E failure), a one-line shell tweak —

    export XAI_WORKHORSE_MODEL=grok-4.20-0309-reasoning

— retargets every authoring call to the frontier model for that run
without changing code or config files.

**Truncation retry.** When a structured-output completion fails to
parse with an "EOF" / "end of input" signature (the S13 eur_lex
case), the provider retries the same request once with `max_tokens`
doubled (clamped to a 32K ceiling). One retry only; if the bigger
budget also truncates, the model genuinely cannot finish and the
caller gets the original error back.

The retry path is deliberately narrow:
- Only triggers when `request.schema.is_some()` (no point retrying
  unstructured completions; they don't go through the JSON parser).
- Only triggers on `LlmError::JsonParse` whose message contains
  "EOF" or "end of input". A schema-violation error like "invalid
  type: string ..., expected an integer" does NOT retry — bigger
  budget can't fix wrong type.
- Only triggers when the original `max_tokens` is below the retry
  ceiling so doubling actually changes anything.

New helper `looks_like_truncated_json` is the discriminant; pure
function, five tests covering EOF-in-string, EOF-in-object,
unexpected-end-of-input, schema-violation rejection, and other
error kinds.

`complete()` is restructured to call a private `send_one` (the
original post + parse logic). The retry path calls `send_one`
again with the doubled-budget request. Both attempts log at
INFO/WARN level so the operator can see the retry happening.

Net test count delta in `grok.rs`: +9 (4 env-config tests, 5
truncation-detector tests).

### C — Logging gaps (`fetch_executor.rs`)

Three new INFO-level logs in the authoring path:

1. **Authoring loop entry/exit**: `load_or_author_recipes` now logs
   the total source count at the top of the loop ("starting") and
   the (succeeded/total) at the bottom ("complete"). The loop is
   restructured to flatten (hint, source_id) pairs into a single
   `Vec<String>` upfront so we know the total before starting and
   can cross-dedup sources nominated by multiple hints.

2. **Per-source progress**: each source-author iteration logs
   `source_id`, `position`, `total` so the operator sees "authoring
   2 of 3" framing rather than a silent stretch.

3. **Pre-fetch start**: `prefetch_excerpt` logs the URL it's about
   to fetch at INFO. Was previously silent until either the fetch
   succeeded (no log) or failed (WARN); the operator now sees the
   "fetching X" step explicitly.

Net behavioural change: zero. These are observability additions
only. No new tests — logging is observable in stdout and the
existing tests still pass with whatever log subscriber is or isn't
installed.

## What this patch does NOT do

- **Does not promote PdfTable**. It's the last unwired mode; carries
  enough complexity to deserve its own session.
- **Does not author ADR 0012** (re-author on failure). The S13 P4
  list item is still deferred. The truncation retry is a *narrower*
  problem — it's the *authoring call itself* failing at the LLM-
  response layer, before any recipe was produced. ADR 0012 is about
  *recipe runtime* failures.
- **Does not bump the recipe-author prompt**. Same prompt-revision
  discipline — never speculative.
- **Does not change any wire shape.** Frontend untouched.
- **Does not ship live tests for the retry path.** The retry only
  fires on the specific class-E truncation, which we can't
  reproduce on demand against the real xAI gateway.

## Verifying after apply

The Rust workspace gains 11 new unit tests across two files:

- `cargo test -p situation_room-llm` — adds 9 tests.
- `cargo test -p situation_room-pipeline` — adds 2 tests, repurposes
  the canary.
- `cargo test --workspace` — total goes from 260 → 271 unit tests.
  `+ 6 ignored` count unchanged.
- `cargo clippy --workspace --all-targets -- -D warnings` — no
  new warnings.
- `cd apps/desktop && npm run check` — unchanged (no frontend edits).

For the manual flow:

1. **Re-run "EU AI Act enforcement"** — the rss_feeds recipe that
   was `skipped` in the S13 run will now actually run. If the regex
   matches the feed XML, you get records. If not, you get a
   `failed @ apply` with the exact regex echoed in the failure
   detail (visible inline thanks to the S13 P2 badges).

2. **Test the env override**: in `.env` (or temporarily exported in
   the shell), set:

       XAI_WORKHORSE_MODEL=grok-4.20-0309-reasoning

   Re-launch the desktop binary. The startup log line will read:

       INFO ... xai: provider configured frontier=... workhorse=grok-4.20-0309-reasoning cheap=...

   Now classification and recipe authoring use the frontier model.
   The eur_lex truncation that hit the workhorse should succeed on
   the frontier.

3. **Watch the multi-source logging**: with this patch you'll see —

       INFO ... authoring recipes for plan: starting total_sources=3
       INFO ... authoring source position=1 total=3 source_id=eur_lex
       INFO ... pre-fetching endpoint hint url=https://...
       (per-source authoring runs)
       INFO ... authoring source position=2 total=3 source_id=...
       ...
       INFO ... authoring recipes for plan: complete succeeded=2 total_sources=3

   The S13 silent stretch is no longer silent.

## Files in this patch

    crates/llm/src/providers/grok.rs           (rewritten — +9 tests, retry path, env config)
    crates/pipeline/src/fetch_executor.rs      (edited — +run_regex_recipe, +2 tests, repointed canary, logging)

Two files. No new modules, no new types crossing crate boundaries.
