# Session 56 — Handoff

Session 56 set out to live-test Patch 4 (recipe_author v1.18) and pick the
next move. The actual outcome was bigger than the prompt question: five
trials on the lithium fixture (4× v1.18 + 1× v1.16 paired) produced
records 0/1/1/2/3 — per-trial variance is larger than any prompt-version
effect we tried to detect. v1.18 stays as the accepted state. Two
production-affecting issues surfaced that are independent of prompt
content; one is fixed in this session, one is deferred to ADR work.

## What landed

**Single commit this session: the 256 KiB prompt-cap fix.**

`crates/pipeline/src/recipe_author.rs::build_prompt_with_fence_id`. The
post-assembly `check_string` was crashing entire nominations wholesale
when the prefetched body + scaffolding exceeded `LLM_PROMPT_BODY` (256
KiB). v1.18 run 2 hit this on the URL proposer's choice of full
`pubs.usgs.gov/periodicals/mcs2024/mcs2024.pdf` (~250 KB body) instead
of the lithium chapter: assembled prompt 267 413 > 262 144, the entire
USGS nomination errored before any of its four targets could be
authored.

The fix:

- Removed the early per-field `check_string` on `document_excerpt`
  (subsumed by truncation).
- Replaced 8 of the 9 placeholders first; computed the remaining budget
  for the excerpt as `LLM_PROMPT_BODY − static_size − 512 byte safety
  margin`, divided by placeholder count (defensively for N occurrences;
  in production it's always 1).
- Truncated the excerpt at a UTF-8 char boundary at-or-below the budget.
  Appended a `[document excerpt truncated to fit prompt budget; original
  X bytes, retained Y bytes]` marker so the LLM can see the body was
  clipped.
- Kept the post-assembly `check_string` as a safety net for the
  degenerate case where the static portion alone overflows the budget
  (misconfigured plan, pathological feedback string).

Truncation is strictly safer than rejection — the model can still
operate on a partial document, while a wholesale rejection costs the
entire nomination's authoring budget.

Tests updated:

- `build_prompt_rejects_oversized_excerpt` → renamed to
  `build_prompt_truncates_oversized_excerpt`. Asserts `out.len() ≤
  LLM_PROMPT_BODY`, marker present, no orphan placeholder.
- New `build_prompt_preserves_in_budget_excerpt`: small excerpt passes
  through unchanged, no marker present.

All cargo tests green after the change.

## Live-test results — five trials, the variance finding

Same lithium plan, 7 nominations, same parallelism config across all
trials. Single-trial outcomes:

| Trial | Prompt | Records | Wall-clock | What succeeded / what crashed |
|---|---|---|---|---|
| Patch 3 verification (Session 55 handoff) | v1.17 | 2 | 2:29 | USGS production + reserves |
| Patch 4 run 1 (Session 56 start) | v1.18 | 1 | 2:51 | USGS production only; reserves declined "no compatible iterator" |
| Patch 4 run 2 (Session 56) | v1.18 | 0 | 2:37 | USGS crashed wholesale on 256 KiB cap (full mcs2024.pdf) |
| Paired baseline (Session 56) | v1.16 | 1 | 1:58 | USGS reserves only; production declined "PDF column whitespace" |
| Post-revert run (Session 56) | v1.18 | 3 | 3:01 | USGS production + reserves + Yahoo-Finance ALB spot_price record |

Records-per-trial range 0–3, mean ≈ 1.4, std ≈ 1.0. Per-trial variance
larger than any v1.16-vs-v1.18 effect we tried to detect on a single
trial each.

**Two compounding sources of non-determinism**:

1. URL proposer (Cheap-tier, temperature > 0): mcs2024.pdf vs
   mcs2024-lithium.pdf, Reuters→Bloomberg vs Reuters→FT,
   `iea.org/reports/global-ev-outlook-2024` vs
   `iea.org/data-and-statistics/data-browser` vs `reports/critical-
   minerals-outlook`, etc.
2. Recipe author (Workhorse-tier, temperature > 0) on the resulting
   (URL, target) pair: USGS production succeeds in some runs, declines
   in others; same for reserves. Different trials find different
   reasons to decline the same target on the same URL ("no iterator",
   "PDF column whitespace", succeeded).

The Patch 3 verification's "2 records" result that the Session 55
handoff used as the reference point was, by this evidence, likely a
single lucky trial. We over-weighted it in Session 55.

## Patch 4 verification verdict

Inconclusive on a per-criterion basis. v1.18 stays as the accepted
state — one v1.16 trial at 1 record cannot distinguish it from the
v1.18 distribution (range 0–3, includes 1).

The five Session-55 acceptance criteria, re-evaluated against
distribution rather than single trial:

1. **Container-selector decline doesn't recur.** Mixed. v1.18 run 1
   didn't trigger any container-selector path. v1.18 run 3 hit it on
   IEA Global EV Outlook (112 KB extraction) and the verbatim 3A shape
   fired correctly at authoring time. Patch 3A is doing the work it
   was supposed to do when the path is exercised.
2. **Fastmarkets-class produces ≥1 coverage-publisher attempt.** Not
   firing reliably. v1.18 run 1: stayed on host both attempts. v1.18
   run 2: stayed on host both attempts. v1.18 run 3: stayed on host
   all three attempts ("***ium/", "***ide/", "battery-materials/"). The
   "name an alternative" framing in 4B item 4 is being satisfied with
   a one-line dismissal ("no responsive coverage-publisher listing")
   rather than a concrete tried-and-failed pivot. **4B item 4 is not
   firing as designed.**
3. **USGS production succeeds without comma/iterator self-decline.**
   Mixed. Production succeeded in v1.17, v1.18 run 1, v1.18 run 3.
   Reserves succeeded in v1.17 and v1.18 run 3 only. Both targets
   succeeded only in v1.17 (one trial) and v1.18 run 3 (one trial).
   The decline shape varies wildly: comma-thousands (anticipated, not
   seen), iterator-shape (run 1), column-whitespace (v1.16 paired). The
   model finds a different reason to decline each time. **Patch 4A's
   negation list isn't suppressing the failure modes — they keep
   shifting.**
4. **Records ≥ 2.** Met by v1.18 run 3 only. Three of four v1.18 trials
   missed.
5. **Wall-clock ≤ 2:30.** Missed in 3 of 4 v1.18 trials. industry.gov.au
   60s timeouts × 2 attempts dominate every run; nothing about Patch 4
   touches that.

**The honest read**: Patch 4 is not obviously worse than Patch 3 on
this fixture, but the variance prevents us from claiming either way.
v1.18 stays in place because there's no evidence to revert. Future
prompt-edit decisions on this fixture need ≥5 trials at the same
prompt before drawing distributional conclusions.

## Real findings that survive variance

These are the things every Session 56 trial agreed on, regardless of
prompt version:

### 1. 256 KiB prompt-cap crash — FIXED this session

Surfaced in v1.18 run 2 only, but would have hit any prompt version
given the same proposer output (URL proposer is a separate LLM call
from recipe author; same v1.4 across all trials). The fix lives in
`build_prompt_with_fence_id` per "What landed" above.

**Verification next session**: re-run the lithium plan and force the
URL proposer toward `mcs2024.pdf` (or any large-document URL). The
expected behaviour is the truncation marker appearing in the
authoring excerpt and the nomination NOT crashing wholesale —
individual targets may still decline because the partial document
doesn't carry the expected data, but the nomination as a whole
should produce at least one authored recipe per accepted URL where
it would have produced zero before.

If the proposer happens to pick `mcs2024-lithium.pdf` again next run,
the fix isn't exercised but isn't regressed either. Worth running a
trial with `SR_LLM_CONCURRENCY=1` and observing whether the proposer
ever picks `mcs2024.pdf` again — it did so on 1 of 4 v1.18 trials, so
roughly 25% chance per trial in this empirical sample.

### 2. industry.gov.au 60s timeouts — wall-clock floor on every run

Every Session 56 trial saw `https://www.industry.gov.au/p***erly`
(attempt 1) AND `.../s***2024.pdf` (attempt 2) BOTH time out at 60s.
Combined ~2:00 wall-clock per trial. No trial has reached this host
successfully in any session.

This is now the architectural lever for go-live wall-clock if you
want to ship at sub-2-minute latency on lithium-class plans:

- **Per-host timeout backoff**: skip the prefetch entirely (or cut
  the timeout to ~5s) for hosts with prior-attempt timeout history.
  Complications: where to store host history (per-process? per-
  workspace? expiry?), how it interacts with the "named L1 source
  is provenance class hint, not contract" discipline (Patch 3D) —
  if we skip industry.gov.au, do we automatically pivot to the
  coverage-publisher tier? That's an architecture choice, not a
  config knob.
- **Or aggressive retry caps when prior attempts on the same host
  all timed out**: simpler. Attempt 1 times out → don't retry on
  the same host. Each timeout would still cost 60s but the second
  one wouldn't.

ADR territory — neither is a one-commit edit. Deferred.

### 3. Yahoo-Finance-class spot_price misroute — semantically wrong records

Surfaced in v1.18 run 3: `finance.yahoo.com/quote/ALB/sec-filings`
authored against the SEC EDGAR nomination's `spot_price` target. The
recipe almost certainly extracts Albemarle's stock price, not lithium
hydroxide/carbonate spot price. The schema slot is satisfied; the
substance is wrong. This is exactly the failure shape the closed-
vocabulary discipline tries to discourage: a stock-price scalar can
look like a "spot_price" data type, but it answers a different
question.

**Worth verifying in the records UI when reviewing the run**: if the
"spot price" value is in the $80–$140 range, it's ALB stock; if it's
in the $10 000–$30 000/tonne range, it's actual lithium pricing. The
former is the failure mode; the latter would mean the proposer's
pivot was correct.

This is the territory of **Patch 3E** — target-vs-nomination routing
at the L1 classifier. Restated below; carried forward as an open ADR.

## Positive signals (with caveats)

These are real and worth keeping, but their incremental value
relative to v1.16 is unclear given the variance evidence.

- **Container-selector decline (Patch 3A)** fired correctly in v1.18
  run 3 on IEA Global EV Outlook's 112 KB extraction. The verbatim
  apply-stage decline shape is reaching the LLM as designed.
- **L1-as-class pivoting (Patch 3D / 4B)** appears across runs:
  Reuters → Bloomberg → FT (v1.16), SEC → Yahoo Finance (v1.18 run 3),
  IEA reports → IEA data browser (v1.18 run 3). v1.16 paired showed
  this too — the proposer was already willing to pivot before Patch
  3D was added; Patch 3D may be writing down a behaviour the proposer
  already had.
- **Stage 1 / Stage 2 parallelism** continues to deliver. All four
  USGS Workhorse author calls returned within the same millisecond
  in v1.18 run 3 (15:29:08.747). The 9:42 → ~2:00–3:00 wall-clock
  speedup remains stable across sessions.

## Deferred items — worth thinking about, not shipping today

Three items carried from the Session 55 handoff plus three new ones
surfaced or sharpened in Session 56.

### Carried — Deferred A: gold-standard surgery on inline anti-examples

Unchanged from Session 55 handoff. Some inline anti-examples (EUR-Lex
CELEX, World-Bank-default-indicator) earn their keep; others repeat
content that positive examples already cover. Selective trim, not
wholesale removal. Class-only (no host strings) per closed-vocabulary
discipline.

### Carried — Deferred B: decision-tree restructure (potential v2.0)

Unchanged from Session 55 handoff. Replace the "reference manual by
topic" structure with stepwise decisions the model must answer in
order. The strongest argument for this rewrite has shifted: Session
56's variance evidence suggests the LLM is making different decisions
on the same inputs across trials, which a stricter decision-tree
structure may or may not address. Pre-condition: we'd want the eval
harness (below) before committing to a v1.18 → v2.0 side-by-side
comparison.

### Carried — Deferred C path 2: two-pass call (free-form reasoning + structured JSON)

Unchanged from Session 55 handoff. The variance evidence makes this
more attractive than the v1.18 → v1.17 paired-trial result alone
suggested. The empirical claim: an explicit reasoning channel reduces
per-call non-determinism on the JSON output. Cheap to A/B test once
the eval harness exists (run N=5 trials at v1.18 single-pass vs N=5
trials at v1.18 two-pass and compare distributions). Doubles
Workhorse cost during the experiment; not in itself a blocker if the
results are clearly better.

### NEW — Patch 3E (target-vs-nomination routing) — sharpened

Restated and sharpened from the Session 55 handoff. Session 56's
Yahoo-Finance ALB spot_price record is the strongest concrete
example yet. Each Workhorse call against a structurally-misrouted
(URL, target) pair (USGS / refining_capacity, USGS / spot_price,
World Bank / production, etc.) costs one decline. With Patch 3D's
class-not-contract pivoting, the proposer can now reach
finance.yahoo.com on a SEC EDGAR nomination — but recipe author then
authors a wrong-substance recipe rather than declining at the
target-class level.

Two architectural directions remain on the table:

1. **Per-nomination target whitelisting** at L1 — emit `targets:
   [observation_metric:0, observation_metric:1, …]` alongside each
   nomination, listing which record buckets the source plausibly
   serves. Executor skips off-whitelist targets.
2. **Per-target descriptor on the nomination** — symmetric but
   inverted. Each nomination carries an explicit list of targets it
   was nominated for; the executor iterates only those.

Both have schema-migration implications (`DocumentSourceNomination`
shape change). Both want an ADR before code.

### NEW — Per-host timeout backoff

Detailed in "Real findings" above. The wall-clock lever for
sub-2-minute lithium plans. ADR territory.

### NEW — Eval harness for distributional comparison

Without it, every prompt-or-runtime experiment lands in the same
single-trial fog Session 56 was stuck in. Smallest viable shape: a
shell script (or `cargo run` binary) that invokes the same Tauri
commands the desktop UI does (`classify` + `run_fetch_for_plan`) for
"lithium supply chain" N=5 times, captures records/trial and the
authoring decline reasons, and prints a one-line distribution
summary. Maybe 30 minutes of work. Doesn't need to be fancy.

Build this when the next experiment requires distributional comparison
(Deferred B side-by-side, Deferred C path 2 A/B test, or the
prompt-cap fix's verification under load), or when the operator asks
for it directly.

## Verification of the prompt-cap fix — what to look for next session

Session 57 first move: re-run the lithium plan once and look for these
shapes in the logs:

1. **No `recipe authoring failed wholesale: prompt construction failed:
   input exceeded llm_prompt_user limit:` line.** That was the
   pre-fix crash. With truncation, the assembled prompt fits the
   budget by construction.
2. **If the URL proposer picks a large-document URL** (e.g.
   `mcs2024.pdf` rather than `mcs2024-lithium.pdf`), look for the
   marker `[document excerpt truncated to fit prompt budget; original
   X bytes, retained Y bytes]` — it should appear in the authoring
   call's prompt body. (You can confirm this by enabling the
   recipe-author input dump if there's a tracing flag for it; otherwise
   the absence of the wholesale crash is sufficient evidence.)
3. **Records ≥ 1 even when proposer picks a large URL.** Pre-fix, that
   path produced 0 records. Post-fix, a partial document should still
   support at least one (URL, target) authoring success.

If all three hold, the fix is verified. Then pick the next move from
the deferred list.

## The discipline this handoff is structured around (carried from Session 54+55)

- **Each commit is a reset target.** This session's single commit is
  the prompt-cap fix. If the next-session live-test reveals a
  regression, `git reset --hard <pre-Session-56 head>` reverts cleanly
  without affecting Session 55's prompt or parallelism work.
- **Live observation only at the end of the work.** Session 56
  followed this for the prompt-cap fix (no intermediate live-test
  during the edit).
- **Reset is a first-class option for prompt edits too.** v1.18 stays
  not because we proved it's better than v1.17, but because we have
  no evidence to revert and the variance evidence makes the question
  harder than a single paired trial can resolve. If a future session
  wants to re-test, the operator now knows it needs to be N≥5 trials
  per prompt.
- **Single-trial verdicts on lithium are not reliable.** Session 56's
  most important finding. Future Patch-class experiments need a
  distribution, not a number. The eval harness (Deferred — NEW above)
  is the prerequisite for any further prompt-edit decision-making
  with a straight face.

End of handoff.
