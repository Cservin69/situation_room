#!/usr/bin/env bash
# Session 75 — verify the bundled push of Session-74-handoff
# candidates 1 + 5 + groundwork for 2:
#
#   1. crates/llm/src/cost_ledger.rs (new) + lib.rs exports
#      crates/api/src/{commands.rs,types_export.rs}
#      apps/desktop/src-tauri/src/main.rs
#      apps/desktop/src/lib/api/{client.ts,types/LlmCostLedgerEntryDto.ts,
#        types/ModelTierDto.ts}
#      apps/desktop/src/components/CostByTierPanel.svelte (new)
#      apps/desktop/src/routes/+page.svelte + components/PlanReview.svelte
#         — `CostLedger` accumulates (provider_id, tier) tallies via the
#           new `MeteredProvider` decorator wrapped at the trait
#           boundary in the desktop composition root. New Tauri command
#           `llm_cost_ledger` reads `cost_ledger.snapshot()`. Dashboard
#           `CostByTierPanel` polls every 15s and renders cache-hit
#           chip per bucket (positive ≥50%, info >0%, muted 0%, "—"
#           when provider doesn't report). The panel mounts on the
#           blank-canvas home view and underneath RecordsDashboard in
#           PlanReview.
#
#   2. crates/llm/src/providers/anthropic.rs
#      docs/adr/0020-anthropic-cache-control.md (new)
#         — Anthropic request-side `cache_control: ephemeral`
#           breakpoint on `tools[0]` (always when a schema is set) and
#           on the user-content prefix when the v1.22 marker
#           `## Concrete inputs` is present. Marker-absent path stays
#           byte-for-byte identical (plain-string user content). ADR
#           0020 documents the rule and verification path.
#
#   3. apps/eval_harness/src/bin/host_probe_to_overrides.rs (new)
#      apps/eval_harness/Cargo.toml (+[[bin]] entry)
#         — Session 75 groundwork for ADR 0009 amendment 2 activation:
#           the operator runs `host-probe` against the suspect URL list,
#           pipes the TSV into this new binary, gets a Rust-syntax
#           `HOST_CLASS_OVERRIDES` snippet to paste into
#           crates/pipeline/src/fetch_classes.rs. No live probe runs
#           in this session; the script is groundwork only. The mapping
#           rule (status × UA → FetchOutcomeClass) is closed-vocab and
#           tested in-binary.
#
# What this runs:
#   [1/3] cargo check --workspace --all-targets
#         — workspace-wide check; catches AppState::new signature drift
#           (the new `cost_ledger` parameter), the `MeteredProvider`
#           wrap shape in the desktop binary, the new Tauri command
#           registration, and the new `host-probe-to-overrides` bin.
#   [2/3] cargo test -p situation_room-llm -p situation_room-api \
#                   -p situation_room-eval-harness
#         — runs the new cost_ledger tests (Tally accumulation, decorator
#           threading), the new types_export tests (ModelTierDto +
#           LlmCostLedgerEntryDto), the new anthropic build_body tests
#           (cache_control on tools, prefix-split on the v1.22 marker,
#           marker-absent legacy path, marker-at-start fallback), and
#           the new host_probe_to_overrides mapping-rule tests.
#   [3/3] cd apps/desktop && npm run check
#         — operator-run on Mac (sandbox node_modules has Mac-built
#           rollup binaries that can't load on Linux). Verifies the
#           new CostByTierPanel.svelte compiles, the new
#           LlmCostLedgerEntryDto.ts and ModelTierDto.ts files
#           resolve, and the +page.svelte + PlanReview.svelte mounts
#           type-check.
#
# Pass criteria: EXIT=0 on the last line of each log.
#
# Operator-driven cache-hit verification (after this session):
#   The Anthropic cache_control plumbing is observable in production
#   only by reading the dashboard's cost-by-tier panel after a pair
#   of consecutive authoring calls against the same source family
#   under `LLM_PROVIDER=anthropic`. The (anthropic, frontier) row's
#   cache-hit chip should flip from "—" (no cache metadata reported,
#   pre-Session-75) to a positive percentage on the second call.
#
#   xAI verification path is unchanged from Session 74 (the
#   x-grok-conv-id + cached_tokens projection already ships). The
#   new dashboard tile makes the xAI ratio visible without grepping
#   INFO logs.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/3] cargo check --workspace --all-targets"
(cargo check --workspace --all-targets 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session75-cargo-check-${STAMP}.log"

echo
echo "[2/3] cargo test -p situation_room-llm -p situation_room-api -p situation_room-eval-harness"
(cargo test \
   -p situation_room-llm \
   -p situation_room-api \
   -p situation_room-eval-harness 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session75-cargo-test-${STAMP}.log"

echo
echo "[3/3] svelte-check (apps/desktop)"
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") \
  | tee "logs/session75-svelte-check-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session75-cargo-check-${STAMP}.log"
echo "  logs/session75-cargo-test-${STAMP}.log"
echo "  logs/session75-svelte-check-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Post-verify, operator-driven:"
echo "  1. Boot desktop with default \$LLM_PROVIDER=xai. Classify a topic,"
echo "     accept, run fetch. CostByTierPanel should populate within 15s"
echo "     poll. Second classification or fetch against the same source"
echo "     family should bump the (xai, *) row's cache-hit chip."
echo
echo "  2. Boot desktop with LLM_PROVIDER=anthropic (requires"
echo "     ANTHROPIC_API_KEY in .env). Same flow. The (anthropic, *)"
echo "     row's chip should flip from '—' (no metadata) to a percentage"
echo "     on the second authoring call. If it stays at '—', Anthropic"
echo "     did not return cache_read_input_tokens — most likely cause is"
echo "     the prefix being below the minimum cacheable size (~1024"
echo "     tokens). The recipe-author prompt is well above that; the"
echo "     classifier prompt may not be."
echo
echo "Activation gate for ADR 0009 amendment 2 (deferred to next session"
echo "with a cost-authorisation budget):"
echo "  bash:  cargo run -p situation_room-eval-harness --bin host-probe -- \\"
echo "             https://example1.com/... https://example2.com/... > probe.tsv"
echo "  bash:  cargo run -p situation_room-eval-harness --bin host-probe-to-overrides \\"
echo "             --input probe.tsv"
echo "  Review the proposed Rust snippet and paste accepted entries into"
echo "  HOST_CLASS_OVERRIDES in crates/pipeline/src/fetch_classes.rs."
