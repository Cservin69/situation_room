# Classification failures

This directory documents observed failures of the **Level-1 research
classifier** — the LLM step that turns a free-text topic into a
[`ResearchPlan`](../../crates/pipeline/src/research.rs).

It is parallel to, but distinct from, [`failure_cases/class_b/`](../class_b/),
which documents Level-2 *recipe-runtime* failures per ADR 0012.

## Why a separate directory

ADR 0012 names four runtime failure classes (B/C/D/E), all of which
describe what happens after a recipe is authored. The UDB case
documented in this directory (Session 14 → 15) is a different shape:
the recipe ran exactly as authored, but it was authored against a
plan whose framing was already wrong. The bug entered the system
*before* recipe authoring, propagated through the prompt chain
(classifier interpretation → plan JSON → recipe author), and only
became visible at runtime.

A failure that's shaped like "the classifier confidently restated
the user's topic in a frame the user did not intend" doesn't fit any
of the runtime classes. It is not "the recipe matched nothing"
(Class B), not "the source is structurally hostile" (Class C), not
"the value is right type, wrong meaning" (Class D), and not "the LLM
response truncated" (Class E). It's a classification-quality issue
that *causes* downstream failures of various shapes.

Documenting these separately keeps ADR 0012's taxonomy clean (it
remains about runtime) and gives prompt-tuning work a place to land
its evidence.

## When to add a file here

Whenever the classifier produces a plan that misframes the user's
intent in a way that the user observed and reported. Specifically:

- The `interpretation` paragraph asserts a connection to a domain the
  user did not raise.
- The plan's `topic_tags` reuse an existing tag on associative rather
  than substantive grounds.
- The plan's `assertion_guidance`, expectations, or document_sources
  inherit a frame from a sibling plan in the topic registry.

A plan that is merely thin (one metric, sparse expectations) is not
a misframing — that's a prompt-richness gap and goes elsewhere.

## File format

One markdown file per case. Filename: `YYYY-MM-DD-short-slug.md`
where the date is when the failure was observed.

The file should contain, in order:

1. **Topic** — the user's literal input.
2. **Observed plan** — the relevant fields of the LLM's output.
3. **What was wrong** — the user's correction.
4. **Chain of contamination** — how the misframing propagated through
   subsequent prompts (recipe author, assertion extractor, etc.).
5. **Diagnosis** — which prompt(s), and what specifically about them,
   produced the failure.
6. **Fix** — what changed to address it (linked to the prompt diff
   or commit if applicable).
7. **Verification** — re-running the same input under the fix and
   confirming the misframing is gone.

The format is loose — this isn't a schema, it's a record. The point
is that someone reviewing prompt v1.5 in a year can see what v1.4
fixed and whether the same shape has recurred.
