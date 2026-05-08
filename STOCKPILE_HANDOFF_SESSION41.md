# STOCKPILE — Session 41 handoff

You are starting Session 41. Read this file, look at the live
test result the operator shares, then start working. The
architecture is settled.


## What works today

Session 40 fixed two bugs Session 39 left behind and updated the
recipe-author prompt to match the runtime.

**Bug A — duplicate decline source_ids.** Session 39's
`derive_source_id_for_decline` took the first 8 hex chars of the
nomination's UUIDv7. UUIDv7's first 48 bits are the millisecond
Unix timestamp; the classifier mints all of one plan's
nominations in well under a millisecond, so every decline came
back with `nom:019e06b0` (or whatever the millisecond was). The
frontend's `{#each report.outcomes as o (outcomeKey(o))}` keyed-
each in `FetchReport.svelte` produces `declined:<source_id>`
keys, so duplicate source_ids meant Svelte 5 threw
`each_key_duplicate` and the outcomes list refused to paint.
That was the operator's "looks identical before and after Run
Fetch" symptom — handoff misidentified the location, the actual
fix is at the executor's source_id derivation.

The fix: full nomination_id, not 8-char prefix. ADR 0013
recipe-feedback (keyed on `(plan_id, source_id)`) was also
broken — flagging one decline appeared to flag all — and is now
correct.

**Bug B — recipe-author was being shown raw PDF binary.** The
runtime apply path (`recipe_apply::extract_pdf_table`) was
already wired in Session 29; the gap was at *authoring* time.
`prefetch_excerpt` fed PDF bytes through `from_utf8_lossy`, so
the LLM saw a wall of replacement chars and correctly declined
every PDF source it ever met. Session 40's fix sniffs `%PDF-`
magic and runs the bytes through `pdf_extract::extract_text_from_mem_by_pages`
(the same library and same per-page reader the runtime apply
path uses), joining pages with `[PDF page N]` markers (1-indexed,
matches the `pdf_table.page` recipe coordinate). The LLM now
sees readable text aligned with what `extract_pdf_table` will
index into at apply time.

**Prompt truth-up.** `config/prompts/recipe_author.md` had two
stale claims that pdf_table "is not yet wired in the runtime"
plus a "First move: hunt for the HTML equivalent" subsection
listing source-specific filename patterns (USGS MCS, SEC EDGAR,
EUR-Lex). The lies are gone; the source-specific routing is
gone. The principle behind it (prefer HTML when both formats
publish the same data) stays, but expressed without naming
sources — same posture as the L1 prompt's "principle-only
language" rule from Session 34.

271+ pipeline tests + 76 api tests + 103 storage tests + 50 llm
tests pass. Five new tests in fetch_executor pin the Session 40
fixes:

- `decline_source_ids_are_unique_across_nominations`
- `is_pdf_recognizes_pdf_magic`
- `render_pdf_text_against_lithium_fixture_emits_page_markers_and_table_text`
- `render_pdf_text_surfaces_errors_for_non_pdf_bytes`
- `prefetch_excerpt_for_pdf_url_yields_extracted_text_to_recipe_author`


## What Session 41 should do

The operator will share a fresh live re-run of "titanium supply
chain" (or another topic). The next steps depend on what shows
up. The handoff for the next session is therefore *conditional*
— read the operator's live output before picking from the menu
below. **Do not speculatively pre-pick a target without seeing
the new run.**

### If the FetchReport panel renders correctly with the seven
### decline rows but every nomination still declines

The source_id fix is good. The propose-URL retry loop is doing
its job. The remaining failures are network-layer realities:

- **SEC EDGAR returns 403.** The default User-Agent emitted by
  `SecureHttpClient` doesn't satisfy SEC's "must identify
  yourself" policy. The fix is a User-Agent string in the
  client config. Read `crates/secure/src/http.rs` for where
  the UA is set; the SEC's policy is at
  `https://www.sec.gov/about/webmaster-faq#code-of-ethics`
  (verbatim guidance: include a contact email).
- **Reuters returns 401 / RSS feed 0 bytes.** Reuters has paywalled
  most public RSS endpoints since 2024. There may not be a fix
  that doesn't involve a registered API key. Drop the source
  from the L1 prompt's list of "good news desks" or accept the
  decline.
- **industry.gov.au times out (300 s).** The default fetch
  timeout is too high for hosts that hang. There's a separate
  `fetch_with_backoff` budget per attempt; check whether 60 s
  per attempt would fail faster without losing real fetches.

These are three small separable patches. **Don't bundle them.**
Each one has its own success criterion (live run produces a
non-403 / non-401 / non-timeout fetch from that source).

### If the USGS MCS PDF authoring now produces a recipe but the
### recipe fails at apply time

The pdf_extract library is the same on both sides, so this
*should* work — but pdf-extract's whitespace collapsing on real
USGS chapters could cause column misalignment that Session 29's
table detector handles imperfectly. The diagnosis path:

1. Capture the recipe's `(page, table_index, row, col)` from the
   recipes panel.
2. Run `pdf_extract::extract_text_from_mem_by_pages` on the same
   PDF locally; check whether the table at those coordinates
   actually contains the expected value.
3. If the LLM's coordinates are off-by-one or off-by-column, the
   prefetch excerpt's marker formatting is misleading the LLM.
   Tighten the marker (e.g., per-table markers, or a column-count
   annotation per page).

### If the FetchReport panel still doesn't render

That would mean the source_id fix wasn't enough — there's a
second bug in the frontend rendering path that the each-key
collision was masking. Walk Session 39's original diagnosis path:

1. Reproduce. Run fetch on a fresh classified plan.
2. Browser devtools / Svelte devtools: check `plans.fetchReport`
   in the runes store immediately after the IPC call resolves.
   Confirm it's non-null.
3. Check `plans.fetchRuns` after `refreshFetchRuns` resolves.
   Confirm length > 0.
4. If both are populated and the panel still doesn't paint, the
   bug is in `FetchReport.svelte`'s inner gate or one of the
   `outcomeKey` / `outcomeTone` / `outcomeLabel` helpers
   throwing on a previously-unseen variant. Add a try/catch in
   the each loop's body to surface the exact line that errors.

### If the operator picks a different topic that produces zero
### nominations from L1

That's a different category of failure. The L1 classifier's
prompt at `config/prompts/research_classifier.md` would be the
target — but **don't edit the prompt without operator approval**
and **don't edit it on speculation about a single bad
classification**. The standing rule from the rule book: prompt
edits come from observed classifications, not speculation. Wait
for the operator to flag the topic and the bad output together.


## Things you will be tempted to do that are wrong

- **Add a sixth extraction mode (`xlsx_cell` was the leading
  candidate from the Session 40 handoff).** The extraction
  vocabulary is closed; adding a mode is allowed in principle
  but each addition is a focused session of its own. Session 41
  has more leveraged work above (network-layer fixes for the
  three sources that returned 4xx in the live run). Don't bundle
  a mode addition with anything else.
- **Speculatively re-architect the retry loop.** The propose-URL
  prompt is doing its job — Session 39's split correctly emits
  description-only L1 + propose-URL L2, and the live run shows
  structurally correct URLs every time. The remaining failures
  are at the network or content-vocabulary boundary, not in the
  loop's structure.
- **Edit the L1 prompt without a live failure to diagnose.** The
  rule book says "no prompt edits without a real plan that came
  back weak." Same applies to the recipe-author prompt.
- **Add an `index` fallback to the frontend's `outcomeKey`.**
  The Session 40 fix is at the source. Adding a fallback would
  silently mask future regressions of exactly this kind. The
  deterministic-runtime posture says "catch what the LLM gets
  wrong"; it doesn't say "paper over backend bugs in the UI."
- **Bundle multiple network fixes (SEC + Reuters + industry.gov.au)
  into one patch.** Each has its own success criterion and its
  own risk surface. One per session.
- **Write a "Session 41 plan" document.** The plan is this
  file plus the operator's next live test result. Read both,
  then code.


## Hard rules carried over

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. Adding a mode is allowed
  but must be its own session, not bundled with bug fixes or
  prompt edits.
- ADR 0009: every HTTP call goes through `SecureHttpClient`. No
  `reqwest::Client::new()`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; never hand-edit.
- ts-rs DTOs and pipeline structs are intentionally separate.
  Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded
  hex.
- Runes-using files end in `.svelte.ts`.
- L1 prompt edits come from observed classifications, not
  speculation. Same for the recipe-author prompt.
- Stockpile prompts: principle-only language. Never bake
  source-specific routing rules ("if URL contains X, do Y").
  ADR 0007's golden rule applies to prompt text as much as
  code. Anti-examples may describe failure shapes but must
  teach a generalizable principle, not a special case.
- **Do not write code to pass tests; if a test is obsolete,
  delete it with a comment explaining why.**


## Files to read first when starting Session 41

In order. Stop reading when you've got enough to make the fix.

1. This file.
2. `SESSION_40_APPLY.md` — what changed, what to verify.
3. The operator's live test result for Session 41 — this is the
   primary input. Without it, you can't pick the right target.
4. For network-fix candidates only: `crates/secure/src/http.rs`
   (where the User-Agent and timeouts live).
5. For "FetchReport still doesn't render" only:
   - `apps/desktop/src/stores/plans.svelte.ts` — the runFetch
     action, `runFetch` and `refreshFetchRuns`.
   - `apps/desktop/src/components/FetchReport.svelte` — the
     gates and each-loop.


## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git
remote, paranoid about security, prefers honesty about
uncertainty over false confidence. Patches arrive as `.tar.gz`
applied via `tar -xzf ~/Downloads/<name>.tar.gz
--strip-components=1 -C .` from the repo root. Operator runs
`cargo build --workspace`, `cargo test --workspace`, and
`cd apps/desktop && npm run check` after each patch and shares
the output. You diagnose and ship the next patch.

Operator approves with terse signals — "go", "continue", a log
dump. Reciprocate the terseness. Don't pad responses with
status preamble or summary postamble; lead with the actual
move.

When operator says "continue" mid-stream, they mean continue
the same work in a fresh context. Don't restart, don't
re-explain what you were doing. Resume.

When operator pushes back, listen. They have caught
architectural drift more than once across these sessions and
they have been right every time.

Session 39's split (description-only L1, propose-URL retry loop
at L2) is the correct architecture. Session 40 fixed the two
bugs in Session 39's plumbing that were hiding behind the same
visible symptom (FetchReport doesn't render → source_id
collision; PDFs always decline → recipe-author saw raw bytes).
The remaining failures in Session 41's live run will be at the
network or content-vocabulary boundary — not in the loop's
structure.

End of handoff.
