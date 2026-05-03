# STOCKPILE — Session 24 handoff

You are starting Session 24. Session 23 promoted the Anthropic
provider from a 7-line stub to a real implementation, generalised
`AppState.provider` to a trait object, and added an `LLM_PROVIDER`
env var so the binaries pick xAI or Anthropic at boot. Default
behaviour is unchanged (`LLM_PROVIDER` defaults to `"xai"`); the
operator opts into Anthropic by setting two env vars.

Read this whole document before writing any code. The ADRs in
`docs/adr/` are still authoritative — this handoff is the layer
above them, not a replacement.

## What Session 23 shipped

| Layer | What landed |
|---|---|
| LLM | `crates/llm/src/providers/anthropic.rs` — full provider, ~970 lines, mirrors `grok.rs`'s structure adapted for Anthropic's wire format (top-level `system`, `x-api-key` auth, `anthropic-version` header, structured output via forced tool use, `stop_reason: "max_tokens"` truncation signal). Truncation-retry path policy-equivalent to xAI. 21 new unit tests, 2 new ignored live tests. |
| LLM | `providers/mod.rs` + `lib.rs` — re-exports for `AnthropicProvider`, `AnthropicConfig`, `ANTHROPIC_API_KEY_ENV`. |
| API | `crates/api/src/commands.rs` — `AppState.provider` lifted from `Arc<XaiProvider>` to `Arc<dyn LlmProvider + Send + Sync>`. Two `save_research_plan*` lineage strings switched from hardcoded `"xai"` to `state.provider.id()`. The trait-object spelling carries explicit `+ Send + Sync` because supertrait bounds aren't auto-applied to `dyn Trait`. |
| Apps | Both binaries — new `pick_provider(http)` helper reading `LLM_PROVIDER` (default `"xai"`), constructing the matching concrete provider, type-erasing. Unrecognised values abort boot. CLI duplicates the helper (intentional — pulling it into a library crate would expose app-level boot decisions through a crate boundary). |
| Config | `.env.example` — new `LLM_PROVIDER=xai` line; new (commented-out) `ANTHROPIC_FRONTIER_MODEL`, `ANTHROPIC_WORKHORSE_MODEL`, `ANTHROPIC_CHEAP_MODEL`, `ANTHROPIC_VERSION` overrides. |

### Test count posture

Expected delta over the Session 22 baseline of 380:

- llm: **+21** (anthropic provider unit tests)

Total **+21**, landing at **401** green. Plus 2 new ignored live
tests (`live_anthropic_returns_nonempty_completion` /
`..._structured_json_when_schema_requested`).

If the operator runs `cargo test --workspace` and lands at 401, the
patch built end-to-end as designed. Anything below 401 is a sign
something didn't compile or a fixture changed shape — read the
failing test's name and chase it.

### Security posture

No new HTTP path beyond what `SecureHttpClient` already mediates.
The Anthropic provider reads `ANTHROPIC_API_KEY` exclusively via
`ApiKey::from_env_optional`, which enforces the same 16-char-min,
non-placeholder, non-empty rules as `XAI_API_KEY`. The header
value is wrapped in `SecretString` and `SecureHttpClient::
post_json_bytes` marks the header value `set_sensitive(true)` so
reqwest-internal logging redacts it. ADR 0009 §"The rule"
satisfied — no `reqwest::Client::new()` introduced.

## What was not shipped, intentionally

### P1 — live xAI verification (still carry-forward)

Three slots from the Session 20 handoff, owed by the operator's
network-enabled machine:

1. **HTML-equivalent path** (USGS MCS).
2. **CELEX re-run** — closes the deferred-by-design EUR-Lex case.
3. **BAKED PDF source** — confirm `static_payload` end-to-end.

**Now incidentally also a verification of Session 23**: pick any of
the three slots, run it once with `LLM_PROVIDER=xai` and once with
`LLM_PROVIDER=anthropic`, diff the resulting `ResearchPlan` JSON.
The cheapest A/B test of classifier quality the project can
currently run.

### P2 — EUR-Lex CELEX failure-case banner (still blocked)

Same blocker as Sessions 20–22: `apps/desktop/failure_cases/` is
absent from the archive and not in `.gitignore`. Operator owes a
disposition decision (commit the tree, or document the convention
and remove the references from session handoffs).

### Wiring `pdf_table` extraction

Explicitly deferred by ADR 0007's June-2026 review note (line 507
of the ADR file). `static_payload` (Amendment 3) is the
architecturally sanctioned workaround for the bake-time-frozen
case; `pdf_table` lands when freshness on PDF-only sources becomes
the operative blocker. Not yet.

### Automating re-author-on-failure

ADR 0012's "READ THIS FIRST — DO NOT DEVIATE" block names the
single gate: **10 or more empirically observed, distinctly-shaped
Class B failures across diverse sources and plan types, documented
in `docs/failure_cases/class_b/`. We do not have that yet.** Until
the gate is met, automation is forbidden. The ADR's Part 2 records
the eventual architecture; do not implement it early.

## Known imperfections (carry-forward + new)

These are conscious leftovers, not surprises.

### 1. Provider stubs that didn't get the Session 23 treatment

`crates/llm/src/providers/openai.rs` and `gemini.rs` are still
~200-byte stubs. The Anthropic promotion proved out the pattern;
the same pattern fits both stubs. Lower priority than xAI +
Anthropic because the architectural concern Session 23 addressed
(single-provider lock-in) is now addressed; OpenAI / Gemini are
nice-to-have, not need-to-have.

### 2. `SourceDescriptor` loaders duplicated across binaries

The CLI and the desktop binary both define a `SourcesFile` /
`SourceEntry` pair locally and call a local `load_source_descriptors`.
The two are word-for-word identical. Session 23 made the duplication
more visible by adding a deliberately-duplicated `pick_provider`
helper alongside it; the principle that justifies duplicating
`pick_provider` (app-level boot decisions don't belong in library
crates) does **not** apply to source-descriptor loading (that's
shared code with no app-specific decisions). Lift to a small
`apps_common` crate when adding a third binary.

### 3. `XaiProvider`-shaped tests in the api crate

The api crate has no test that exercises the trait-object lift
directly. The lift was verified by reading: the change at the
import + AppState field + three call sites is small enough to
audit by hand, and the existing live tests of `classify` /
`reclassify` continue to validate the end-to-end behaviour. If a
future provider-mocking test wanted to inject a fake provider
into AppState, the trait object now permits it cleanly.

### 4. `docs/failure_cases/class_b/` directory absent

ADR 0012 names it as the evidence-base directory for Class B
failures. The directory doesn't exist in the tree. A `.gitkeep`
adds it cheaply; flagged so the next person hitting a Class B
failure has a home for the file.

### 5. The chip cluster's layout headroom (carry-forward from S22)

Three chips (BAKED, STUB-AUTHORED, FLAGGED) is currently the
maximum that fits glanceably in the recipe-card head. A fourth
needs layout rework first. No fourth chip is on the roadmap; a
"classified by Claude / Grok" tag would be one candidate — but
it would also be redundant with `classified_by` already in the
DTO, so don't add it.

### 6. Carry-forward from Session 22

- `apps/desktop/failure_cases/` tree absent (P2 above).
- The `sanitize_for_fence` helper duplicated across
  `research_classifier.rs` and `recipe_author.rs`.
- Pre-ADR-0014 recipes load as `AuthoredFrom::Unknown` (legacy
  data; intentional posture, no chip until re-authoring).
- Option 3 (silent self-healing of stub-authored recipes)
  deferred — ADR 0014 §"What the user does NOT see" enumerates
  the four amendment-trigger questions.

### 7. Carry-forward from Session 19 and earlier

- Apply-runtime strict deserialization permissive.
- PdfTable extractor unimplemented.
- Authoring latency 30–60s (xAI gateway, not us; Anthropic latency
  TBD — the live test will tell).
- `SecureHttpClient` doesn't surface response headers — affects
  both xAI's and Anthropic's `retry-after` story.
- Crate-level `#![allow(...)]` lint suppressions outside the api
  crate.

## Suggested Session 24 priorities

In rough order of leverage:

### Priority 1 — Live A/B verification of providers

The single highest-leverage move now possible. With both providers
real, the operator can take any one of P1's three slots, run it
with each provider, and form a first opinion on relative quality
on real plans. Specifically expect:

- **HTML-equivalent USGS MCS / SEC EDGAR** — both providers should
  produce `FetchedBytes` recipes, no `STUB-AUTHORED` chip.
- **CELEX re-run** — likely `FetchedBytes` in both cases. If the
  selectors differ between providers, that's a useful data point
  for prompt v1.9.
- **A source whose hint URL is intermittently rate-limited** (e.g.
  GDELT) — should produce `StubExcerpt` recipes, chip appears, in
  both. Confirms the provenance signal works regardless of
  provider.

The output is two `ResearchPlan` JSONs and the operator's read on
them. Three or four runs is enough to tell whether the prompts'
v1.4 / v1.8 are equally well-tuned for both providers, or whether
provider-specific prompt forks are warranted (the latter is a
much bigger ADR-level decision; don't make it on one run).

### Priority 2 — Decide P2's failure_cases disposition

Two named options in Session 21's README and Session 22's handoff:
commit the tree, or document the convention. Either choice
unblocks the EUR-Lex banner. The decision is small.

### Priority 3 — `docs/failure_cases/class_b/` `.gitkeep`

ADR 0012 names this directory as the evidence-base for any future
re-author automation. Adding a `.gitkeep` is a single-line patch
and materially lowers the friction for the first session that
documents a real Class B failure. Worth doing as a 30-second
follow-up alongside any other patch.

### Priority 4 — `SourceDescriptor` loader consolidation

Lift the duplicated `SourcesFile` / `SourceEntry` pair into a
small `apps_common` crate (or into `pipeline::sources` if that's
where it belongs architecturally). Leave `pick_provider`
duplicated — the architectural reason for that one differs and
should be documented in the new crate's docstring as the
exemplar of "why some duplication is intentional".

### Priority 5 — OpenAI provider, real

Same scaffolding as Anthropic. OpenAI's chat-completions wire
format is identical to xAI's (xAI's is OpenAI-compatible by
design), so the body construction is mostly a copy of `grok.rs`'s
`build_body`. The differences live in the auth header
(`Authorization: Bearer …`, same as xAI), the endpoint
(`https://api.openai.com/v1/chat/completions`), and the model
catalog. A faster session than the Anthropic one was.

## What Session 24 is explicitly NOT

- **Not implementing automated re-author-on-failure.** ADR 0012's
  10-Class-B-failure gate is unmet. Don't pre-empt.
- **Not wiring `pdf_table`.** ADR 0007 defers it. The
  `static_payload` path covers the operative cases.
- **Not back-filling pre-v10 `authored_from` rows.** Same reasoning
  as Session 22: retroactive truth claim, warrants its own
  decision.
- **Not adding a fourth chip to the recipe head.** Three is the
  ceiling.
- **Not bumping the recipe-author or classifier prompts.**
  Provider-quality A/B (P1 above) might surface evidence
  motivating a v1.9 / v1.5 bump; if it does, that's a deliberate
  next-session shape, not the same patch.
- **Not adding a second LLM call site in the runtime path.**
  ADR 0007 §"runtime path" is unambiguous: runtime is LLM-free.

## Hard rules (carry-over, unchanged from Sessions 5–23)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All HTTP
  through `SecureHttpClient`.
- Bounds checking on every IPC string input. `check_user_text` is
  the gate for user-supplied text.
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` written
  by ts-rs via `cargo test -p situation_room-api`. Never hand-edit
  except as a temporary measure shipped alongside the matching
  Rust change.
- ts-rs DTOs and pipeline structs are mirrored, not shared.
- Components only use CSS vars from `global.css`. No hardcoded
  hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Migrations: read the prior migration's comment block before
  writing the next. The DuckDB `ALTER TABLE` constraint trap is
  real.
- API keys (xAI **and** Anthropic) never read, written, or
  referenced anywhere visible. `ApiKey::from_env*` only.
- **New for Session 23**: when adding a new LLM provider, mirror
  the `grok.rs` structure (config / provider / trait impl / wire
  shapes / tests) but adapt the wire format to match the
  provider's docs. Do not pull in an SDK; the project-wide
  contract is one HTTP client, one set of guards.

Standing-order priority: **security > generalisation > simplicity**.

## First thing to do in Session 24

1. Read this file.
2. Run `cargo test --workspace` to confirm the **401**-test baseline.
   If you see fewer, the patch didn't land cleanly — check the
   compiler errors first.
3. Decide P1-vs-P2-vs-something-else with the operator. P1 is
   field-work the operator's machine can do (now meaningfully
   broader — both providers); P2 is a small posture decision the
   operator owes; P3+ are smaller follow-ups.

If P1 surfaces material that justifies a v1.9 prompt bump or an
ADR amendment, those are deliberate next-session shapes — don't
do them in the same patch as the verification run.

## Continuity note

The continuity note from Sessions 19–22 still applies. The
operator is rigorous about security ("paranoid about security" —
earned, not affected), prefers honesty about uncertainty over
false confidence, reacts well to direct disagreement when
warranted, and has explicitly asked for "do not deviate"
discipline.

Session 23's deviation was structurally invited by the operator
("do as much as you can"); the Anthropic provider work is the
named carry-forward from every handoff since 17, so going large
on it isn't deviation, it's overdue execution. The standing
posture for Session 24 is unchanged from the default: stick to
the plan; deviation requires a real defect against a published
ADR's invariant, or an explicit operator instruction that
broadens scope.

The codebase has a strong existing style. Read three files in
any crate before writing a fourth. The hardest part of
contributing well here is matching the existing voice in the
code comments and the ADR cross-references — the comments aren't
decoration, they're load-bearing for the next reader.

End of handoff.
