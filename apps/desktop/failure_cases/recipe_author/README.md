# Recipe-author failures

This directory documents observed failures of the **Level-2 recipe
author** — the LLM step that turns a `ResearchPlan` plus a source
context into a [`FetchRecipe`](../../crates/pipeline/src/recipes.rs).

It is parallel to, but distinct from,
[`failure_cases/classification/`](../classification/) (which
documents Level-1 classifier misframings) and
[`failure_cases/class_b/`](../class_b/) (which documents Level-2
*runtime* failures per ADR 0012).

## Why a separate directory

Three failure surfaces, three categories:

- **Classification** failures happen *before* a recipe exists:
  the plan is wrong about the user's intent. Prompt-tuning lives
  in `config/prompts/research_classifier.md`.
- **Recipe-author** failures happen at recipe authoring time:
  the plan is fine, but the LLM produces a structurally weak
  recipe (wrong endpoint tier, naive selector, silent partial
  coverage of the plan's expectations). The recipe persists and
  runs forever in that shape, so the symptom is "the user gets
  fewer or weaker records than the plan suggested." Prompt-
  tuning lives in `config/prompts/recipe_author.md`.
- **Class B / runtime** failures happen at apply time: the
  recipe ran, the source returned content, and one of ADR 0012's
  named runtime classes triggered (no match, structurally
  hostile, wrong-meaning value, truncated response). These are
  the cases that motivate ADR 0012's automated re-author path
  if/when the gate conditions are met.

A failure that's shaped like "the recipe pointed at the wrong
tier of resource" or "the recipe's bindings don't honestly mirror
the plan's expectations" doesn't fit any of ADR 0012's runtime
classes — the runtime did exactly what the recipe said. It also
isn't a classification failure because the plan was correctly
framed. It's a recipe-author prompt-quality issue.

Documenting these separately keeps each prompt's evidence near
the prompt it improves, and gives ADR 0012's gate conditions a
clean view of which failures are genuinely runtime-recoverable.

## When to add a file here

Whenever the recipe author produces a recipe whose structural
choices visibly under-serve the plan that produced it.
Specifically:

- The recipe's `source_url` is an instance URL where the plan's
  matching bucket needed a listing endpoint.
- The recipe's `produces` array under-covers the plan's bucket
  in a way that isn't structurally honest (one binding for an
  N-expectation bucket where the source could have supported
  more, or N padded bindings that don't actually differentiate).
- The recipe's `extraction` selector / path / column is
  obviously naive for the source's structure (selector matches
  nothing, JSON path picks the wrong scalar from a known
  multi-field response).
- The recipe's `field_mappings` lift framing from the plan's
  `interpretation` paragraph into per-record fields (the
  Session 14 UDB case — note that case is filed under
  `classification/` because the *cause* was a classifier
  misframe, but the recipe-author prompt's permissive
  `literal` headline rule was the contributing weakness).

A recipe that's merely thin (one binding when one is honest, no
ergonomic concerns) is not a failure — that's the runtime working
as designed.

## File format

One markdown file per case. Filename: `YYYY-MM-DD-short-slug.md`
where the date is when the failure was observed.

The file should contain, in order:

1. **Topic** — the user's literal input, plus the relevant
   plan fields that fed the recipe author.
2. **Observed recipe** — the relevant fields of the LLM's
   output (URL, extraction, produces).
3. **What was wrong** — what the recipe should have looked
   like instead, and why.
4. **Chain of contamination** — how the prompt's wording
   permitted the bad shape, and how the recipe's bad shape
   then surfaced (or didn't surface) at apply time.
5. **Diagnosis** — which prompt section, and what
   specifically about it, produced the failure.
6. **Fix** — what changed to address it (linked to the prompt
   diff or commit).
7. **Verification** — re-running the same plan under the fix
   and confirming the recipe shape is now correct. May start
   "pending" and be updated.

The format is loose — same as the classification README — and
the point is the same: someone reviewing prompt v1.6 in a year
should be able to see what v1.5 fixed and whether the same
shape has recurred.
