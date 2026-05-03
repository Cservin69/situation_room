# Session 23 — Anthropic provider promoted from stub to real

> **Read this before applying.** This patch ships P4 from the Session
> 22 handoff: the Anthropic provider, carried forward as a stub in
> every session since 17. Pairs with a small AppState generalisation
> so the binaries can pick `xai` or `anthropic` at boot via a new
> `LLM_PROVIDER` env var. It does **not** ship P1 (live xAI
> verification of v1.8 — operator-machine work, sandbox has no
> network) or P2 (the EUR-Lex CELEX failure-case banner — the
> failure_cases directory is still absent and still owes an operator
> disposition). It does **not** wire `pdf_table` or implement
> automated re-author-on-failure (both are deliberately ADR-deferred;
> see ADR 0007 §"PdfTable deferral" and ADR 0012 §"When to automate").

## Apply

From the repo root (`/Users/aben/RustroverProjects/situation_room`):

```
tar -xzf ~/Downloads/session23_anthropic_provider.tar.gz --strip-components=1 -C .
```

The tarball is layered on top of the Session 22 baseline (380 tests
green, ADR 0014 + plan-status everything wired). It is **additive**
to a clean Session-22 checkout — no existing recipe row, plan, fetch
run, or recipe-feedback row is touched, and the default boot
behaviour is bit-for-bit identical (`LLM_PROVIDER` defaults to
`"xai"`).

## What this ships

### P4 — Anthropic provider, real

The Anthropic Messages API has a meaningfully different wire shape
from xAI's OpenAI-chat-compatible endpoint. The provider speaks the
real shape directly through `SecureHttpClient`, with no SDK and no
second HTTP client.

| Layer | File | What changed |
|---|---|---|
| LLM | `crates/llm/src/providers/anthropic.rs` | **Promoted from 7-line stub to ~970-line provider.** Mirrors `grok.rs`'s structure (config → provider → trait impl → wire shapes → tests) but adapted for Anthropic's wire format: `system` is a top-level field (not a `role: "system"` message), auth header is `x-api-key` (not `Authorization: Bearer …`), every request carries `anthropic-version: 2023-06-01`, and structured output is delivered via forced tool use (`tools: [{name, input_schema}]` + `tool_choice: {type: "tool", name}`) rather than a `response_format` field. The structured payload comes back as `content[].input` of the matching `tool_use` block. Truncation-retry path is policy-equivalent to xAI's: when a structured-output request comes back with `stop_reason == "max_tokens"` and the original budget is below the 32K ceiling, retry once with doubled `max_tokens`. Plain-text requests do not retry on truncation (rationale documented inline). 21 unit tests covering body construction, response parsing (text, tool_use, mixed text+tool_use preamble, unknown block types), truncation signalling, env-driven model overrides, retry predicate corner cases, and HTTP error mapping. Plus 2 ignored live tests. |
| LLM | `crates/llm/src/providers/mod.rs` | Re-export `AnthropicProvider`, `AnthropicConfig`, `ANTHROPIC_API_KEY_ENV` alongside the xAI exports. |
| LLM | `crates/llm/src/lib.rs` | Top-level re-export for `AnthropicProvider` + module-level docstring updated to describe the provider catalog. |
| API | `crates/api/src/commands.rs` | `AppState.provider` lifted from `Arc<XaiProvider>` to `Arc<dyn LlmProvider + Send + Sync>` so the field can hold either provider concretely. The two `save_research_plan*` call sites that previously hardcoded `"xai"` as the lineage string now use `state.provider.id()`, so a plan classified by Anthropic persists with `classified_by = "anthropic"` (the trait method's stable identifier). All three downstream call sites (`classify_topic`, `reclassify`, the executor's `run_fetch_for_plan`) use `state.provider.as_ref()` which coerces from `&(dyn LlmProvider + Send + Sync)` to the `&dyn LlmProvider` they take — no other API-crate edits needed. The trait object spelling carries explicit `+ Send + Sync` because `dyn LlmProvider` alone isn't auto-trait-bounded (the trait declares them as supertraits, but for trait objects the auto-trait bounds must be spelled explicitly; tauri's `State<T>` requires `T: Send + Sync + 'static`). |
| Apps | `apps/desktop/src-tauri/src/main.rs` | New `pick_provider(http) -> Result<Arc<dyn LlmProvider + Send + Sync>>` helper that reads `LLM_PROVIDER` (default `"xai"`), constructs the matching concrete provider, and type-erases. An unrecognised value aborts boot rather than falling back, so a typo is loud. The `XaiProvider::from_env(http)` two-liner becomes a single `pick_provider(http)?` call; the rest of the composition root is unchanged. |
| Apps | `apps/situation_room/src/main.rs` | Same `pick_provider` helper (intentionally duplicated rather than shared — pulling it into a library crate would expose app-level boot decisions through a crate boundary). The CLI's `run_classify` now passes `provider.as_ref()` to `classify_topic` and `provider.id()` to `save_research_plan`. |
| Config | `.env.example` | New `LLM_PROVIDER=xai` line at the top of the LLM section, documenting the two valid values. New per-tier `ANTHROPIC_*_MODEL` and `ANTHROPIC_VERSION` env vars documented (commented out — empty/unset means "use defaults"). |

### What didn't change

- **The classifier and recipe-author prompts.** ADR 0014's signal
  lives in code, not in the LLM's view; the prompt-author and
  classifier prompts remain at v1.8 and v1.4 respectively. A
  provider switch is not a prompt revision.
- **The pipeline crates.** `classify_topic` and `author_recipe` take
  `&dyn LlmProvider`; that interface is unchanged. The trait
  receivers' implementation difference (xAI vs Anthropic) is fully
  contained in the LLM crate.
- **Storage / migration count.** No new migration. Plans persisted
  before this patch keep their `classified_by` exactly as it was.
- **The IPC surface.** No new Tauri commands, no DTO field changes,
  no ts-rs regeneration triggered — `cargo test -p
  situation_room-api` produces the same TypeScript output.
- **Anything frontend.** Zero `.svelte` / `.ts` changes.

## Why this scope

The Session 22 handoff named four priorities. P1 (live verification)
and P2 (failure_cases disposition) require an environment the
sandbox doesn't have — operator-machine network access and an
operator decision respectively. P3 (FetchReport chip) is explicitly
gated on P1 producing fresh evidence. **P4 (Anthropic provider) is
the only sandbox-runnable item**, and it's been a named carry-
forward in every handoff since Session 17.

The motivation goes beyond "land the carry-forward":

- **Single-provider lock-in is a real go-live risk.** A Session 13
  failure (xAI gateway truncation) was severe enough to motivate the
  truncation-retry path; a similar future failure with no fallback
  provider would block users. Two viable providers means the
  operator has a tested escape hatch.
- **The user's own notes name this as on-the-horizon work**:
  "potential migration to Claude or a higher xAI tier if LLM
  reliability issues persist." This patch makes that migration a
  one-line env-var change rather than a code change.
- **It pairs cleanly with `AppState.provider` generalisation,**
  which was a documented "if/when we support more providers" lift
  from Session 6.

I considered and rejected:

- **Wiring `pdf_table`.** ADR 0007's June-2026 review note (line
  507) explicitly defers it: "Pure-Rust positional PDF table
  extraction is a known hard problem … `PdfTable` extraction lands
  as its own focused session." Amendment 3 (`static_payload`) is
  the architecturally sanctioned workaround for the cases where
  it's needed. Implementing `pdf_table` now would have spent the
  session on the wrong axis.
- **Automating re-author-on-failure.** ADR 0012 has a hard 10-
  Class-B-failure gate ("**The single gate:** 10 or more empirically
  observed, distinctly-shaped Class B failures across diverse
  sources and plan types, documented in `docs/failure_cases/
  class_b/`. We do not have that yet."). The ADR's "READ THIS FIRST
  — DO NOT DEVIATE" block is unmistakable.
- **Back-filling pre-v10 `authored_from` rows.** Session 22
  explicitly forbade this: "retroactive truth claim, warrants its
  own decision."
- **A fourth recipe-card chip.** Session 22 §"Imperfection 3":
  "three is the visual ceiling; a fourth would warrant rethinking
  the layout." The only candidate fourth chip would be a per-
  provider "classified by Claude / Grok" tag, which would be both
  redundant (the data is already in `classified_by`) and a layout
  violation.

## Test count

Expected delta over the Session 22 baseline of **380**:

- llm (`anthropic.rs`): **+21** unit tests (build_body shape ×3,
  response parsing ×7, provider id, supported_tiers, model_for,
  config-from-env ×4, retry predicate ×5, http error mapping ×4)
- api: 0 (the `XaiProvider`-shaped tests don't exist; no new
  api tests added because the behaviour change — `provider.id()`
  for lineage — is observed by the existing live test, which
  remains `#[ignore]`)

That's **+21**, landing at **401** green, plus 2 new ignored live
tests (`live_anthropic_returns_nonempty_completion` and
`live_anthropic_returns_structured_json_when_schema_requested`).

## Verification the operator should run

```
cargo check --workspace
cargo test -p situation_room-llm                  # full llm suite, including new anthropic tests
cargo test -p situation_room-api                  # regenerates ts-rs files (same output)
cargo test --workspace                            # everything
cargo clippy --workspace --all-targets -- -D warnings
```

Then in the desktop app (no frontend changes, but verify the build
still succeeds end-to-end):

```
cd apps/desktop
npm run check
npm run dev          # default — uses XAI_API_KEY as before
```

To test the new Anthropic path on a network-enabled machine, set
`LLM_PROVIDER=anthropic` and `ANTHROPIC_API_KEY=…` in `.env`, then:

```
# Live test the provider in isolation:
cargo test -p situation_room-llm --ignored live_anthropic

# Or run the situation-room CLI against Anthropic:
LLM_PROVIDER=anthropic cargo run -p situation_room \
    -- "lithium supply chain"
```

The plan that lands in `situation_room.duckdb` will have
`classified_by = "anthropic"`, distinguishable from earlier xAI-
classified plans without any UI surface change (the field exists
already; nothing renders it specially yet).

## Hard rules honored

- **xAI API key:** never read, written, or referenced anywhere in
  the new Anthropic path. The two providers are independent — the
  Anthropic provider reads `ANTHROPIC_API_KEY` only.
- **Anthropic API key:** never read, written, or referenced outside
  the provider's `from_env` boundary. `ApiKey::from_env_optional`
  enforces the same min-16-char + non-placeholder rules as the xAI
  path. The header value is wrapped in `SecretString` and marked
  `set_sensitive(true)` by `SecureHttpClient::post_json_bytes`, so
  reqwest-internal logging redacts it.
- **ADR 0009 §"The rule":** no fresh `reqwest::Client::new()`. The
  Anthropic provider takes a `SecureHttpClient` parameter exactly
  like xAI; the desktop binary builds one and clones it for the
  provider + the executor.
- **ADR 0007 runtime-is-LLM-free invariant:** unchanged. The
  provider is a Level-1/Level-2 component; the runtime path doesn't
  invoke either provider on refresh.
- **ADR 0011 plan lifecycle:** unchanged. No transitions added.
- **ADR 0013 recipe-feedback channel:** unchanged.
- **Closed extraction-mode enum:** unchanged. Provider selection is
  orthogonal to extraction.
- **No new dependency.** The patch adds zero crates. The Anthropic
  provider uses `serde`, `serde_json`, `async-trait`, `tracing`,
  `situation_room-secure`, and the existing trait — every one
  already in the workspace dependency tree.
- **Bounds checking on IPC string inputs:** unchanged. The provider
  enforces `Bounds::LLM_PROMPT_BODY` on system + user prompts at
  send time, same as xAI.
- **Generated TS files:** untouched. No DTO surface change.
- **Migrations:** unchanged. No schema change.

## Followups for next session

- **Live xAI verification (P1, still carry-forward).** Now also
  paired with: live verification of the Anthropic path against the
  same three slots. Picking a single plan, classifying it once
  with each provider, and diffing the resulting plans is the
  cheapest A/B test of provider quality.
- **EUR-Lex banner (P2, still carry-forward).** Pending operator
  decision on the failure_cases directory's commit posture.
- **`SourceDescriptor` consolidation.** The CLI and the desktop
  binary both define a `SourcesFile` / `SourceEntry` pair locally
  and call `load_source_descriptors` locally; the two are word-for-
  word identical. Lift to a small `apps_common` crate or to the
  pipeline crate's `research_classifier` module. Defer-to-when-it-
  matters territory; flagged because the duplication is now visible
  next to the new `pick_provider` helper which **is** intentionally
  duplicated for the architectural reason described above.
- **Class B failure documentation.** ADR 0012 §"Documenting observed
  Class B failures" is the gate to any future re-author automation.
  The directory `docs/failure_cases/class_b/` exists in the ADR but
  not in the tree; either commit a `.gitkeep` so future failures
  have a home, or leave it for the first real Class B run to
  create. Either is fine; flagging the gap.
- **OpenAI / Gemini providers.** Both still stubs. Same scaffolding
  pattern as Anthropic; lower priority because xAI + Anthropic
  cover the architectural concern (single-provider lock-in).
- **`SecureHttpClient` response headers.** Multi-session carry-
  forward. Lights up Anthropic's `retry-after` header on 429s
  exactly as it does for xAI's.
