# STOCKPILE — Session 43 handoff

You are starting Session 43. Session 42 closed the Session 41
milestone — every recipe persisted is one the architecture has
ground-truth evidence will produce records, or the LLM honestly
declined. Items 3, 6, and 7 of the Session 42 handoff shipped as
patches 3 and 4 (`SESSION_42_PATCH_3.md`, `SESSION_42_PATCH_4.md`).

**Read this file. Read patches 3 and 4. Do not start by writing an
ADR. Do not propose architectural revisions. Start working.**

The operator's standing principle, re-stated because every prior
session that drifted got it wrong:

> Every fix must be one of:
> - Teaching the LLM what the runtime actually does.
> - Showing the LLM ground-truth bytes/structure.
> - Network-layer truth (UA, timeouts, backoff) with no LLM path.
>
> Anything that smells like "if URL contains X, do Y" or "for
> source S, use endpoint E" is the failure mode.

If you find yourself writing source-specific routing in code, prompt,
config, or fixture: **stop.** That is the failure mode the operator
has caught more than once across sessions and has been right every
time.

## What works today (post-Session-42)

- L1 classifier emits descriptions only (Session 38).
- L2 propose-URL retry loop commits to a URL per nomination (Session
  39).
- PDF prefetch frames detected tables in the runtime's coordinate
  space; pages without tables marked explicitly (Session 41 patch 1).
- HTML prefetch produces a structural digest via `scraper` (Session
  41 patch 2).
- **JSON prefetch produces a path/type shape outline via
  `serde_json` with polymorphic-leaf annotation and head-element
  rendering** (Session 42 patch 3).
- Authoring-time validation runs the runtime's own extractor against
  the prefetched bytes for every authored `pdf_table`, `css_select`,
  and `json_path` recipe; recipes that wouldn't extract convert to
  `AuthoringError::Declined` (Session 41 patch 1, **json_path arm
  pinned by Session 42 patch 3's null-trap test**).
- xAI provider config defaults are intentional `grok-4.3` across
  tiers (Session 42 patch 4 — see "Cost-tier differentiation" below).

## Session 43 scope — pick the highest-leverage item from this list

These are real architectural pieces discovered or carried during
Session 41–42. They are mutually independent. Pick one. Each is its
own session; **do not bundle more than one into a single tarball /
commit pair.**

### A. Reasoning-effort plumbing (xAI cost-tier differentiation)

**Why.** Session 42 patch 4 confirmed via web search that xAI
consolidated their lineup in May 2026 — `grok-4.3` is the universal
recommendation, the cheap-tier model strings are retiring on
2026-05-15. The handoff's complaint about cheap-tier paying frontier
prices is real and unresolved by patch 4 (which was config-only). The
actual fix is architectural: cost differentiation is now exposed as
`grok-4.3`'s **reasoning intensity** (low / medium / high) — a
request parameter, not a model name.

**What to do.**

- Add `reasoning_effort: Option<ReasoningEffort>` to
  `CompletionRequest` (`crates/llm/src/providers/trait_def.rs`),
  with `ReasoningEffort` an enum of `Low | Medium | High`.
- Add per-tier mapping to `XaiConfig` (`grok.rs`) — three new
  fields, `frontier_effort` / `workhorse_effort` / `cheap_effort`,
  with sane defaults (`High` / `Medium` / `Low`) and env override
  path mirroring the existing model-string overrides.
- Map `reasoning_effort` into the xAI request body in
  `XaiProvider::complete`. The body field name xAI uses changes;
  re-confirm against `https://docs.x.ai/developers/models` before
  shipping.
- Update Anthropic / OpenAI providers to accept the field on the
  request and ignore it (no-op) for parity; do NOT pretend they
  honor it.
- Test: pin that the xAI request body carries the per-tier effort
  parameter. Mock-response shape doesn't change so existing parser
  tests are unaffected.

**Why this is its own session.** Touches the provider trait, all
provider impls, and per-tier config. Compile-drift surface is wide;
bundle it alone so any test failure attributes cleanly.

### B. PDF prefetch truncation gap

**Why.** From the Session 41 patch-1 lithium MCS run: USGS MCS PDF
chapter is on page 110, but `PREFETCH_EXCERPT_BUDGET = 32 KiB` only
covers ~8–10 pages. The framing is correct (item 1 lands its job),
but the LLM sees the TOC, identifies the page it needs, and can't
get to it. New framing → honest decline; the gap is upstream.

**Two architectures, both viable.**

1. **Two-pass authoring.** First pass returns the TOC; LLM nominates
   a page range; prefetch re-fetches that slice with a bigger
   per-pass budget; second pass authors the recipe.
2. **TOC-aware excerpting.** When a PDF has a TOC, prefetch parses
   it and samples the pages it points at, not just the first N
   pages.

Pick one with the operator before starting. Each has its own risk
surface.

### C. `apps_common` test race

**Why.** Session 41 patch 2 found
`crates/apps_common/src/sources.rs::tests::tempdir()` uses
`SystemTime::now().as_nanos()` as its only entropy source. Parallel
`cargo test` collides on modern hardware. Manifests as flaky
`load_source_descriptors_respects_limit` ("left: 'a' / right:
'wb'").

**Fix.** 1–2 lines: add an `AtomicUsize` counter or
`thread::current().id()` to the path component. Trivial drive-by;
ship it on its own.

Workaround in the meantime:
`cargo test -p situation_room-apps-common -- --test-threads=1`.

### D. Network-layer issues (still pending from Session 40)

- **SEC user-agent.** `data.sec.gov` requires a non-empty UA;
  without it everything 403s. Real architectural choice (every
  fetcher gets a configurable UA, or `SecureHttpClient` adds a
  default). Don't bake the UA into the SEC code path — that's the
  failure-mode rule.
- **Reuters feeds.** `feeds.reuters.com` appears defunct or
  blocked. Either find a working endpoint by network testing or
  drop Reuters from the candidate set entirely. Don't write "if
  host is reuters, do X."
- **`industry.gov.au` timeouts.** 300s timeouts suggest geo-block
  or aggressive rate-limiting. Could be a per-host backoff
  question.

### Trivial drive-bys (not their own session, do alongside the chosen
piece)

- **Empty `code/` directory at repo root.** Leftover from
  `git archive --prefix=code/ HEAD -o clean_code.zip` extracted
  with `--strip-components=1`. Just remove it: `rmdir code`.

## Hard rules carried over

- Six record types. No seventh.
- Topic is the universal subject tag.
- Closed enum of N extraction modes. The piece you pick must not
  add a mode.
- ADR 0009: every HTTP call goes through `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`.
- TS files in `apps/desktop/src/lib/api/types/` are written by
  ts-rs; never hand-edit.
- ts-rs DTOs and pipeline structs are intentionally separate.
  Mirror, don't share.
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`.
- L1 prompt edits come from observed classifications, not
  speculation.
- **Stockpile prompts: principle-only language.** Never bake
  source-specific routing rules. ADR 0007's golden rule applies to
  prompt text as much as code.
- **Do not write code to pass tests.** If a test is obsolete,
  delete it with a comment explaining why and replace it with a
  test that pins the new behaviour.

## Things you will be tempted to do that are wrong

Same as Sessions 41–42, plus new ones from the Session 42 work:

- **Bundle reasoning-effort plumbing with model-string updates.**
  Don't. Patch 4 already shipped the model-string update. The
  plumbing is its own session.
- **Hardcode reasoning-effort per source ("if URL is large, use
  high effort").** That is source-specific routing in disguise.
  Per-tier mapping is fine; per-source is not.
- **"Improve" the JSON outline by replacing raw bytes with the
  outline.** Don't. The asymmetry is intentional — the LLM may
  need specific values to author a filter expression. The outline
  sits *above* the raw bytes, not in place of them.
- **Special-case World Bank, OECD, Eurostat, etc. in any new
  branch.** The polymorphic-leaf annotation is the principle that
  catches all of them without naming any of them.
- **Re-architect the iterator runtime to support more mode
  pairings.** ADR 0016 Phase 2 is its own session.
- **Edit the L1 classifier prompt.** Not in scope.
- **Edit the recipe-author prompt beyond what the chosen piece
  requires.** The Session 42 patch 3 prompt edits (json_path
  bullet + Document excerpt section) are the only changes; no
  follow-up tweaks.
- **Write a "Session 43 plan" document.** This file is the plan.
  Read it once, then code.
- **Ship a tarball / commit that "doesn't compile but is the right
  shape."** The operator runs `cargo build --workspace` and shares
  output. Each commit must compile.

## Files to read first

In order. Stop when you have enough to make the fix.

1. This file.
2. `SESSION_42_PATCH_3.md` and `SESSION_42_PATCH_4.md`.
3. The piece-specific files. For piece A:
   - `crates/llm/src/providers/trait_def.rs` — `CompletionRequest`,
     `ModelTier`.
   - `crates/llm/src/providers/grok.rs` — `XaiConfig`,
     `XaiProvider::complete`, the request body construction.
   - `crates/llm/src/providers/anthropic.rs` and `openai.rs` (or
     equivalent) — the parallel structures that must accept the new
     field as a no-op.

## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git remote
(or one they manage outside the agent loop), paranoid about security,
prefers honesty about uncertainty over false confidence.

**Workflow change since Session 42 onboarding.** The tarball-apply
flow is replaced by direct in-place editing. Operator runs cargo on
their Mac with output teed into the repo root:

```
cd ~/Documents/Claude/Projects/SituationRoom && \
  (cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log && \
  (cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

The agent reads `build.log` and `test.log` directly; the sentinel
`EXIT=0` lets the agent tell "done and green" from "still streaming."
Sandbox bash cannot reach `crates.io` or `sh.rustup.rs` — there is
no way to run `cargo` from inside the agent's container, and that's
fine because the operator's Mac is the source of truth anyway.

Operator approves with terse signals — "go", "continue", a log dump.
Reciprocate. Don't pad responses with status preamble or summary
postamble; lead with the actual move.

When operator pushes back, listen. They have caught architectural
drift more than once across these sessions and have been right every
time. The most important push-back to internalize: **the LLM is the
only specialist; do not hand-code commodity adapters or
source-specific routing.** Sessions 38–42 honor this rule by giving
the LLM better evidence (PDF framed tables, HTML scraper digest,
JSON shape outline) and validating its output against the bytes it
saw — not by encoding source-specific knowledge anywhere.

After Session 43 ships its chosen piece, the runway is clear for the
next architectural piece. The remaining "On the horizon" items
(B, C, D above) all stay on deck regardless of what Session 43
picks.

End of handoff.
