# STOCKPILE — Session 24 handoff

You are starting Session 24. **This handoff supersedes the one
shipped with Session 23**, because we ran the live verification
already — at the end of Session 23 — and what surfaced reshapes the
priority list. The Session-23-shipped handoff was written before the
verification; this one was written after it. Trust this one.

Read this whole document before writing any code. The ADRs in
`docs/adr/` are still authoritative — this handoff is the layer
above them, not a replacement.

## What Session 23 + 23.1 actually shipped

Session 23 (the main patch):

- **Anthropic provider, real.** `crates/llm/src/providers/anthropic.rs`,
  ~970 lines, mirrors `grok.rs`'s structure adapted for the Messages
  API (top-level `system`, `x-api-key` auth, `anthropic-version`
  header, structured output via forced tool use, `stop_reason ==
  "max_tokens"` truncation signal). 21 unit tests, 2 ignored live
  tests. The truncation-retry path is policy-equivalent to xAI's.
- **`AppState.provider` lifted** from `Arc<XaiProvider>` to
  `Arc<dyn LlmProvider + Send + Sync>` so the binaries can pick
  either provider concretely. Two hardcoded `"xai"` lineage strings
  swapped for `state.provider.id()`.
- **`pick_provider(http)` helper** in both binaries reading
  `LLM_PROVIDER` (default `"xai"`), constructing the matching
  concrete provider, type-erasing. Unrecognised values abort boot.
- **`.env.example`** documents `LLM_PROVIDER` and the new
  `ANTHROPIC_*_MODEL` / `ANTHROPIC_VERSION` overrides.

Session 23.1 (the amendment, which you should also have applied):

- **One-line fix in the desktop `invoke_handler!` macro** registering
  `commands_records::records_for_plan`. Session 22 added the command
  but missed the registration; the operator only saw the failure
  ("storage: Command records_for_plan not found") when they
  exercised the records pane after the Session 23 verification run.
- **`scripts/check_tauri_commands_registered.sh`** — shell guard
  that fails non-zero if any `#[tauri::command]` in
  `crates/api/src/commands*.rs` isn't registered in
  `apps/desktop/src-tauri/src/main.rs`. Prints offending names with
  remediation. Sub-second. Not yet wired into a make / just / xtask
  target — see Followup 2 below.

Both rolls are clean against the Session 22 baseline. Session 23.1's
desktop main.rs supersedes Session 23's; if you have both tarballs
extracted, the second extract overwrites the first's `main.rs` with
the merged version (pick_provider + records_for_plan registered).

## What Session 23 *also* shipped, by accident — the verification run

The operator ran the full live flow at the end of Session 23 (xAI
provider, fresh classify of "south-korea elections", accept,
run-fetch). What landed is the most useful single piece of empirical
evidence the project has produced about end-to-end behaviour. Both
authored recipes ended `authored_from = "stub_excerpt"` (ADR 0014's
STUB-AUTHORED chip showing on both) for two distinct reasons:

| Source | Why stub-authored |
|---|---|
| `rss_feeds` | **No `endpoint_hint` in `config/sources.toml`.** The recipe author had only the source description to go on. It invented `https://www.yna.co.kr/rss` (Yonhap News's RSS endpoint), which returned HTTP 400 at fetch time. |
| `gdelt` | **`endpoint_hint` rate-limited from the operator's IP with HTTP 429** during the Session-10 Option-F pre-fetch. The author fell back to the stub-excerpt path. The recipe extracts `$.articles[0].title` — a single news article title, fetched successfully but matching none of the plan's expectations (`polling_support`, `voter_turnout`, `election_held`, etc.). |

The system reported all of this honestly:

- Both recipes carry the STUB-AUTHORED chip — ADR 0014 is doing its
  job.
- The fetch report says 1/2 succeeded, 1 record produced (the gdelt
  title) — accurate.
- The records pane (after the 23.1 fix) shows the one record,
  empty everywhere else.

**Read this run as evidence, not as breakage.** The diagnosis is
sharper than any test fixture could have produced. The system
correctly surfaced two distinct, real failure shapes and the chip
mechanic worked.

## Session 24 priorities, reshaped by the verification

### P1 — Endpoint-hint coverage in `config/sources.toml`

Single highest-leverage move. Walk every entry in
`config/sources.toml`. For each one without an `endpoint_hint`,
either:

- **Add a hint** if a sensible default URL exists (most sources
  have one — even a homepage works as a "structure-discovery"
  excerpt for the recipe author).
- **Document why none was added** in the description text, so the
  next operator (and the next prompt revision) knows the omission
  is deliberate.

The verification proved that *no hint at all* is functionally
worse than *a partially-stale hint*: the LLM's guesses are
random-conditional-on-source-name, while a real fetched page —
even one whose precise field structure has shifted — at least
anchors the author in real bytes.

The `rss_feeds` entry is the canonical example. It is a *category*
of source, not a *source*. Either it should be split into specific
RSS-publishing sources (e.g. `yna_rss`, `bbc_rss`,
`reuters_world_rss`) each with a real URL, or it should carry a
hint URL that exposes a representative RSS structure the author
can pattern-match against.

**Scope cap:** this is config-only. No code changes, no prompt
changes, no migration. One PR per ~5-10 sources is fine; no need
to do all 12 in one shot. Test by re-classifying any topic that
nominated the previously-thin source and verifying the recipe
arrives without the STUB-AUTHORED chip.

### P2 — Live A/B verification of providers (carry-forward, broadened)

Now that both providers are real, the operator can pick any plan
and run it through both. Cheapest concrete shape:

1. Pick a topic that nominates 3–5 sources spanning multiple
   shapes (one HTML, one JSON API, one with a hint URL, one
   without). "EU AI Act enforcement" or "Hungarian sovereign
   debt" both fit; the operator's earlier verifications targeted
   them.
2. Run with `LLM_PROVIDER=xai` first. Save the resulting
   `ResearchPlan` JSON.
3. Run with `LLM_PROVIDER=anthropic`. Save the JSON.
4. Diff. The diff should be small and qualitative: same six
   buckets populated, comparable topic_tags, similar geographic
   scope. Surprises (very different bucket counts, very different
   source nominations) are the interesting signal.

If the diff produces evidence motivating a v1.5 / v1.9 prompt bump
or a per-provider prompt fork, that's a Session-25-shaped decision,
not the same patch. Keep this session's output as a saved diff
plus a short observation note.

### P3 — Recipe-author prompt: refuse rather than guess on stub-excerpt

The Session 23 verification surfaced an architectural question that
ADR 0014 didn't fully answer: *should the prompt be allowed to
produce a recipe at all when only a stub excerpt is available?*

Today: yes. The chip surfaces the provenance for human review.
Stricter: no. The author returns a structured "no recipe — pre-
fetch failed" outcome; the fetch executor records it as an
authoring failure rather than a stub-authored success.

Arguments for stricter:

- A stub-authored recipe wastes a fetch round-trip almost certain
  to produce noise (the gdelt case: 1 record with the wrong field).
- The records pane currently shows one technically-correct record
  that is functionally garbage; the operator has to know about
  ADR 0014 and read the chip to understand why.

Arguments against:

- ADR 0014's whole *point* is that the chip is the discipline.
  Refusing in the author shifts the discipline from operator-
  visible (chip + record + judgment) to operator-invisible
  (no record at all).
- A stub-authored recipe sometimes works. The Session 11 / 12
  sequence had cases where the LLM's invented URL was correct.

This is **ADR-amendment territory**, not a quick prompt tweak.
Session 24 should observe and decide, not implement. If the operator
wants to try stricter, the right shape is:

- A new field on the recipe-author wire output:
  `stub_excerpt_response: "produced_recipe" | "refused"`.
- A new `RecipeOutcome` variant (or an extension of an existing
  one) for the executor when the author refused.
- An ADR amendment on 0014 documenting the new option and the
  reason for the choice.

If this lands, it's a focused session on its own.

### P4 — Wire `check_tauri_commands_registered.sh` into CI

Session 23.1 added the script. The next time the operator wires up
a make / just / xtask check pre-tag, this script should be in it.
The class of bug it catches has now bitten once (records_for_plan);
without the guard, it can bite again any time a new
`#[tauri::command]` lands.

Specifically:

- **If there's a Justfile or Makefile** at the repo root: add a
  `check-tauri` target that runs the script. Add it to whatever
  the operator's "run before commit" target is.
- **If there's no such file**: write a tiny Justfile with two
  targets (`check-tauri` and `check`, where `check` runs both
  this and the cargo / clippy / fmt sequence). Five lines.

### P5 — Lift the duplicated `SourcesFile`/`SourceEntry` loader

The CLI and the desktop binary both define a `SourcesFile` /
`SourceEntry` pair locally and call a local `load_source_descriptors`.
The two are word-for-word identical. Session 23 made the duplication
more visible by adding a *deliberately-duplicated* `pick_provider`
helper alongside it; the principle that justifies duplicating
`pick_provider` (app-level boot decisions don't belong in library
crates) does **not** apply to source-descriptor loading (that's
shared code with no app-specific decisions).

Lift to a small `apps_common` crate. Cheap. Worth doing before any
further duplication accumulates.

### P6 — `docs/failure_cases/class_b/` `.gitkeep`

ADR 0012's evidence-base directory still doesn't exist in the tree.
A `.gitkeep` is a one-line patch and lowers the friction for the
first session that documents a real Class B failure. Worth doing
as a 30-second follow-up alongside any other patch.

### Lower-priority carry-forwards

- **EUR-Lex CELEX failure-case banner** (P2 from Session 22, still
  blocked on operator's posture decision about
  `apps/desktop/failure_cases/`).
- **OpenAI / Gemini providers** still stubs. Same scaffolding
  pattern as Anthropic; lower priority than P1–P3 because the
  architectural concern (single-provider lock-in) is now addressed.
- **`SecureHttpClient` doesn't surface response headers** —
  multi-session carry-forward. Lights up `retry-after` for both
  providers' rate-limited responses.

## What Session 24 is explicitly NOT

- **Not implementing automated re-author-on-failure.** ADR 0012's
  10-Class-B-failure gate is unmet. Don't pre-empt. The single
  Class B observation from Session 23's verification (gdelt's
  rate-limit-induced stub) is *one* data point, not ten.
- **Not wiring `pdf_table`.** ADR 0007 defers it. The
  `static_payload` path covers the operative cases.
- **Not back-filling pre-v10 `authored_from` rows.** Same reasoning
  as Session 22: retroactive truth claim, warrants its own
  decision.
- **Not adding a fourth chip to the recipe head.** Three is the
  ceiling.
- **Not bumping the recipe-author or classifier prompts.** P2
  might surface evidence motivating a bump; P3 might motivate an
  amendment. Do those as deliberate next-session shapes.
- **Not adding a second LLM call site in the runtime path.**
  ADR 0007 §"runtime path" is unambiguous.

## Hard rules (carry-over, unchanged)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All HTTP
  through `SecureHttpClient`.
- Bounds checking on every IPC string input. `check_user_text` is
  the gate for user-supplied text.
- Tauri commands return `CommandError`, not internal error types.
- **Every new `#[tauri::command]` requires two edits**: define it
  in `crates/api/src/commands*.rs`, register it in
  `apps/desktop/src-tauri/src/main.rs::generate_handler!`. The
  Session 23.1 CI guard catches the missed second edit; until the
  guard is wired into CI (P4), it's a manual checklist item.
- Generated TS files in `apps/desktop/src/lib/api/types/` written
  by ts-rs via `cargo test -p situation_room-api`. Never hand-edit
  except as a temporary measure shipped alongside the matching
  Rust change.
- ts-rs DTOs and pipeline structs are mirrored, not shared.
- Components only use CSS vars from `global.css`. No hardcoded
  hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- API keys (xAI **and** Anthropic) never read, written, or
  referenced anywhere visible. `ApiKey::from_env*` only.
- Migrations: read the prior migration's comment block before
  writing the next.

Standing-order priority: **security > generalisation > simplicity**.

## Test count

Expected baseline: **401** (Session 22's 380 + Session 23's 21
anthropic tests). Session 23.1 adds zero test count — the CI guard
is shell, outside cargo's test surface.

## First thing to do in Session 24

1. Read this file.
2. Run `bash scripts/check_tauri_commands_registered.sh` to confirm
   the registration list is consistent. If it isn't, that's the
   first thing to fix — anything else stacks on a broken IPC
   surface.
3. Run `cargo test --workspace` to confirm the **401**-test
   baseline. If you see fewer, chase the failing test name first.
4. Pick P1 or P2 with the operator. P1 is fast and cheap (config
   + verify); P2 is a network-latency cost (two classifications)
   but produces more evidence per minute. Either is a good
   single-session shape.

## Continuity note

The continuity note from Sessions 19–23 still applies. The
operator is rigorous about security ("paranoid about security" —
earned, not affected), prefers honesty about uncertainty over
false confidence, reacts well to direct disagreement when
warranted, and has explicitly asked for "do not deviate"
discipline.

The Session 23 retrospective lesson: **even a one-line miss in a
macro can hide for two sessions because both compilers (Rust and
TypeScript) accept it.** Tauri's runtime registration model has a
gap where the build chain doesn't catch the registration
mismatch. The CI guard from 23.1 closes that gap; the lesson for
the next reader is: *be specifically wary of macro-list
registrations that the type system cannot verify.* When you add a
new `#[tauri::command]`, the second edit is on the manual
checklist until P4 lands.

The codebase has a strong existing style. Read three files in
any crate before writing a fourth. The hardest part of
contributing well here is matching the existing voice in the
code comments and the ADR cross-references — the comments aren't
decoration, they're load-bearing for the next reader.

End of handoff.
