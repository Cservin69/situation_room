# ADR 0012 — Re-author on failure: manual-first, automated-later

**Status**: Accepted — Deferred (manual-first interim practice in effect)
**Date**: 2026-04-30
**Related**: ADR 0007 (research function: two-level LLM architecture),
ADR 0011 (plan lifecycle and fetch executor), ADR 0003 (six record
types), ADR 0009 (security posture)

---

## READ THIS FIRST — DO NOT DEVIATE

This ADR encodes a hard-won design decision made after reviewing both
an automated draft path (ADR 0012 v1) and a competing minority report
from the frontier LLM used in production (Grok Frontier, Session 14).
The synthesis took non-trivial reasoning. Future sessions — including
sessions where a test is failing, a deadline feels close, or the
automated path looks "almost ready" — **must not implement the
automated re-author retry loop** until every prerequisite in §"When
to automate" is met.

**The single gate:** 10 or more empirically observed, distinctly-shaped
Class B failures across diverse sources and plan types, documented in
`docs/failure_cases/class_b/`. We do not have that yet. Until we do,
the predicate strings that would drive the automated loop are
guesswork, and guesswork in a retry loop compounds into wasted LLM
spend and silent wrong records.

**If a test is failing and you are considering wiring the retry loop
to make it pass: stop.** The test is not the truth. The failure-mode
taxonomy is the truth. Fix the test failure through the manual path
first, observe and document what you find, and revisit automation
only when the gate is met.

The automated architecture is recorded in §"Part 2 — Automated path
architecture" so future sessions do not redesign it from scratch.
That section is a *specification*, not an invitation. Do not
implement it early.

---

## Context

### The failure-mode taxonomy (Session 13)

Session 13 produced the first real failure-mode taxonomy for the
executor, grounded in three live xAI runs against real sources:

- **Class B** — recipe authored cleanly, but the extraction pattern
  matched nothing in the fetched bytes. The LLM authored against a
  description of the source or an idealized mental model of its
  content, rather than the actual bytes at runtime. Session 10's
  pre-fetch (Option F) was supposed to anchor the LLM in reality;
  empirically it does not always succeed, because content changes
  between authoring and runtime and because the pre-fetch excerpt
  may not include the specific content the pattern needs to anchor
  on.
- **Class C** — structural mismatch with the extraction vocabulary:
  the source is a JS-rendered SPA; the static HTTP response never
  carries the data. No recipe in the closed enum can address this.
  Re-authoring produces a different recipe against the same empty
  bytes. It does not help.
- **Class D** — record produced but semantically misaligned: the
  extractor finds *something*, but the value does not answer the
  plan's expectation. This is not an Apply-stage failure from the
  runtime's perspective — the record *was* produced. Re-authoring
  the extraction spec does not help.
- **Class E** — LLM response truncation at the gateway layer.
  Partially addressed in Session 13 via a single-shot token-doubling
  retry inside `XaiProvider::complete`. This is a provider-layer
  concern, not an executor-layer concern.

ADR 0011 §"LLM-free runtime invariant" explicitly deferred
re-authoring: "A failed recipe surfaces in the report; the user
decides what to do." That was correct when the taxonomy didn't exist.
This ADR upgrades "user decides" to "user follows this protocol."

### Why automation is deferred: the minority report

ADR 0012 v1 (Session 14, first draft) proposed an automated retry
loop: detect Class B via string-matching predicates on error messages,
call `reauthor_recipe` with the fetched bytes, cap at 2 retries.
The design was technically coherent. A minority report from the
frontier LLM in production (Grok Frontier) identified six edge cases
it cannot handle reliably at current evidence levels:

1. **Oscillation**: after 2–3 re-author attempts on the same failure
   message and bytes, the LLM may cycle through structurally similar
   patterns rather than converging. An automated loop surfaces the
   final failure; a human in the loop can name the oscillation and
   redirect ("this source may not be addressable with the current
   extraction vocabulary — stop here").

2. **Non-Class-B misclassification**: `looks_like_class_b_failure`
   is a string-matching heuristic over error messages. It will
   misclassify. Class C failures (JS SPA matched nothing) produce
   Apply-stage failures that look identical to Class B. The
   automated loop would retry Class C until hitting the cap,
   spending real money on runs that are structurally impossible.
   The manual path catches this before the first API call.

3. **Missing empirical data before re-authoring**: the automated
   loop re-authors using the bytes from the initial fetch. If the
   operator triggers re-authoring without understanding *why* it
   failed, they cannot evaluate whether the new recipe is actually
   better or just differently wrong. The manual path forces the
   operator to read the failure before asking for a correction.

4. **Recipe degradation**: a reauthored recipe may extract less
   useful data or introduce a regression in a binding that was
   previously working. An automated loop that persists version 2
   over version 1 without human review creates a class of silent
   regression that is hard to detect after the fact.

5. **Volatile or anti-bot sources**: if the bytes on retry differ
   noticeably from the initial fetch (the source is dynamic,
   rate-limiting, or returning different content to repeated
   requests), re-authoring against the second set of bytes produces
   a recipe optimized for a content shape that may not recur. The
   operator needs to see this and decide whether the source is
   fundamentally unsuitable.

6. **Scale explosion**: a plan with 10+ sources where all recipes
   hit Class B in a single run would consume 10 × 2 retries ×
   authoring cost at frontier tier — potentially 20+ minutes and
   meaningful API spend — without the operator ever choosing to
   incur that cost. The manual path makes the cost explicit before
   it is spent.

The minority report was reviewed and accepted as correct on the
central point: **the predicate strings are not yet evidence-grounded**.
Speculative predicates in a retry loop automate the wrong thing.

The automated architecture was not rejected — it is the right
eventual design. It was deferred until the evidence base is large
enough to make the predicates trustworthy.

---

## Decision

### Part 1 — Manual-first interim practice (in effect now)

The following protocol applies for all sessions until every condition
in §"When to automate" is met.

#### When a recipe fails at Apply stage

**Step 1.** The operator reads the fetch report. `RecipeOutcome::
Failed { stage: Apply, message }` carries the failure reason and
the source id.

**Step 2.** The operator classifies the failure:

- **Class B** (pattern matched nothing): proceed to Step 3.
- **Class C** (JS SPA / structural impossibility): do not re-author.
  Update `config/sources.toml` — add to the source's `description`
  field: "JS-rendered SPA; static HTTP response does not carry
  data." The fix is registry curation, not recipe authoring.
- **Class D** (value extracted but semantically wrong): do not
  re-author the extraction spec. Examine the plan's expectation
  and the record that landed. The fix is a plan refinement
  (re-classify with a more specific topic) or an `endpoint_hint`
  update targeting an API endpoint that returns structured data
  rather than prose.
- **Class E** (LLM response truncation): use the
  `XAI_WORKHORSE_MODEL` env var to escalate to frontier tier per
  the Session 13 handoff and the README. Do not re-author.

**Step 3.** For Class B only: present to the frontier LLM
(Grok Frontier or equivalent) in the conversation:

- The failure reason verbatim from the fetch report.
- The actual fetched bytes, or a bounded excerpt (≤32 KiB). These
  are the runtime bytes, not the pre-fetch excerpt — they may
  differ and the difference is exactly what matters.
- The original recipe's `extraction` spec.
- The plan's relevant expectation.
- An explicit ask: "Author a corrected recipe. Here is what the
  extractor saw and why it failed."

Do not ask for a re-author without these inputs. The empirical
data is what makes the second authoring pass effective.

**Step 4.** Evaluate the frontier LLM's response before persisting:

- Does the new extraction spec address the stated failure reason?
- Does it introduce a regression in any binding that was
  previously working?
- Is the new pattern structurally identical to the failed pattern,
  just rephrased? If yes: this is oscillation. Stop and document.

**Step 5.** After at most **2 re-author attempts** on the same
source within the same session, stop regardless of outcome. If
both attempts fail or regress, annotate the source in
`config/sources.toml` as Class B-resistant and move on. Do not
spend a third LLM call on the same source in the same run.

#### Frontier LLM pushback discipline

The frontier LLM is expected to push back in these cases. That
pushback is correct and must be heeded:

- **"I have now corrected this recipe twice with the exact bytes
  provided and it is still matching zero. The source structure
  may not be expressible with the closed extraction vocabulary."**
  → Stop. Document the source as Class B-resistant. Do not ask
  again in this session.

- **"This is not a pattern-matching issue. Re-authoring the
  extractor won't help because [reason]."**
  → Accept the reclassification. Apply the remedy for the class
  the LLM identifies.

- **"Source appears volatile between calls — stable extraction
  may be impossible."**
  → Document as Class C-adjacent. Do not re-author. Curate the
  registry entry.

- **"You haven't provided the failure message and fetched bytes."**
  → Provide them before proceeding. This is not optional.

#### Documenting observed Class B failures

Every Class B failure encountered during the manual-practice period
must be documented. Create a file in `docs/failure_cases/class_b/`
named `{YYYY-MM-DD}_{source_id}.md` containing:

- Source id and plan topic.
- Extraction mode and the failing spec verbatim.
- The failure message verbatim.
- The first 512 bytes of the fetched content (or the full content
  if shorter).
- Whether re-authoring succeeded, failed, or oscillated.
- The corrected extraction spec if re-authoring succeeded.

This documentation is the evidence base for automation. No
shortcuts. A session that skips the documentation is not building
toward the gate.

---

### Part 2 — Automated path architecture (deferred, do not implement yet)

This section records the eventual automated implementation. It is
here so future sessions do not redesign it. It is **not to be
implemented** until every condition in §"When to automate" is met.

#### Class B detection predicate

```rust
/// Returns true when an Apply-stage failure has a recognizable
/// "matched nothing" shape, indicating a Class B failure that
/// is a candidate for re-authoring. See ADR 0012.
///
/// IMPORTANT: each string in this predicate must be verified
/// against at least 2 observed live failures before being added.
/// Do not add strings speculatively. The predicate is
/// evidence-grounded or it is wrong.
pub fn looks_like_class_b_failure(stage: FailureStage, message: &str) -> bool {
    if stage != FailureStage::Apply {
        return false;
    }
    message.contains("matched 0 times")           // RegexCapture
        || message.contains("path matched no nodes")   // JsonPath
        || message.contains("selector matched no elements") // CssSelect
        || message.contains("no row matched filter")   // CsvCell
}
```

This lives in `fetch_executor.rs`. If `recipe_apply`'s error
messages change, this predicate must be updated in the **same
commit**. Divergence between the predicate and the error messages
is the misclassification risk that keeps automation deferred today.

The function is analogous to `looks_like_truncated_json` in the
xAI provider: a named, tested predicate that centralizes the
pattern, rather than inline string-matching scattered across the
executor.

#### Re-author entry point

```rust
pub async fn reauthor_recipe(
    provider: &dyn LlmProvider,
    tier: ModelTier,
    prompt_template: &str,
    plan: &ResearchPlan,
    ctx: &AuthoringContext,
    original_recipe: &FetchRecipe,
    fetched_bytes: &[u8],
    failure_reason: &str,
) -> Result<FetchRecipe, AuthoringError>
```

Lives in `crates/pipeline/src/recipe_author.rs`. Composes the
existing `build_prompt` output with a clearly delimited feedback
section:

```
--- REAUTHOR FEEDBACK ---
Your previous recipe failed at the extraction stage.
Failure reason: {failure_reason}
Actual fetched bytes (up to {REAUTHOR_EXCERPT_BUDGET} bytes):
{fetched_bytes}
--- END FEEDBACK ---
Instruction: Write a corrected recipe. Use the actual bytes above
to find stable selectors or patterns. Do not echo back the same
extraction that already failed.
```

`REAUTHOR_EXCERPT_BUDGET = 32 * 1024`. The bytes are from the
initial runtime fetch, retained in memory. No second network
call is made. `build_validated_recipe` is called on the LLM's
response — same validation path as first authoring. A reauthored
recipe that fails validation is an `AuthoringError`, not a
retried validation; no recursion.

#### Executor retry loop

`run_one_recipe` gains an internal retry counter (not part of
`ExecutorContext`). `MAX_CLASS_B_RETRIES = 2` — compile-time
constant, not configurable. Three total authoring attempts at
most per recipe per run. Changing it requires a code review; it
is a cost and latency policy.

After an Apply-stage failure:

1. `looks_like_class_b_failure(stage, &message)` → true,
   and `retry_count < MAX_CLASS_B_RETRIES`.
2. Call `reauthor_recipe` with original recipe, fetched bytes,
   and failure reason.
3. Reauthoring succeeds → persist via `save_recipe`, run
   `run_one_recipe` again with `retry_count + 1`. The final
   outcome replaces the intermediate failure in the report.
4. Reauthoring fails (LLM error, validation error) → surface the
   *original* Apply failure as the outcome. Log the reauthor
   failure at WARN. Do not surface it to the user separately.
5. `retry_count >= MAX_CLASS_B_RETRIES` → surface the final
   attempt's failure as-is.

Class C failures look identical to Class B at the predicate level
and will consume the full retry budget before the cap stops them.
This is the known, accepted cost of the cap — two wasted API calls
per Class C recipe when it fires in the automated path. The manual
path eliminates this cost entirely during the deferral period.

#### Storage: recipe version chain (migration v7)

New column on `recipes`: `prior_recipe_id UUID` (nullable). `NULL`
for first-authored recipes. Non-null points at the id of the
recipe being superseded. `dedup_key` stays unchanged across
versions (`{plan_id}:{source_id}`), so `save_recipe` upserts by
dedup_key and the new version becomes active. Old versions are
retained — storage is append-only per ADR 0007 §"Versioning vs
migration". The authoring history is walkable via `prior_recipe_id`
(a chain of at most `MAX_CLASS_B_RETRIES + 1` nodes).

Migration must follow the DuckDB `ALTER TABLE` discipline from
Session 7: no `NOT NULL DEFAULT` on an existing table that has
indexes. Add the nullable column, rely on Rust for the invariant.

---

## When to automate

Automation may be implemented when **all** of the following
conditions are true. These are not suggestions. A session that
implements the automated retry loop before all conditions are met
is drifting from this ADR. Purge and revert, the same way the
Session 2 `parse.rs` shortcut was purged.

**Condition 1.** Ten or more distinct Class B failures have been
observed in live runs, across at least three different extraction
modes (e.g. RegexCapture, CssSelect, JsonPath).

**Condition 2.** Each failure is documented in
`docs/failure_cases/class_b/` per the format in §"Documenting
observed Class B failures." The files must exist and be committed
before the predicate is coded. No files, no predicate.

**Condition 3.** The `looks_like_class_b_failure` predicate strings
have been verified against all documented cases. Each string in the
predicate must match at least two observed cases. A string that
matches only one case is not yet trustworthy — it may be too
specific to that instance. Remove it and observe more runs.

**Condition 4.** At least one Class C failure has been observed and
confirmed to look identical to Class B from the predicate's
perspective (Apply-stage failure, matched-nothing message). This
confirms the known misclassification risk is real and understood,
not theoretical. The case must be documented with a note: "This
case would have triggered re-authoring under the automated path;
the manual path correctly identified it as Class C."

**Condition 5.** Migration v7 (`prior_recipe_id`) has been applied
and verified in a real run — the version chain is visible via a
storage query before the automated loop starts writing to it.

---

## Consequences

### Good

- Class B is named, classified, and has a documented response
  protocol. Operators in future sessions know exactly what to do
  without reading multiple handoffs.
- The frontier LLM's pushback discipline is codified. A Grok
  response of "stop, this source is not addressable" is a correct
  signal, not a failure of the retry mechanism. Sessions that
  see this pushback know to document and move on.
- The automated architecture is fully designed and recorded.
  When the gate is met, implementation proceeds without redesign.
  The code in §"Part 2" is essentially copy-paste-ready.
- Every Class B failure encountered during the manual period
  produces one documented case. The gate fills itself through
  ordinary operation.

### Tradeoffs

- Class B failures require operator involvement until automation
  lands. This is the correct tradeoff at current scale. The manual
  path is more adaptive than an evidence-starved automated path.
- The documentation burden (10 cases in `docs/failure_cases/`) is
  real. It is also exactly the minimum evidence base for trustworthy
  automation. Skipping it is not faster — it moves the unreliability
  from the documentation step to the predicate step, where it is
  harder to detect.
- The §"Part 2" code will age. If `recipe_apply`'s error messages
  change, the predicate strings will need updating before
  automation can land. Future sessions must read this ADR before
  changing those messages.

### Neutral

- Class C, D, and E handling is unchanged from the current state.
  Their remedies are documented in §"Part 1" and the Session 13
  handoff.
- The primary path — fetch a plan with existing recipes, apply,
  insert — remains unconditionally LLM-free throughout, including
  during the deferral period. ADR 0007's runtime invariant is
  unaffected.

---

## Alternatives considered

### Implement automation now (ADR 0012 v1)

The original automated design was technically coherent. It was
deferred, not rejected, because the predicate strings are not
yet evidence-grounded and the Class C misclassification risk is
unquantified. "Technically coherent" and "safe to automate" are
different bars. The manual-first approach earns the evidence base
that makes automation safe. The design is preserved in §"Part 2."

### Keep ADR 0011's "user decides" posture unchanged

ADR 0011's posture was correct when the taxonomy didn't exist.
Now that Class B is named and its response is understood, "user
decides" without a documented protocol produces inconsistent
outcomes across sessions and operators. This ADR upgrades "user
decides" to "user follows this protocol."

### Manual "re-author this recipe" UI button

Valid future feature, additive to this ADR. When the gate is met
and automation lands, the button becomes "trigger the automated
path immediately." Before the gate is met, the button would expose
the manual protocol — which requires the operator to provide bytes
and failure reason anyway. The button does not change the
underlying discipline; it adds a UI affordance for it.

### Treat every Apply failure as Class B and retry

Rejected. Class D failures have no Apply-stage error message —
they look like successes. Class C failures produce Apply-stage
failures identical to Class B. Retrying all Apply failures would
retry Class C until the cap, spending real money on structurally
impossible runs. The predicate exists to narrow the trigger to
cases where retry is likely to help. Until the predicate is
evidence-grounded, "retry all Apply failures" is worse than the
manual path.

---

## Code references

Files to touch when automation is implemented (§"Part 2"):

- `crates/pipeline/src/recipe_author.rs` — `reauthor_recipe`.
- `crates/pipeline/src/fetch_executor.rs` — `looks_like_class_b_failure`
  and retry loop in `run_one_recipe`.
- `migrations/0007_recipe_prior_id.sql` — `prior_recipe_id` column.
- `crates/storage/src/recipes.rs` — `save_recipe` upsert; the column
  is a pure addition.
- `config/prompts/recipe_author.md` — the reauthor feedback section
  is appended at call time, not embedded in the template.
- `docs/failure_cases/class_b/` — must be populated before the
  predicate is coded.

Until automation: **no new code is required.** The protocol in
§"Part 1" is the implementation.

---

## Review notes

**ADR drafted and hardened 2026-04-30 (Session 14).**

Design inputs:

- Session 13 failure-mode taxonomy: three live runs, four named
  failure classes, documented in `SITUATION_ROOM_HANDOFF_SESSION13.md`
  and `SITUATION_ROOM_HANDOFF_SESSION14.md`.
- ADR 0012 v1 (Session 14, first pass): the automated path
  architecture, now recorded in §"Part 2" as the deferred design.
- Minority report from Grok Frontier (Session 14): six edge cases
  the automated loop cannot handle reliably at current evidence
  levels. Reviewed and accepted as correct on the central point:
  predicate strings are not yet evidence-grounded. The automated
  architecture was not rejected — only deferred, with a concrete
  gate for resumption.
- Session 2 handoff §"READ THIS FIRST": the lesson that a
  technically coherent shortcut is still a shortcut if it bypasses
  an ADR. That lesson applies here in the opposite direction. The
  gate conditions are the constraint; implementing automation before
  they are met is drift. Purge and revert.

**Hardening rationale:**

The "READ THIS FIRST" block and the explicit gate conditions exist
because this design went through a deliberate adversarial review
before being accepted. The reasoning is non-trivial. Future sessions
must not redo it on the basis of a failing test or a feeling that
the automated path is "almost ready." The gate conditions are the
minimum evidence base. Trust the process.
