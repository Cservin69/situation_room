# Session 97 — ADR 0023 → Accepted, Stage 4 refresh

Sn-93's verify-runbook ships the canonical Stage 4 for flipping
ADR 0023 from `Proposed` to `Accepted`. This Sn-97 refresh records
the binary-level caveats that landed between Sn-93 and today so
the operator running Stage 4 against the current build knows what
to read and what to ignore.

The runbook itself stays at `session93-verify.sh` Stage 4. Run
that script; the Sn-97 caveats below describe what changed
underneath.

## What flipped between Sn-93 and Sn-97

**Sn-94** — migration 0020 (ADR 0024 + Sn-78 poison cleanup). Touches
`record_derived_from.parent_type` backfill + Sn-78 Assertion delete.
Independent of ADR 0023's verdict; the cleanup runs once at boot
and never re-runs. No effect on Stage 4 read.

**Sn-95** — diagnosis only, no code. Identified the iterator-vs-
detector conflation. Independent of ADR 0023.

**Sn-96** — `check_index_page` gains `iterator: Option<&ExtractionSpec>`;
returns `None` early for iterator-bearing recipes. The cull path
(`cull_index_assertions_for_plan` + `sample_index_assertions_for_plan`)
runs the SAME detector against each Document's raw bytes (not
against any recipe's iterator), so the cull continues to remove the
Sn-91 aluminium singletons exactly as Sn-93's Stage 4 assumed.

  - **Stage 4 read [c]** (B2 aluminium singletons in POST < same in
    PRE) is unchanged. Sn-96 didn't touch the cull detector.

  - **Stage 4 read [a]** (C1.n_recent > 0 in POST) is unchanged.
    The re-extract path (`reextract_relations_for_plan`) walks
    Documents, not recipes; iterator-recipe Documents go through
    the same per-Document extraction loop they always have.

**Sn-97 (this session)** — five bundled landings, none of which
write to the `assertions` table the way the Sn-91 fragmentation
measured:

  - **Bug 4 + Bug 5** — migration 0021 cleans up orphan
    entity_synth Entity rows from rejected plans and dangling
    `record_derived_from` rows whose parent resolves to no
    per-table row. Both are storage-cleanup passes; neither
    deletes Assertion rows or changes per-claimant counts.
    Stage 4's measurement SQL reads from `assertions` and
    `record_subjects_entities` filtered by `record_type='assertion'`;
    migration 0021 doesn't touch either.

  - **Lever A** — per-Document Entity extractor. Writes to the
    `entities` table via `Store::upsert_entity`. Does NOT emit
    Assertion rows; ADR 0023 measures relation Assertions
    specifically. Lever A is invisible to Stage 4's verdict.

  - **Lever B** — `record_type=entity` opened to recipes. Same
    target table (`entities` via `upsert_entity`). Same
    invisibility to Stage 4.

  - **Recipe-author v1.25** — opens `entity` as a valid binding.
    Doesn't change `document_assertions.md`. The v1.2 prompt ADR
    0023 hinges on stays as is.

  Net: nothing in Sn-97 nudges the four ADR-coherence reads in
  Stage 4. The runbook reads the same SQL columns and the same
  table populations.

## What to actually run

1. Open `session93-verify.sh`. Stage 4 starts at the
   `# Stage 4 — Option 1 live verify (LLM-paid; INTERACTIVE)`
   banner.

2. Skim the cost preview (Stage 4 Step A) and pick a plan with
   article-shape Documents under `~10` so the workhorse-tier
   cost stays under a dollar. Same posture as Sn-93's note about
   Minecraft-shape plans.

3. Click through the desktop steps (a)–(e) listed inside Stage 4.

4. Let the script compute reads [a]+[b]+[c]+[d] from the
   before/after snapshots. The script auto-flips its own verdict
   line — no eyeball-reading the SQL required.

5. **If [a]+[b] both hold**, edit
   `docs/adr/0023-relation-claimant-diversity-at-extraction.md`
   line 3 (the `**Status**:` line) from
   `Proposed (Session 91 — code-and-prompt landed; … live
   verification gated on Sn-93 verify-runbook Stage 4 output)`
   to
   `Accepted (Sn-93 Stage 4 verified live on Sn-97 binary …)`.
   That single-line edit is the entire status flip.

6. **If [a]+[b] don't both hold**, the prompt change isn't
   verified. Re-read Sn-91's framing under "Path A1" and decide
   whether the empirical question (does v1.2 produce multi-claimant
   rows on a clean Minecraft-shape Document?) needs a different
   plan, a different fixture, or a different multi-claimant prompt
   shape than v1.2.

## Cost discipline

This runbook is LLM-paid at Stage 4 step (c) (re-extract
relations for the chosen plan). One workhorse-tier call per
article-kind Document. Match Sn-67/68 discipline: pick the
smallest plan that exercises multi-claimant attribution
(a Minecraft-shape plan with ~5 articles is the canonical Sn-93
choice).

No evals are required. ADR 0023's "live verify" is a single
re-extract pass over an existing plan's Documents — orders of
magnitude cheaper than the multi-trial prompt A/Bs the
`feedback_eval_cost_discipline` memory governs.

## What this runbook is NOT

- Not a script. The work is exclusively operator-driven (desktop
  clicks + ADR edit). Sn-93's shell script is the harness.
- Not an automatic status flip. The ADR edit is one line; the
  diligence is in confirming [a]+[b] yourself before pressing
  save.
- Not a replacement for Sn-93's Stage 4. Read it in
  `session93-verify.sh` lines 251–440.
