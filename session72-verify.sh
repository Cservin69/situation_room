#!/usr/bin/env bash
# Session 72 — verify xAI prompt-caching plumbing:
#   1. `x-grok-conv-id` header on every xAI request (cache locality).
#   2. `usage.prompt_tokens_details.cached_tokens` parsed + logged at
#      INFO so the operator can see cache hits in tail logs.
#
# What this runs:
#   [1/2] cargo test -p situation_room-llm grok
#         — exercises the new tests in providers/grok.rs:
#           * conv_id_defaults_to_a_uuid4_when_env_is_absent
#           * conv_id_picks_up_env_override_when_set
#           * conv_id_env_treats_empty_string_as_unset
#           * conv_id_env_treats_whitespace_only_as_unset
#           * conv_id_is_stable_across_repeated_reads
#           * with_conv_id_overrides_the_construction_default
#           * parse_response_projects_cached_tokens_from_prompt_tokens_details
#           * parse_response_handles_missing_prompt_tokens_details
#           * parse_response_handles_present_details_with_zero_cached_tokens
#           * parse_response_handles_details_without_cached_tokens_field
#
#   [2/2] cargo check --workspace
#         — confirms the parse_response signature change (now takes a
#           ModelTier param) didn't break any caller across the
#           workspace.
#
# Both stages tee to logs the agent reads back. Sentinel `EXIT=N` on
# the last line of each log lets the agent tell green from streaming.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/2] cargo test -p situation_room-llm grok"
(cargo test -p situation_room-llm grok 2>&1; echo "EXIT=$?") \
  | tee "logs/session72-cargo-test-${STAMP}.log"

echo
echo "[2/2] cargo check --workspace"
(cargo check --workspace 2>&1; echo "EXIT=$?") \
  | tee "logs/session72-cargo-check-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session72-cargo-test-${STAMP}.log"
echo "  logs/session72-cargo-check-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Live exercise — confirm the cache is hitting:"
echo "  cargo run -p situation_room-desktop"
echo "  Run a fetch on any plan (e.g. the existing TESLA plan); each"
echo "  xAI completion now emits an INFO log line:"
echo "    xai: completion done tier=Cheap model=grok-4.3 \\"
echo "      input_tokens=Some(N) cached_tokens=Some(M) \\"
echo "      output_tokens=Some(K) conv_id=<uuid>"
echo "  On the FIRST run of a session, cached_tokens will be Some(0)"
echo "  (cache miss — first request to the routed server)."
echo "  On SUBSEQUENT runs against the same plan in the same process,"
echo "  cached_tokens should be > 0 for any request whose system +"
echo "  user prefix matched a prior one — typically the longest stable"
echo "  prefix of each authoring prompt."
echo
echo "Cross-process eval pinning (the big lever for eval cost):"
echo "  export XAI_CONV_ID=\$(uuidgen)"
echo "  for i in 1 2 3 4 5; do bash run-one-trial.sh; done"
echo "  Trial 1 warms the per-server cache; trials 2-5 hit it. Without"
echo "  XAI_CONV_ID exported, each process gets a fresh uuid4 and trials"
echo "  may land on different servers (cold cache each)."
echo
echo "Boot-log sanity check — the provider line now carries conv_id:"
echo "  xai: provider configured frontier=grok-4.3 workhorse=grok-4.3 \\"
echo "    cheap=grok-4.3 frontier_effort=High workhorse_effort=Medium \\"
echo "    cheap_effort=Low conv_id=<uuid>"
