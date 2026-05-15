#!/usr/bin/env bash
# Session 76 — verify the bundled push of Session-75-handoff
# candidate "neither, populate more dashboard types" reframed by
# the operator as Choice C: Entity exemplar materialisation +
# classifier event-bias prompt edit.
#
#   1. crates/pipeline/src/entity_synth.rs (new)
#      crates/pipeline/src/lib.rs (+pub mod entity_synth)
#      crates/api/src/commands.rs (accept_plan hook +
#         build_typed_plan_from_stored helper)
#         — Promote each `entity_kinds[*].exemplars[*]` to a
#           persisted Entity row at plan-accept. Closed-vocab
#           (classifier output), no LLM calls. Idempotent: the
#           `entities.entity_id` UNIQUE constraint plus an upfront
#           `get_entity_by_business_id` check absorb re-accepts of
#           the same plan and same-exemplar overlap across plans.
#           Failures are non-fatal and surface in the returned
#           `MaterializationReport.errors`.
#
#   2. crates/storage/src/queries.rs (records_for_plan extension)
#         — Add a `plan:{plan_id}#%` LIKE pattern alongside the
#           existing `%#recipe:<uuid>@v%` patterns so the per-plan
#           dashboard view picks up Session-76 plan-keyed Entity
#           rows even before any recipes run. Two new tests pin
#           inclusion and cross-plan non-leakage.
#
#   3. config/prompts/research_classifier.md v2.1
#         — New section "Don't let `observation_metrics` crowd out
#           `event_types`" + four shape-specific event-vocab
#           starter lists (stock / rate / commodity / population).
#           Trigger language pinned: words like "announcements",
#           "releases", "decisions", "events" appearing in the
#           interpretation → event_types must not be empty.
#
#   4. config/prompts/recipe_author.md
#         — Loose-wording update: replace the stale "entities come
#           from registry lookup" reference with "entities are
#           materialised from the classifier's
#           entity_kinds[*].exemplars[*] at plan-accept time". The
#           recipe author still cannot bind entity_kind via
#           field_mappings (the storage layer rejects it); the
#           framing now points at where the Entity rows actually
#           originate.
#
# What this runs:
#   [1/3] cargo check --workspace --all-targets
#         — workspace-wide check; catches accept_plan's new
#           dependency on `entity_synth`, the storage queries
#           pattern change, and the pipeline module add.
#   [2/3] cargo test -p situation_room-pipeline -p situation_room-storage \
#                    -p situation_room-api
#         — runs the new entity_synth tests (canonical_name
#           humanisation, build_exemplar_entity provenance,
#           idempotent re-materialisation, empty-kind skip), the
#           queries.rs additions (records_for_plan picks up
#           plan-keyed entity, no leak across plans), and the
#           api command surface integration.
#   [3/3] cd apps/desktop && npm run check
#         — operator-run on Mac. No frontend changes in this
#           session — the existing RecordsDashboard already
#           renders entities by `kind` + `canonical_name`. Run
#           anyway to confirm nothing regressed.
#
# Pass criteria: EXIT=0 on the last line of each log.
#
# Operator-driven verification (after this session):
#   1. Boot desktop. Open any existing accepted plan that has
#      `entity_kinds` populated with exemplars (lithium plans
#      from the fixture do; the FEMA plan from today's logs
#      had entity_kind expectations but the materialisation
#      didn't exist yet — so the FEMA plan will only get
#      entities on a fresh accept, not retroactively).
#
#   2. Re-accept the plan via the UI (status flip pending→accepted
#      is not required if the plan is already accepted; the
#      materialiser runs on every accept_plan call so any UI path
#      that hits the command will trigger it). Alternative: use
#      the typed API directly if exposed.
#
#   3. Watch the dashboard's Entities panel populate with one
#      MetricCard per `entity_kinds[].kind`, sample line showing
#      the humanised exemplar name (e.g. "tsla", "ibm quantum",
#      "fema").
#
#   4. Classify a NEW broad-story topic where v2.1's bias would
#      matter — e.g. "ferrari stock price". Confirm the resulting
#      plan has `event_types` populated (not just
#      `observation_metrics`). Earnings_release,
#      analyst_rating_change, etc. should appear. This is the
#      classifier-v2.1 verification.
#
# What this session did NOT do (deferred):
#   - Per-document Assertion synthesis (Phase 3 of pipeline::extract).
#     Picking up the Session 76 Option B path lights up the
#     Assertions panel for every plan that has Documents — bounded
#     LLM cost per fetched URL. Sized as a half-to-full session.
#   - Re-classification of existing plans whose `entity_kinds`
#     have empty `exemplars`. Materialisation can only promote
#     what the classifier already named; plans classified before
#     the operator pushed for broader output may need a fresh
#     `reclassify_plan` call to gain exemplars.
#   - Frontend changes — the dashboard already renders entities
#     from `RecordsByPlan.entities`. Nothing to do until the
#     operator reports a missing tile.

set -u

cd "$(dirname "$0")"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mkdir -p logs

echo "[1/3] cargo check --workspace --all-targets"
(cargo check --workspace --all-targets 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session76-cargo-check-${STAMP}.log"

echo
echo "[2/3] cargo test -p situation_room-pipeline -p situation_room-storage -p situation_room-api"
(cargo test \
   -p situation_room-pipeline \
   -p situation_room-storage \
   -p situation_room-api 2>&1; \
  echo "EXIT=$?") \
  | tee "logs/session76-cargo-test-${STAMP}.log"

echo
echo "[3/3] svelte-check (apps/desktop)"
(cd apps/desktop && npm run check 2>&1; echo "EXIT=$?") \
  | tee "logs/session76-svelte-check-${STAMP}.log"

echo
echo "Done. Logs:"
echo "  logs/session76-cargo-check-${STAMP}.log"
echo "  logs/session76-cargo-test-${STAMP}.log"
echo "  logs/session76-svelte-check-${STAMP}.log"
echo
echo "Look for EXIT=0 on the last line of each."
echo
echo "Post-verify, operator-driven:"
echo "  1. Boot desktop. Re-accept any plan with entity_kind exemplars"
echo "     (the lithium fixture plans qualify). Watch the per-plan"
echo "     Entities panel populate."
echo "  2. Classify a NEW broad-story topic (e.g. \"ferrari stock price\")."
echo "     Confirm event_types is NOT empty in the resulting plan."
echo "     This is the classifier v2.1 verification — the prompt change"
echo "     biases against the obs-only failure mode shown in today's"
echo "     Tesla screenshot."
