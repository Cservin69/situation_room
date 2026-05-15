#!/usr/bin/env bash
# Session 74 — verify the bundled push of three Session-73-handoff
# candidates:
#
#   1. config/prompts/recipe_author.md            (v1.21 → v1.22)
#         Per-call placeholders moved to a `## Concrete inputs` section
#         at the end of the file so the cacheable prefix grows
#         ~3% → ~92%. The narrative refs ("above"/"below") that pointed
#         at the now-relocated placeholders are updated in place.
#
#   2. crates/pipeline/src/{fetch_classes.rs,ua_policies.rs,
#        http_fetcher.rs,fetch_backoff.rs,fetch_executor.rs}
#         ADR 0009 amendment 2 wire-up. `ua_policy_for_host` reads
#         through `host_class_override` (now `pub(crate)`) → maps to
#         `UaPolicy` → resolves to a per-request UA override at the
#         `fetch_recipe_bytes` call site.
#         `fetch_with_backoff_ua` is the UA-aware twin of
#         `fetch_with_backoff`; the trait gains
#         `fetch_bytes_with_meta_ua` with a default impl so test
#         fetchers stay unchanged.
#         Activation still gated on `HOST_CLASS_OVERRIDES` being
#         empty — production behaviour is byte-for-byte unchanged.
#
#   3. crates/llm/src/providers/{trait_def.rs,grok.rs,anthropic.rs}
#        + crates/pipeline/src/fetch_executor.rs (6 stub sites)
#         `CompletionResponse.cached_input_tokens: Option<u32>`.
#         xAI projects from `prompt_tokens_details.cached_tokens`
#         (Session 72 plumbed the parse; Session 74 surfaces it onto
#         the response). Anthropic projects from
#         `cache_read_input_tokens` (no-op in practice until
#         request-side cache_control breakpoints land).
#
#   4. apps/desktop/src/lib/dashboard/document_table.ts + .test.mjs
#         Session 74.5 mid-session hot-fix: extend the detector to
#         recognise object-of-scalars (JSON-stat / lookup-map shape)
#         alongside array-of-objects, so Eurostat-style
#         `{value: {key: scalar}}` payloads render as a 2-col
#         Key/Value table instead of dropping to raw-JSON fallback.
#         array-of-objects still wins when both shapes are present.
#
# What this runs:
#   [1/4] cargo check --workspace
#         — workspace-wide check; catches stub-construction-site
#           drift in any downstream crate that builds a
#           CompletionResponse in a test fixture this session
#           didn't enumerate. Verifies the new
#           `fetch_with_backoff_ua` / `fetch_bytes_with_meta_ua`
#           paths compile end-to-end.
#   [2/4] cargo test -p situation_room-pipeline -p situation_room-llm
#         — runs the new ua_policy_for_host test plus the new
#           fetch_with_backoff_ua identity + override-tolerance tests,
#           and confirms the pre-existing test suite still passes
#           after the CompletionResponse field addition.
#   [3/4] tsc + node detector regression
#         — compiles document_table.ts to /tmp/dt-build and runs the
#           extended node assertions (Session 73 originals + Session
#           74.5 object-of-scalars cases: Eurostat shape, mixed-shape
#           preference, bare object-of-scalars, single-entry rejection,
#           mixed-scalar+object rejection).
#   [4/4] cd apps/desktop && npm run check
#         — operator-run on Mac (sandbox node_modules has Mac-built
#           rollup binaries that can't load on Linux). The prompt edit
#           is markdown, but the desktop tauri main embeds the prompt
#           via `include_str!`; check makes sure the embed still
#           parses as a string literal at the include site.
#
# Pass criteria: EXIT=0 on the last line of each log.
#
# Operator notes (cost-discipline lever):
#   The v1.22 prompt restructure is observable in production only by
#   reading `cached_input_tokens` on the next non-eval authoring call
#   pair. Expectation: the second call against the same source family
#   reports `cached_input_tokens` ≈ 92% of `input_tokens`. If the
#   ratio stays at ~3% the restructure didn't land where the cache
#   key lives — re-check that no per-call randomness (timestamp, UUID
#   in framing prose) leaked above the `## Concrete inputs` section.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/3] cargo check --workspace"
# Workspace-wide check catches stub-construction-site drift in any
# downstream crate (apps/api/desktop) that builds a CompletionResponse
# in a test fixture this session didn't enumerate.
(cargo check --workspace --all-targets 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session74-cargo-check-${STAMP}.log"

echo
echo "[2/4] cargo test -p situation_room-pipeline -p situation_room-llm"
(cargo test -p situation_room-pipeline -p situation_room-llm 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session74-cargo-test-${STAMP}.log"

echo
echo "[3/4] tsc + node detector regression (Session 74.5)"
rm -rf /tmp/dt-build
(cd apps/desktop && \
  ./node_modules/.bin/tsc \
    --target es2022 --moduleResolution bundler --module esnext \
    --strict --skipLibCheck \
    --outDir /tmp/dt-build \
    src/lib/dashboard/document_table.ts 2>&1 && \
  node src/lib/dashboard/document_table.test.mjs 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session74-detector-test-${STAMP}.log"

echo
echo "[4/4] svelte-check (apps/desktop)"
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") \
  | tee "logs/session74-svelte-check-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session74-cargo-check-${STAMP}.log"
echo "  logs/session74-cargo-test-${STAMP}.log"
echo "  logs/session74-detector-test-${STAMP}.log"
echo "  logs/session74-svelte-check-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Cache-hit verification (after the next non-eval authoring call):"
echo "  Grep the desktop INFO log for 'xai: completion done'."
echo "  First call against a source family reports cached_tokens=Some(0)"
echo "  (cold prefix); second call reports cached_tokens=Some(n) where"
echo "  n / input_tokens is the cache-hit ratio. Pre-v1.22 baseline:"
echo "  ratio ~ 0.03. v1.22 target: ratio ~ 0.92 on cacheable prefix."
echo
echo "UA-policy wire-up verification (no live activation expected):"
echo "  Run any fetch. The desktop INFO log should NOT contain"
echo "  'ua_policy: applying per-request override' — HOST_CLASS_OVERRIDES"
echo "  is empty, so every host resolves to UaPolicy::Default and the"
echo "  override branch is skipped. The wire-up is plumbing-only until"
echo "  the override map is populated."
