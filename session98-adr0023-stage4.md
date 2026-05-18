# Session 98 — ADR 0023 → Accepted, Stage 4 refresh

Sn-93's verify-runbook ships the canonical Stage 4 for flipping
ADR 0023 from `Proposed` to `Accepted`. This Sn-98 refresh extends
the Sn-97 wrap (`session97-adr0023-stage4.md`) with the
Sn-98 binary-level caveats. The runbook itself stays at
`session93-verify.sh` Stage 4; run that script.

## What flipped between Sn-97 and Sn-98

**Sn-98 ships five things; none of them write to the `assertions`
table or change relation-claimant attribution.** The five landings,
in order:

  - **Sn-97 migration 0021 binder-error fix.** `CAST(... AS VARCHAR)`
    added in 6 spots in the SQL migration plus 1 spot in
    `Store::cleanup_orphan_entities_for_rejected_plan`. Production
    blocker — DuckDB rejected `JSON LIKE VARCHAR` with no implicit
    cast. Effect on Stage 4: zero. The migration still cleans
    Entity rows only.

  - **Sn-98 #3 — entity rewire-on-reject.** Migration 0022 + a
    rewire UPDATE in the existing cleanup helper. Mutates the
    `entities.source_id` column for re-claimed exemplars. Effect
    on Stage 4: zero. The `assertions` table is untouched; relation
    Assertions' provenance `source_id` is `{source}#recipe:…@v…`
    which the migration doesn't match (`plan:%#entity_exemplar`
    prefix only).

  - **Sn-98 #4 — Lever A wider MIME gate.** `should_extract_entities_from`
    accepts JSON/CSV/XML in addition to HTML. Increases the volume
    of `entities` table writes; does not write to `assertions`.
    Effect on Stage 4: zero.

  - **Sn-98 #5 — `upsert_entity` tier-aware refresh.** Updates
    `entities.kind` / `entities.canonical_name` / `entities.license`
    in place on conflict, when the incoming row's provenance tier
    strictly exceeds the existing row's. `id` and `entity_id` stay
    stable. Effect on Stage 4: zero. The `assertions` table is
    untouched, and the refresh policy doesn't touch
    `record_subjects_*` / `record_derived_from`.

  - **Sn-98 #1 — verify-runbook wiring.** This document plus
    `session98-verify.sh` Stage 6 pointer. Operator-facing only.

  Net: nothing in Sn-98 nudges the four ADR-coherence reads in
  Stage 4. The runbook reads the same SQL columns and the same
  table populations as it did under the Sn-97 binary.

## What to actually run

1. Open `session93-verify.sh`. Stage 4 starts at the
   `# Stage 4 — Option 1 live verify (LLM-paid; INTERACTIVE)`
   banner.

2. Pick a Minecraft-shape plan with ~5 article-shape Documents
   so cost stays under a dollar. Same posture as Sn-97/93.

3. Click through the desktop steps (a)–(e) inside Stage 4.

4. Let the script compute reads [a]+[b]+[c]+[d] from the
   before/after snapshots. The script auto-flips its own
   verdict line — no eyeball-reading required.

5. **If [a]+[b] both hold**, edit
   `docs/adr/0023-relation-claimant-diversity-at-extraction.md`
   line 3 (`**Status**:`) from `Proposed (...)` to
   `Accepted (Sn-93 Stage 4 verified live on Sn-98 binary ...)`.
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
  `session97-adr0023-stage4.md`. This is a delta-only refresh.
