# STOCKPILE — Session 45 handoff

You are starting Session 45. Session 44 shipped piece B from the
Session 43 handoff (PDF prefetch truncation gap) as patch 1
(`SESSION_44_PATCH_1.md`). Single-pass; no new dependencies; no
schema or migration; build + test green across the workspace
(297/297 pipeline tests, all other crates green).

**Read this file. Read `SESSION_44_PATCH_1.md`. Do not start by
writing an ADR. Do not propose architectural revisions. Start
working.**

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

## What works today (post-Session-44)

- L1 classifier emits descriptions only (Session 38).
- L2 propose-URL retry loop commits to a URL per nomination (Session
  39).
- PDF prefetch frames detected tables in the runtime's coordinate
  space; pages without tables marked explicitly (Session 41 patch 1).
- HTML prefetch produces a structural digest via `scraper` (Session
  41 patch 2).
- JSON prefetch produces a path/type shape outline via `serde_json`
  with polymorphic-leaf annotation and head-element rendering
  (Session 42 patch 3).
- Authoring-time validation runs the runtime's own extractor against
  the prefetched bytes for every authored `pdf_table`, `css_select`,
  and `json_path` recipe; recipes that wouldn't extract convert to
  `AuthoringError::Declined` (Session 41 patch 1, json_path arm
  pinned by Session 42 patch 3).
- xAI provider config defaults are intentional `grok-4.3` across
  tiers (Session 42 patch 4).
- Reasoning-effort plumbing — `CompletionRequest::reasoning_effort`
  threads per-tier `Low`/`Medium`/`High` to xAI's
  chat/completions endpoint; cost-tier differentiation lives in
  the request parameter, not the model string (Session 43 patch 1).
- `apps_common` tempdir race fixed — nanos + thread::id +
  AtomicUsize counter (Session 43 patch 1 drive-by).
- **PDF prefetch covers the whole document** — no-table-page
  narrative dropped, `PREFETCH_EXCERPT_BUDGET` bumped 32 KiB →
  64 KiB; the framed-table list across all pages serves as the
  implicit navigation index (page numbers in headers, row-0 cells
  naming tables). Long PDFs (USGS MCS shape, ~110 pages) now
  reach the LLM in full framing rather than first ~8 pages
  (Session 44 patch 1).

## Session 45 scope — pick the highest-leverage item from this list

These are real architectural pieces carried from Sessions 40, 43,
and 44. They are mutually independent. Pick one. Each is its own
session; **do not bundle more than one into a single tarball /
commit pair.**

### D. Network-layer issues (carried from Sessions 40, 43)

This piece has gone unaddressed across Sessions 43 and 44 because
both sessions picked content-pipeline pieces (A: reasoning-effort
plumbing; B: PDF prefetch truncation). D is the next obvious thing
and the foundational layer underneath any further source coverage
work.

Three sub-items. The principle-clean answers were sketched during
Session 44 (the operator audited the design with `are we again
hardcoding sources?` and the answers held up); a Session 45 patch
implementing them must keep that audit clean — no `if host == X`
branches, no per-source code paths.

- **SEC user-agent.** `data.sec.gov` requires a non-empty UA;
  without it everything 403s. Real architectural choice: every
  fetcher gets a configurable UA, **or** `SecureHttpClient` ships a
  default. The principle answer is `SecureHttpClient` ships a
  default UA from a build-time identifier (e.g., `SituationRoom/<version>
  (<contact>)`) for every request; per-fetcher override is a
  builder field, settable to anything; SEC is one caller among
  many. Don't bake SEC into any code path — that's the failure-
  mode rule the handoff has flagged across sessions.
- **Reuters feeds.** `feeds.reuters.com` appears defunct or
  blocked. Either find a working endpoint by network testing or
  drop Reuters from the candidate set entirely. The fix lives in
  the candidate-source data (`config/sources.toml` /
  `crates/apps_common/src/sources.rs`) or a generic feed-health
  probe at ingest time. Code does not learn "Reuters." Don't
  write `if host == "reuters.com" do X.`
- **`industry.gov.au` timeouts.** 300s timeouts suggest geo-block
  or aggressive rate-limiting. The principle answer is generic
  per-host backoff in the client, keyed on the host string at
  runtime; parameters uniform across hosts; runtime adapts on
  observed signals (429, `Retry-After`, timeouts). The host
  string is a runtime key, not a config knob. No
  `[per_host."industry.gov.au"]` table in any config file.

**Why this is its own session.** Touches `SecureHttpClient`'s UA
default, the candidate-source data file, and the per-host backoff
state in the fetch client — three distinct surfaces. Bundle them
together; they all live at the network layer. The compile-drift
risk is small but the test surface widens (UA assertions on the
HTTP client; backoff state assertions on the client; candidate-
source data file changes pick up at runtime via the existing
config-load path).

### Other architectural openings (not on the critical path)

- **Explicit outline block in PDF prefetch.** Session 44 chose
  implicit navigation through the framed-table list (every
  `[PDF page N, table M] ...` header inlines its page number, and
  row-0 column-header cells name the table). If a live run
  surfaces a long PDF where the LLM cannot disambiguate between
  similar-shaped tables without prose context, the additive
  change is parsing the PDF outline via `lopdf` and rendering a
  compact `[TOC]` block above the framed-table list. Not a
  Session 45 piece on its own — wait for a real failure case.
  See `SESSION_44_PATCH_1.md` "Architecture / Why no explicit
  outline parsing" for why the implicit version shipped first.
- **xAI Responses API migration.** Only architecturally necessary
  if a live grok-4.3 run shows chat/completions silently ignoring
  the `reasoning_effort` parameter Session 43 plumbed. If a live
  run confirms the parameter takes effect, this stays parked.

### Trivial drive-bys (not their own session, do alongside the chosen
piece)

None identified at the close of Session 44.

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

Same as Sessions 41–43, plus new ones from the Session 44 work:

- **Add `lopdf` for explicit TOC parsing without a triggering
  failure.** Session 44 deliberately chose implicit navigation
  through the framed-table list because (a) outline metadata is
  often absent, (b) "sample pages from outline targets" is a
  heuristic that drifts toward source-specific routing, and (c)
  the framed-table list IS the TOC for navigation purposes. Adding
  `lopdf` and a separate TOC block before there is a real failure
  case re-introduces the heuristic temptation Session 44's
  reframing avoided.
- **Bake source-specific defaults in the SEC UA path.** The
  default UA on `SecureHttpClient` is a generic project
  identifier (`SituationRoom/<version> (<contact>)`), not an SEC
  contact email. Per-fetcher override is the builder; SEC is one
  caller among many. The diagnosis "SEC needs a UA" lives in the
  patch notes; the fix lives at the client layer.
- **Configure `[per_host."industry.gov.au"]` backoff parameters.**
  Wrong. Backoff parameters are uniform defaults; runtime adapts
  on observed signals (429, `Retry-After`, timeouts). The host
  string is a runtime key, not a config knob. If a host needs
  different parameters, the per-host adaptation logic should
  derive them from observed signals, not from a static table.
- **Add a `match host` branch for Reuters.** Reuters lives in the
  candidate-source data file or a generic feed-health probe at
  ingest time. Code does not learn "Reuters."
- **Bundle reasoning-effort plumbing with model-string updates.**
  Don't. Patch shipped in Session 43 patch 1; do not relitigate.
- **Hardcode reasoning-effort per source ("if URL is large, use
  high effort").** That is source-specific routing in disguise.
  Per-tier mapping is fine; per-source is not.
- **"Improve" the JSON outline by replacing raw bytes with the
  outline.** Don't. The asymmetry is intentional.
- **Special-case World Bank, OECD, Eurostat, etc. in any new
  branch.** The polymorphic-leaf annotation is the principle that
  catches all of them without naming any of them.
- **Re-architect the iterator runtime to support more mode
  pairings.** ADR 0016 Phase 2 is its own session.
- **Edit the L1 classifier prompt.** Not in scope.
- **Edit the recipe-author prompt beyond what the chosen piece
  requires.** Session 44's v1.14 edit (PDF strategy section, no
  no-table-narrative) was required by the prefetch format change.
  Piece D should not require any further prompt edits — network-
  layer changes are invisible to the LLM.
- **Write a "Session 45 plan" document.** This file is the plan.
  Read it once, then code.
- **Ship a tarball / commit that "doesn't compile but is the right
  shape."** The operator runs `cargo build --workspace` and shares
  output. Each commit must compile.

## Files to read first

In order. Stop when you have enough to make the fix.

1. This file.
2. `SESSION_44_PATCH_1.md` (and `SESSION_43_PATCH_1.md` for the
   prior session's plumbing — useful context on how patches are
   shaped in this project).
3. The piece-specific files. For piece D:
   - `crates/secure/src/secure_http_client.rs` — UA placement
     decision lives here. Look for the request-builder path and
     where headers get set.
   - `crates/pipeline/src/http_fetcher.rs` and
     `crates/pipeline/src/fetch_backoff.rs` — backoff machinery;
     where per-host state would live if it doesn't yet.
   - `crates/apps_common/src/sources.rs` and
     `config/sources.toml` — candidate-source data; Reuters
     drop/replace lives here; structure for any feed-health
     probe.

## Continuity note

Operator works in RustRover on macOS, npm not pnpm, no git remote
(or one they manage outside the agent loop), paranoid about security,
prefers honesty about uncertainty over false confidence.

**Workflow.** Direct in-place editing in the workspace folder
(`~/Documents/Claude/Projects/SituationRoom/`). Operator runs cargo
on their Mac with output teed into the repo root:

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

After patch + green logs, agent says "rsync" or "ship it"; operator
pastes a saved rsync block to mirror the workspace folder to
`~/RustRoverProjects/situation_room/` for git/remote management.
The block is theirs; don't re-print it.

Operator approves with terse signals — "go", "continue", a log dump.
Reciprocate. Don't pad responses with status preamble or summary
postamble; lead with the actual move. Resume mid-stream on
"continue", don't restart.

When operator pushes back, listen. They have caught architectural
drift more than once across these sessions and have been right every
time. The most important push-back to internalize: **the LLM is the
only specialist; do not hand-code commodity adapters or
source-specific routing.** Sessions 38–44 honor this rule by giving
the LLM better evidence (PDF framed tables, HTML scraper digest,
JSON shape outline, whole-document PDF coverage) and validating its
output against the bytes it saw — not by encoding source-specific
knowledge anywhere.

After Session 45 ships its chosen piece, the runway is clear for the
next architectural piece. With A, B, C drive-by all shipped from
the Session 43 list, only D remains; after D the next major opening
is the optional outline block (only if a real failure justifies it)
or a fresh observation surfaced by then.

End of handoff.
