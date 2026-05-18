# Session 99 — ADR 0023 → Accepted, Stage 4 refresh

Sn-93's verify-runbook ships the canonical Stage 4 for flipping
ADR 0023 from `Proposed` to `Accepted`. This Sn-99 refresh extends
the Sn-98 wrap (`session98-adr0023-stage4.md`) with the
Sn-99 binary-level caveats. The runbook itself stays at
`session93-verify.sh` Stage 4; run that script.

## What flipped between Sn-98 and Sn-99

**Sn-99 ships four things; none of them write to the `assertions`
table or change relation-claimant attribution.** The four landings,
in order:

  - **Sn-99 #5 — explicit-tier upsert API.** New
    `Store::upsert_entity_with_tier(&Entity, EntityProvenanceTier)`
    is now the authoritative entity-upsert path. `upsert_entity`
    (the Sn-98 method) becomes a back-compat shim that derives the
    tier from license and delegates. Migrated call sites: Lever A
    (`extract.rs`) and Lever B (`record_dispatch.rs`); entity_synth
    intentionally stays on `insert_entity` (see method docstring).
    Effect on Stage 4: zero. Writes still go to `entities`, never
    to `assertions`. The refresh decision itself is the same shape
    Sn-98 #5 introduced — what changed is the signal source (typed
    arg vs. license-derived).

  - **Sn-99 #4 — refresh-event ring buffer.**
    `Store::entity_refresh_log_snapshot()` exposes an in-memory
    `VecDeque<EntityRefreshEvent>` capped at
    `ENTITY_REFRESH_LOG_CAP = 50`. The Sn-98 #5 refresh branch
    pushes one event per in-place mutation. New Tauri command
    `entity_refresh_log` + `EntityRefreshPanel.svelte`. Effect on
    Stage 4: zero. No new DB rows; the ring is process-local.

  - **Sn-99 divergence guard.**
    `upsert_entity_with_tier` emits a WARN log when the explicit
    tier disagrees with the license-derived tier. Defence-in-depth
    against future call sites that opt into an explicit tier but
    stamp a misaligned license string. No DB writes; logging only.
    Effect on Stage 4: zero.

  - **Sn-99 #1 — verify-runbook wiring.** This document plus
    `session99-verify.sh` Stage 5 pointer. Operator-facing only.

  Net: nothing in Sn-99 nudges the four ADR-coherence reads in
  Stage 4. The runbook reads the same SQL columns and the same
  table populations as it did under the Sn-98 binary, plus the
  additional `entity_refresh_log` reads — which sit outside the
  ADR 0023 surface.

## What to actually run

1. Open `session93-verify.sh`. Stage 4 starts at the
   `# Stage 4 — Option 1 live verify (LLM-paid; INTERACTIVE)`
   banner.

2. Pick a Minecraft-shape plan with ~5 article-shape Documents
   so cost stays under a dollar. Same posture as Sn-98/97/93.

3. Click through the desktop steps (a)–(e) inside Stage 4.

4. Let the script compute reads [a]+[b]+[c]+[d] from the
   before/after snapshots. The script auto-flips its own
   verdict line — no eyeball-reading required.

5. **If [a]+[b] both hold**, edit
   `docs/adr/0023-relation-claimant-diversity-at-extraction.md`
   line 3 (`**Status**:`) from `Proposed (...)` to
   `Accepted (Sn-93 Stage 4 verified live on Sn-99 binary ...)`.
   Single-line edit closes the ADR.

6. **If [a]+[b] don't both hold**, the prompt change isn't
   verified. Re-read Sn-91's framing under "Path A1" before
   choosing the next prompt-A/B experiment (gated by
   `feedback_eval_cost_discipline`).

## Cost discipline

LLM-paid at Stage 4 step (c) (re-extract relations for the chosen
plan). One workhorse-tier call per article-kind Document. Pick the
smallest plan that exercises multi-claimant attribution. No evals
required.

## What this runbook is NOT

- Not a script. The work is exclusively operator-driven (desktop
  clicks + ADR edit). Sn-93's shell script is the harness.
- Not an automatic status flip. The ADR edit is one line; the
  diligence is in confirming [a]+[b] yourself before pressing
  save.
- Not a replacement for Sn-93's Stage 4 or for
  `session98-adr0023-stage4.md`. This is a delta-only refresh.
