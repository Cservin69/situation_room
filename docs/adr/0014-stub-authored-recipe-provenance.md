# ADR 0014 — Stub-authored recipe provenance: a visible signal

**Status**: Accepted
**Date**: 2026-05-02
**Related**: ADR 0007 (research function: two-level LLM architecture
— specifically the "every number traceable to a source" promise),
ADR 0013 (recipe-feedback channel), ADR 0011 (plan lifecycle and
fetch executor), ADR 0009 (security posture)

---

## Context

Session 20's first end-to-end live run surfaced a real provenance
gap. The plan `019de792-…` ("south Korea latest election") had two
bound sources, `rss_feeds` and `gdelt`. The `gdelt` endpoint hint
was rate-limited (HTTP 429) at the moment recipe-authoring ran, so
[`fetch_executor::author_one`](../../crates/pipeline/src/fetch_executor.rs)
took the documented fallback path — it logged

```
endpoint_hint pre-fetch failed; authoring will fall back to stub excerpt
```

then constructed a [`stub_excerpt`](../../crates/pipeline/src/fetch_executor.rs)
from the plan's prose plus the URL, and handed that to the LLM in
place of the source's actual response bytes. The LLM produced a
recipe (`$.articles[0].title`) — a plausible *guess* at GDELT's
JSON shape, not a shape it had observed. The recipe was persisted
to the `recipes` table and is now the single source of truth for
how the runtime extracts records from GDELT for this plan.

The defect: nothing in the recipe row, the `FetchReport`, or the
inspection panel distinguishes "authored from real bytes" from
"authored from stub." A user looking at a *successful* future
fetch from this recipe — one that returned bytes that happened to
have an `articles` array, or one that returned an empty array
(empty result, not extraction failure) — has no way to tell
whether to trust the data shape.

Stockpile's central architectural claim — ADR 0007's "every number
traceable to a source" — depends on the user being able to assess
the *quality* of that trace, not only its existence. A trace whose
authoring step relied on a fallback description rather than the
source's response is a weaker trace, and the system must say so.

The Session 21 handoff named three options. This ADR records the
choice and the rationale for the deferred option.

## Decision

A new typed signal, `AuthoredFrom`, is stamped on every recipe at
authoring time and rendered as a visible chip in the UI. The signal
is structurally narrow (one column, one Rust enum, one DTO field,
one chip, one dialog hint) and orthogonal to every other recipe
concern — it does not change extraction, validation, freshness, or
re-authoring policy.

### The enum

```rust
pub enum AuthoredFrom {
    /// The recipe author's prompt included the source's actual
    /// fetched response bytes (from `prefetch_excerpt`'s
    /// `Some(real_bytes)` path). The LLM had ground truth.
    FetchedBytes,

    /// The recipe author's prompt included a stub excerpt
    /// (from `stub_excerpt(...)`) describing the source from the
    /// plan + URL only. The LLM was guessing the response shape.
    /// Reachable when:
    ///   - the source has no `endpoint_hint` configured,
    ///   - the configured `endpoint_hint` is unparseable,
    ///   - the pre-fetch returned an HTTP/transport error (429,
    ///     5xx, DNS, TLS, body-too-large).
    StubExcerpt,

    /// The column was NULL on disk — typical of recipes authored
    /// before this ADR landed (migration v10 added the column as
    /// nullable). Treated as "we don't know" in the UI rather than
    /// silently coerced to `FetchedBytes`, which would be a
    /// retroactive truth claim about historical authoring runs the
    /// new code never witnessed.
    Unknown,
}
```

Wire form is lowercase snake_case (`fetched_bytes`, `stub_excerpt`,
`unknown`), matching the project's existing `serde(rename_all =
"snake_case")` convention on `PlanStatus`, `ExtractionSpec`, and
`RecipeOutcome`.

### Where the signal is set

The fetch executor is the only place that knows. In
[`author_one`](../../crates/pipeline/src/fetch_executor.rs), the
existing code already takes one of two branches when building the
document excerpt:

```rust
let excerpt = match &hint_for_prefetch {
    Some(url) => match prefetch_excerpt(ctx, url, source_id).await {
        Some(real) => real,                                // FetchedBytes
        None       => stub_excerpt(plan, source_id, ...),  // StubExcerpt
    },
    None => stub_excerpt(plan, source_id, None),           // StubExcerpt
};
```

This ADR adds a parallel `authored_from` value derived from the
same branch and stamped on the returned `FetchRecipe` next to the
existing `source_id` and `dedup_key` stamps:

```rust
recipe.authored_from = if used_real_bytes {
    AuthoredFrom::FetchedBytes
} else {
    AuthoredFrom::StubExcerpt
};
```

`build_validated_recipe` itself does not know — it has no view of
which excerpt path produced its input. The convention is the same
as for `source_id` and `dedup_key`: the validator sets a default
(`AuthoredFrom::Unknown`); the executor stamps the real value.

### What the user sees

Two surfaces, both passive:

1. **A `STUB-AUTHORED` chip** on the recipe card in the inspection
   panel, visible iff `recipe.authored_from == StubExcerpt`. Sits
   next to the existing `BAKED` (ADR 0007 Amendment 3) and
   `FLAGGED` (ADR 0013) chips, with the same chip discipline:
   monospace, uppercase, 9px, `--signal-warning` hue (warranted
   attention, not destructive). Tooltip:

   > Authored from a fallback description, not the source's actual
   > response. The recipe is a guess at the response shape. If the
   > source becomes reachable, run fetch again — a future session
   > may surface a "re-author from real bytes" path.

2. **A hint banner** in the [`RecipeFlagDialog`](../../apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte),
   rendered above the textarea iff the recipe being flagged carries
   `StubExcerpt`. Distinct from the freeform note — it's a
   deliberate piece of context the operator should see *before*
   typing:

   > This recipe was authored without the source's actual response.
   > Flagging it is still useful, but consider running fetch first —
   > if the source is now reachable, the next authoring run will
   > see the real bytes.

   The hint is informational; it does not gate submission.

The chip on the recipe card is the canonical surface (always
visible without a dialog). The dialog hint exists because the
flag-vs-rerun decision is a real fork: a flag with a custom note
will steer the *next* author against a stub-vs-real-bytes situation
the operator has the chance to resolve first by simply re-running
fetch.

### What the user does NOT see (option 3, deferred)

The Session 21 handoff named a third option:

> The cleanest invariant: stub-authored recipes are tagged at
> authoring time and **silently re-authored from real bytes on the
> first successful fetch**, regardless of whether the stub-authored
> recipe also succeeded. The system self-heals to real-bytes
> provenance.

This option is **deferred**, by deliberate choice. Reasoning:

1. **It changes runtime behavior.** ADR 0007's "runtime is
   LLM-free" invariant has one explicit exception: re-authoring on
   recipe failure (per the same ADR's "Recipe failure and
   re-authoring" section). Self-healing on stub-authored recipes
   would add a *second* trigger — re-authoring on an *actively
   succeeding* recipe — and changes the invariant from
   "deterministic except after observable failure" to
   "deterministic except after observable failure or after silent
   policy decisions." That's a different shape, deserving an ADR
   amendment to 0007 in its own right.

2. **It costs LLM tokens silently.** A stub-authored recipe that's
   working from the user's perspective (records arriving on every
   fetch) would, on its first successful real-bytes fetch, trigger
   a Level-2 authoring pass (30–60 seconds of xAI time, real
   spend). The user has no expectation of this cost being incurred
   when they click "run fetch" on a recipe that's been working for
   weeks. Auto-spending API budget on a behind-the-scenes upgrade
   is a posture decision, not a default.

3. **The signal alone is sufficient for the immediate need.**
   Option 1 + 2 closes the *visibility* gap completely. The user
   can see at a glance which recipes are stub-authored, can hover
   for the explanation, and can manually trigger re-authoring by
   editing the feedback note (or by waiting for the existing
   re-author-on-failure path if the recipe later breaks). Self-
   healing is an *optimization* on top of visibility, not a
   replacement for it.

4. **Empirical evidence is missing.** The motivating case (GDELT
   429) is a single observed instance. Whether stub-authored
   recipes commonly produce wrong-shape extractions, commonly
   produce right-shape extractions by lucky guess, or commonly
   produce empty-result extractions that look healthy is currently
   unknown. Designing self-healing without knowing the failure
   distribution risks optimizing for the wrong path.

This ADR commits to amending 0007 (and this ADR) **before** any
code lands that performs silent re-authoring on a non-failing
recipe. The amendment must answer:

- **When does self-healing trigger?** First successful real-bytes
  fetch? Every Nth successful fetch? Only if the prior records
  match a heuristic (empty-result rate above threshold)?
- **How does the user opt out?** A per-plan flag? A global
  setting? The flag affordance from ADR 0013 (operator-attached
  feedback) gates re-authoring already in the manual path; should
  the feedback channel also gate auto-healing?
- **Does the auto-re-authored recipe persist alongside the
  stub-authored one (versioning)?** Or replace it (overwrite)?
  ADR 0007's "Versioning vs migration" already specifies the
  semantic-changes-bump-version rule; auto-re-authoring is a
  semantic change.
- **What does the UI show?** A "self-healing in progress" pill?
  A history strip on the recipe card?

When at least one of these has a clear answer grounded in
operational data, the amendment lands. Until then, the visible
chip is the user's hook for triggering the manual path themselves.

### Storage shape

A single nullable `TEXT` column on `recipes`, added by migration
v10:

```sql
ALTER TABLE recipes ADD COLUMN authored_from TEXT;
```

NULL is the on-disk shape for any recipe authored before this ADR
landed (Sessions 1–20). The Rust load path coerces NULL →
`AuthoredFrom::Unknown`. New recipes always carry a populated
value (`FetchedBytes` or `StubExcerpt`), set by the executor.

The same DuckDB ALTER trap from migrations 0005 and 0007 applies
(`ADD COLUMN ... NOT NULL DEFAULT ...` is rejected; the
split-then-set-NOT-NULL path is rejected when indexes exist on the
table — which `recipes` has via 0003's `(plan_id, source_id)`
index). The nullable column sidesteps the trap. The Rust enum is
the load-bearing invariant — same posture as `PlanStatus` is in
0005 and `static_payload` is in 0008.

### Why a column on `recipes`, not a sibling table

The signal is one-to-one with a recipe row, derived at the moment
the recipe is authored, never updated thereafter. The natural
shape is a column. A sibling table (`recipe_authoring_provenance`,
keyed by `recipe_id`) would force a JOIN on every recipe load
without buying anything — there's no second consumer of the
information, no history to track (the field is set once at
authoring time and never mutated), no FK to anywhere else. The
sibling-table shape would only make sense if multiple provenance
facts about the recipe accumulated over time; today there is one,
and even the deferred option-3 self-healing would either *bump
version + insert a new row with FetchedBytes* (preserving the
audit chain via the existing version mechanic) or *overwrite the
existing row's authored_from* (handled by the column directly).

### Why not a `RecipeOutcome` field

The outcome enum mirrors *what happened during this fetch run*.
`authored_from` describes *what happened during the prior
authoring run* — a different lifecycle. Threading it through the
outcome would either duplicate the value (the recipe row already
carries it) or imply that the outcome can mean a different thing
across runs based on authoring history that the runtime did not
re-do (the recipe's `authored_from` is fixed; only the recipe row
ever changes it, and only via a re-author).

The chip on the recipe card is the right surface. The
`FetchReport` panel renders the outcomes; the recipes panel
renders the recipes; they're separated by intent and each carries
the data appropriate to its concern.

## Alternatives considered

### Add a free-text "authoring notes" field

A `TEXT` column the LLM populates with a paragraph about what it
saw at authoring time. *Rejected.* The information we want is
boolean-shaped (real bytes / stub) and known by the executor
without the LLM's help. Asking the LLM to self-report would
introduce an LLM-controlled signal where a deterministic one
suffices, and would consume prompt + completion tokens for a fact
the executor already has.

### Write the chip into `RecipeOutcome` rather than `RecipeDto`

So the FetchReport panel surfaces it on every successful run.
*Rejected.* See "Why not a RecipeOutcome field" above. The chip
on the recipe card is one place; duplicating it in two surfaces
encodes the same fact twice and risks the two surfaces drifting if
one is updated and the other isn't. The recipes panel is the
canonical surface; the FetchReport stays focused on per-run
outcomes.

### Trigger silent self-healing on the first real-bytes fetch

Option 3 from the handoff. *Deferred*, with the explicit amendment
trigger conditions in §"What the user does NOT see (option 3,
deferred)" above.

### Block the operator from accepting plans with stub-authored recipes

A pre-acceptance check: any recipe whose authoring fell back to
the stub gates the plan from being accepted until the operator
acknowledges. *Rejected.* Stub-authored recipes are useful — the
operator's `rss_feeds` recipe in the Session 20 live run was
stub-authored against a well-known feed format and worked
correctly on the first fetch. Treating the stub path as failure
would block working configurations for a posture that a chip
suffices to communicate.

## Consequences

### Good

- The provenance gap closes at the level it was opened: the user
  sees, on the recipe card, whether the LLM had ground truth at
  authoring time. The chip is the smallest possible surface that
  communicates the fact.
- Self-healing is now *implementable* in a future session — the
  column is the prerequisite, and the stub-authored recipes are
  the candidate set. The amendment trigger conditions in this ADR
  define what would unlock it.
- The recipe-feedback dialog now has a stronger context for the
  operator: when flagging a stub-authored recipe, the hint guides
  toward "rerun fetch first" before typing a note that may be
  overtaken by reality.

### Tradeoffs

- One more chip on the recipe card (alongside `BAKED` and
  `FLAGGED`). The chip cluster is bounded — three is still
  glanceable; a fourth would warrant rethinking the head layout.
  No fourth chip is on the roadmap.
- The `Unknown` variant exists for legacy data and will be
  *visible* in the wire shape forever. The UI does not render a
  chip for it (absence is the signal); the wire form still carries
  it for honesty about what's known. A future session could
  back-fill (`UPDATE recipes SET authored_from = '...' WHERE
  authored_from IS NULL`) but only if a heuristic is defensible —
  which today it isn't. Leaving it as `Unknown` is accurate.
- Migration v10 is additive but does mean the next operator-side
  `cargo run` on an existing local database will run one ALTER
  statement. Same operational shape as v8 (also an ADD COLUMN).

### Neutral

- No new HTTP path. No new LLM call. No change to the closed
  extraction-mode enum. ADR 0007's "runtime is LLM-free"
  invariant is unchanged (the executor's branching on
  `prefetch_excerpt`'s success was already there; this ADR adds a
  derived value, not a new fork). ADR 0009's security posture is
  unchanged (no new trust boundary; the field is purely internal
  metadata).

## When to amend or supersede

This ADR will need an amendment if any of the following becomes
true:

- The operational data justifies option 3 (silent self-healing).
  The amendment must answer the four questions in §"What the user
  does NOT see (option 3, deferred)".
- A second source of recipe-authoring provenance appears (e.g. a
  per-source quality score from prior fetches feeds back into
  authoring). At that point, the column-on-recipes shape may want
  to grow into a small sibling table. Until then, one column is
  enough.
- A failure-mode taxonomy emerges that distinguishes
  stub-authored-and-empirically-correct from
  stub-authored-and-empirically-wrong. The chip would then want
  a stronger signal (color, count, history) than the current
  binary "is it a stub" indicator.

Until then: one column, one enum, one chip, one dialog hint. The
minimum surface that surfaces the fact without committing to a
remediation policy.

## Code references

- Storage column:
  `migrations/0010_recipes_authored_from.sql` — additive ALTER,
  nullable.
- Storage row:
  `crates/storage/src/recipes.rs::AuthoredFrom`,
  `crates/storage/src/recipes.rs::RecipeRow::authored_from`,
  `crates/storage/src/recipes.rs::StoredRecipe::authored_from`.
- Pipeline type:
  `crates/pipeline/src/recipes.rs::FetchRecipe::authored_from`.
- Executor stamp:
  `crates/pipeline/src/fetch_executor.rs::author_one` — sets
  `recipe.authored_from` after `author_recipe` returns, derived
  from the `prefetch_excerpt` branch already in place.
- Marshalling:
  `crates/pipeline/src/recipes_store.rs::recipe_to_row` and
  `stored_to_recipe` — thread the field both directions.
- Wire DTO:
  `crates/api/src/types_export.rs::AuthoredFromDto`,
  `crates/api/src/types_export.rs::RecipeDto::authored_from`.
- UI chip:
  `apps/desktop/src/components/RecipesPanel.svelte` — STUB-AUTHORED
  badge in the recipe head, sized + colored consistently with
  BAKED and FLAGGED.
- UI hint:
  `apps/desktop/src/components/dialogs/RecipeFlagDialog.svelte` —
  optional banner above the textarea when the flagged recipe is
  stub-authored.

## Review notes

ADR drafted Session 21 (2026-05-02), in response to the live run
finding from Session 20. Per the discipline established in Session
19 (ADR before code) and reaffirmed in Session 20 (option-3 is the
operator's call), this ADR records the choice and the deferred
option's amendment triggers in the same document, so a future
session weighing self-healing finds the conditions already named.

The Session 21 handoff explicitly framed this as "architectural,
not a drop-in patch" and "design discussion before any code." The
design above is the product of that discussion. The implementation
in Session 21 follows this ADR exactly. If empirical use surfaces
any of the amendment triggers in §"When to amend or supersede,"
the next session writes the amendment before the code change.
