# Session 43 — Patch 1

Piece A from the Session 43 handoff: reasoning-effort plumbing for
xAI cost-tier differentiation. Drive-by: `apps_common` tempdir race.

## Apply

Files were edited in place. To verify:

```
cd ~/Documents/Claude/Projects/SituationRoom
(cargo build --workspace 2>&1; echo "EXIT=$?") | tee build.log
(cargo test --workspace 2>&1; echo "EXIT=$?") | tee test.log
```

`code/` empty directory at repo root was already absent — drive-by
moot.

## Files changed

- `crates/llm/src/providers/trait_def.rs` — new `ReasoningEffort`
  enum (`Low | Medium | High`); new
  `CompletionRequest::reasoning_effort: Option<ReasoningEffort>`
  field with rustdoc naming the per-tier-mapping precedence and the
  per-source-routing footgun.
- `crates/llm/src/providers/mod.rs` — re-export `ReasoningEffort`.
- `crates/llm/src/providers/grok.rs` — `XaiConfig` gains
  `frontier_effort`, `workhorse_effort`, `cheap_effort` (defaults
  `High`/`Medium`/`Low`); three new env vars
  (`XAI_FRONTIER_EFFORT`, `XAI_WORKHORSE_EFFORT`, `XAI_CHEAP_EFFORT`)
  with empty/whitespace-only/unrecognised-value posture mirroring
  the existing model-string env vars; `effort_for(tier)` accessor;
  `parse_effort` and `effort_wire_str` helpers; `build_body` adds
  `reasoning_effort` to the chat/completions wire body driven by
  request-override-or-tier-mapping; truncation-retry preserves the
  field; INFO boot log gains the three effort values; default-config
  comment block rewritten to reflect that the cost lever is now the
  effort parameter, not a model string. Eight new unit tests.
- `crates/llm/src/providers/anthropic.rs` — module docs explain the
  field is intentionally a no-op for Anthropic Messages
  (no equivalent wire parameter today; do not synthesise one);
  retry-path and test/live-test `CompletionRequest` constructions
  updated to carry the new field.
- `crates/pipeline/src/propose_source_url.rs`,
  `crates/pipeline/src/research_classifier.rs`,
  `crates/pipeline/src/recipe_author.rs` — three caller sites set
  `reasoning_effort: None` so per-tier mapping is the sole
  authority; comment at each site names which tier and which
  default applies, and re-states the per-source-routing
  prohibition.
- `crates/apps_common/src/sources.rs` — `tempdir()` now layers
  `SystemTime::now().as_nanos()` with `thread::current().id()` and a
  process-wide `AtomicUsize` counter; doc comment names the
  Session 41 patch 2 flake that this addresses.

No changes to schemas, DTOs, prompt files, or anything outside the
LLM crate's plumbing + the apps_common drive-by.

## What this patch does

### Item A — reasoning-effort plumbing

xAI consolidated their lineup in May 2026; `grok-4.3` is the
universal recommendation across tiers. The model-string lever Session
42 patch 4 left in place (`frontier=workhorse=cheap=grok-4.3`) is
correct against the catalog but means cheap-tier propose-URL paid
the same per-token rate as a frontier-tier authoring call. xAI's
`/v1/chat/completions` endpoint accepts a per-request
`reasoning_effort` parameter with values `low | medium | high`; that
parameter is the cost lever Session 43 plumbs through.

**Architecture.** Three layers, each with a clear owner of the
final value:

1. **Trait shape.** `CompletionRequest::reasoning_effort:
   Option<ReasoningEffort>` carries an *optional* per-call
   override. The normal path is `None`; per-tier mapping decides.
2. **Provider config.** `XaiConfig::{frontier,workhorse,cheap}_effort`
   carries the per-tier mapping. Defaults are `High`/`Medium`/`Low`
   — frontier authoring deserves the deep think; cheap propose-URL/
   classification runs fast and cheap.
3. **Wire mapping.** `XaiProvider::build_body` picks
   `req.reasoning_effort.unwrap_or_else(|| config.effort_for(tier))`
   and serialises the result as the flat `reasoning_effort` field
   on the chat/completions JSON body.

**Per-source routing is forbidden** by both the
`ReasoningEffort` rustdoc and inline comments at every caller.
"If URL is X, use High" is the failure mode the operator has caught
more than once across sessions; per-tier mapping is fine, per-source
is not. The LLM is the only specialist that decides what each
source needs; we only decide what the *tier* should cost-budget for.

**Sibling providers** (Anthropic Messages, OpenAI/Gemini stubs)
ignore the new field rather than pretend to honor it. The
Anthropic provider's module docs name the policy explicitly:
synthesising a knob with no wire effect would mislead callers into
thinking they have cost differentiation here when they do not.
If Anthropic later adds an equivalent parameter, the change lands
in `build_body`; the trait shape already carries the field.

**Wire format choice.** xAI's chat/completions endpoint accepts the
flat `reasoning_effort` field (OpenAI-compat shape). The newer xAI
Responses API (`/v1/responses`) uses a nested object
(`{"reasoning":{"effort":...}}`) with an `input` field replacing
`messages`. We post to chat/completions, so the flat form is what
ships. If a live `grok-4.3` run shows the legacy endpoint silently
ignoring the parameter, the architectural follow-up is migrating
the xAI provider to the Responses API — that change touches the
endpoint, the body shape, and the response parser, so it is its own
session per the handoff's bundle rule. The `effort_wire_str` helper
and the single comment in `build_body` are the only places that
change in that future migration.

**Env override posture.** Three new env vars
(`XAI_FRONTIER_EFFORT`, `XAI_WORKHORSE_EFFORT`, `XAI_CHEAP_EFFORT`)
follow the same posture as the existing model-string env vars from
Session 13 Improvement B: empty / whitespace-only / unrecognised
value all degrade to the tier's default rather than crashing the
provider on a 400 from the gateway. Operators conditionally
exporting these from a shell script don't need to worry about a
typo turning into a runtime crash. `clear_model_envs` (the test-
helper) was extended to clear the three new vars too, so the
existing model-env tests stay isolated from the new effort tests.

### Item C drive-by — `apps_common` tempdir race

`crates/apps_common/src/sources.rs::tests::tempdir()` previously used
`SystemTime::now().as_nanos()` as its sole entropy source. On modern
hardware, two parallel `cargo test` threads can enter the helper
within the same nanosecond, collide on the directory name, and
manifest as the flaky `load_source_descriptors_respects_limit`
failure ("left: 'a' / right: 'wb'") Session 41 patch 2 saw. The fix
layers three sources of uniqueness: nanos (cross-process),
`thread::current().id()` (cross-test within a process), and a
process-wide `AtomicUsize` counter (cross-test on the same thread,
rare but possible under nextest). The thread id is sanitised
(non-alphanumeric → `_`) so unusual `Debug` formats can never
introduce a path-illegal char.

## Tests

New unit tests in `grok.rs`:

- `build_body_emits_per_tier_reasoning_effort_from_default_config` —
  pins frontier→`"high"`, workhorse→`"medium"`, cheap→`"low"` on
  default config.
- `build_body_request_level_effort_overrides_tier_mapping` — request-
  level `Some(Low)` against the `Frontier` tier emits `"low"` on the
  wire; the override path is the per-call escape hatch.
- `build_body_uses_provider_config_effort_when_request_is_none` —
  exercises a *non-default* `XaiConfig` so a regression that wires
  Frontier to Low silently can't pass on the same value as the
  default.
- `xai_config_default_assigns_high_medium_low_per_tier` — pins the
  policy so a future "let's set everything to medium for safety"
  silent edit fails this test.
- `effort_for_maps_each_tier_to_its_configured_intensity` —
  parallel of the existing `model_for` test for the new lever.
- `effort_wire_str_emits_low_medium_high_strings` — pins the exact
  spelling the wire is sensitive to.
- `parse_effort_accepts_low_medium_high_case_insensitively` —
  shell-script export tolerance.
- `parse_effort_rejects_unknown_values_returning_none` — empty,
  whitespace, partial spellings (`lo`/`med`), and would-be aliases
  (`none`/`disabled`/`extreme`) all return `None`.
- `xai_config_from_env_picks_up_effort_overrides` — env-driven
  override path.
- `xai_config_from_env_treats_empty_effort_string_as_unset` and
  `xai_config_from_env_treats_whitespace_only_effort_as_unset` —
  posture parity with the model-string env tests.
- `xai_config_from_env_unrecognised_effort_value_falls_back_to_default`
  — typo degrades to default rather than crashing.

All new tests use the existing `ENV_LOCK`/`clear_model_envs`
discipline so they compose cleanly with the Session 13 model-env
tests.

The Anthropic provider has no new tests: ignoring a field is the
absence of behavior, not a behavior to pin. Existing tests (which
already pin every other body-shape concern) confirm by passing
unchanged that the field's presence on the request doesn't perturb
the wire body.

The pre-existing `live_xai_*` and `live_anthropic_*` ignored tests
inherit the new field as `None` and continue to exercise the
default per-tier mapping; running them with `XAI_API_KEY` in `.env`
will confirm that grok-4.3's chat/completions endpoint accepts the
new body field without 400'ing.

## What to expect

Boot log on next launch (with no env overrides):

```
xai: provider configured frontier=grok-4.3 workhorse=grok-4.3 cheap=grok-4.3 frontier_effort=High workhorse_effort=Medium cheap_effort=Low
```

Live runs against xAI should now:

- Charge cheap-tier propose-URL and classification calls at the
  Low reasoning intensity on grok-4.3, dropping per-call cost
  vs. the pre-patch state where every tier paid frontier prices
  on the same model.
- Continue to spend High intensity on frontier-tier recipe
  authoring where the deep think is worth the extra tokens.

## Out of scope (still / carried)

- The PDF prefetch truncation gap — Session 44+, its own session
  with the operator-picked design (two-pass authoring vs.
  TOC-aware excerpting).
- Network-layer issues from Session 40 (SEC user-agent placement,
  Reuters defunct-or-blocked, `industry.gov.au` timeouts) —
  Session 44+ per the handoff's hard rule that this is its own
  session.
- xAI Responses API migration — only becomes architecturally
  necessary if a live grok-4.3 run shows chat/completions silently
  ignoring `reasoning_effort`.
