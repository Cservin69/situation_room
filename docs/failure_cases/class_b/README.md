# Class B failure case archive

This directory is the evidence base ADR 0012 §"Documenting observed
Class B failures" calls for. **Do not implement the automated
re-author retry loop** until the gate in ADR 0012 §"When to automate"
is met: 10 or more empirically observed, distinctly-shaped Class B
failures across diverse sources and plan types, all documented here.

## Definition (verbatim from ADR 0012)

> **Class B** — recipe authored cleanly, but the extraction pattern
> matched nothing in the fetched bytes. The LLM authored against a
> description of the source or an idealized mental model of its
> content, rather than the actual bytes at runtime.

The deferred automated detection predicate looks for these specific
strings (each must be verified against ≥ 2 observed live failures
before being added — see ADR 0012 §"Class B detection predicate"):

- `matched 0 times` — RegexCapture
- `path matched no nodes` — JsonPath
- `selector matched no elements` — CssSelect
- `no row matched filter` — CsvCell

A failure that doesn't match any of these strings is **not** strict
Class B by the predicate's definition, but it may still belong here
if the root cause is the same shape: an LLM-authored recipe whose
assumption about the source's response shape was wrong. Document
the case anyway, label the taxonomy honestly (e.g. "Class B-adjacent:
extraction succeeded structurally but produced an untyped value"),
and flag it in the case file. Future sessions deciding whether to
extend the predicate need this evidence.

## File naming and schema

Per ADR 0012:

```
docs/failure_cases/class_b/{YYYY-MM-DD}_{source_id}.md
```

Each file must contain:

1. Source id and plan topic.
2. Extraction mode and the failing spec verbatim.
3. The failure message verbatim.
4. The first 512 bytes of the fetched content (or the full content
   if shorter).
5. Whether re-authoring succeeded, failed, or oscillated.
6. The corrected extraction spec if re-authoring succeeded.

When (5) and (6) cannot be filled in within the same session as the
observation, leave them as "Pending" and add a follow-up note —
ADR 0012 explicitly forbids the shortcut of skipping documentation
because the manual fix hasn't been done yet.

## Directory was empty until Session 24

The Session 23 verification run produced one Class B-adjacent
observation (gdelt rate-limited stub-authored to a wrong-field
recipe) but no Class B file landed because no `failure_cases/class_b/`
directory existed. Session 24's verification run (operator-machine,
fresh classification of "venezuela oil production") produced the
first concrete entry, `2026-05-03_world_bank_indicators.md`. The
directory was created in Session 24 alongside that entry; this
README establishes the convention so subsequent sessions have a
known-good schema to follow.

## Gate status (Session 67, 2026-05-13)

ADR 0012 Condition 1 wants ≥10 distinct Class B cases across ≥3
extraction modes. Current count:

| Mode          | Strict cases | Spec-grounded | Notes |
|---------------|--------------|---------------|-------|
| CssSelect     | 5            | 2 (pending fill-in) | 4× `www.nhc.noaa.gov` (Session 63/64); 1× `www.federalreserve.gov` (Session 65 screenshot; spec + bytes filled in Session 66). **Host-diverse as of Session 66**. |
| RegexCapture  | 1            | 1             | `rss_feeds` (BBC CDATA mismatch) |
| JsonPath      | 0            | —             | `world_bank_indicators` is **Class B-adjacent** (`null` vs f64), not strict. Session 67's `api_fema_gov_jsonpath_iterator_cap_exceeded` is **also Class B-adjacent** (cap-exceeded predicate, not on strict list — but proposed for extension; see file). |
| CsvCell       | 0            | —             | None observed |
| PdfTable      | 0            | —             | Phase 2B mode, may be rare |

Strict Class B total: **6** (unchanged from Session 66 — Session
67's hunt did **not** produce any strict-by-predicate JsonPath
case, contrary to the pre-run expectation). Class B-adjacent
total: **3** (world_bank + gdelt + fema_jsonpath_cap_exceeded).

**Why Session 67's hunt didn't produce strict JsonPath cases.**
Pre-Session 67, every `json_path × json_path` recipe was declined
at authoring with a NotImplemented validator gate (recorded in ADR
0019 Session 67 verification subsection). The Session 67 patch
closed that gate; the eval ran 5 trials and persisted 23 such
recipes, of which 11 succeeded with records (1047 total — biggest
single-topic haul ever) and 11 failed at apply with the
cap-exceeded predicate. Zero apply-time failures matched the
strict predicate `path matched no nodes` — because the structural
validator now catches that shape at authoring against the same
prefetched bytes the apply will see, so on stable APIs like FEMA
the validator and apply agree perfectly. The remaining apply-time
failure surface for JsonPath is bytes-shape-dependent (paginated
APIs that exceed our cap, or sources whose shape drifts between
authoring fetch and apply fetch).

**Implication for the predicate list.** Today's run is empirical
evidence that ADR 0012's four-predicate list under-covers
JsonPath's real apply-time failure modes on stable APIs. The
[`2026-05-13_api_fema_gov_jsonpath_iterator_cap_exceeded.md`](2026-05-13_api_fema_gov_jsonpath_iterator_cap_exceeded.md)
case file proposes extending the strict list with
`matched N elements; cap is N` (mode-shared between JsonPath and
CssSelect iterators). The extension decision belongs in a future
ADR 0012 amendment, not in this README. If accepted, today's 11
cap-exceeded failures convert from Class B-adjacent to strict
Class B, Condition 1 count climbs 6 → 17, and Condition 2 mode
diversity climbs 2 → 3 (closing Condition 2 outright).

Outstanding for Condition 1 (≥10 distinct strict): need 5+ more
strict cases under the current predicate list, OR adopt the
proposed extension above (which yields +11 immediately).

Outstanding for Condition 3 (each predicate string ≥2
spec-grounded cases):

- `selector matched no elements` (CssSelect): 2 ✓ (pending fill-in)
- `matched 0 times` (RegexCapture): 1 — need 1 more
- `path matched no nodes` (JsonPath): 0 — need 2 (note: stable-API
  validator-gating makes this predicate hard to produce at apply
  time; the natural sources are shape-drifting APIs or sources
  with prefetch-vs-fetch divergence — neither well-represented
  in the eval-harness yet)
- `no row matched filter` (CsvCell): 0 — need 2 (Session 66 +
  Session 67 confirmed BLS / Census CSV hosts are WAF-blocked,
  blocking the LLM from authoring CsvCell recipes; ADR 0009
  amendment territory)
- `matched N elements; cap is N` (proposed extension): 11 from
  Session 67 — but per the case file's discussion, the
  "spec-grounded" definition under the proposed extension may
  want shape diversity across hosts, not just attempt count
  on one host

Outstanding for Condition 4 (Class C disguised as Class B):
**none documented yet**. Several candidates exist in the eval
data (the `apnews.com/hub/hurricanes` JS-rendered hub that the
runtime sees as "no extractable structure" — close, but
typically declines at the URL proposer stage rather than at
apply; we need an apply-stage failure on a JS-SPA source whose
selector fires the Class B predicate string).

Outstanding for Condition 5 (migration v7 / `prior_recipe_id`):
the column **already landed** in Session 26 as
`migrations/0011_recipes_prior_recipe_id.sql` — ADR 0012 calls it
"v7" but actual numbering drifted to v11 because migrations
0001–0010 were used in the intervening sessions. The substrate
is in place; live verification of the chain via a storage query
is **pending**. The Fed re-author surface for recipe `019e1cbb`
(Session 64 chat screenshot) is the natural live-verification
opportunity: clicking re-author writes a new recipe row with
`prior_recipe_id = '019e1cbb'`, after which
`SELECT id, prior_recipe_id FROM recipes WHERE plan_id = <fed>`
closes Condition 5.

## Session 64 operator follow-up

Two pending tasks unblock fully-grounded entries against the
predicate string `selector matched no elements`:

1. **Run the SQL queries** named in the `Extraction mode and the
   failing spec` sections of
   `2026-05-12_www_nhc_noaa_gov_trial0_css_inner_no_elements.md`
   and the corresponding `trial4` file. Both reference the
   per-trial DuckDB at
   `/var/folders/.../situation_room-eval-019e1cd1-e0b6-7563-bc45-bbe0e413eb65/`.
   Paste the verbatim recipe spec and the leading 512 bytes back
   into the case files.

2. **Decide whether to attempt manual re-author** on either of
   the two recipes via the desktop app's recipe-detail panel
   (the same UI surface the Fed-volatility screenshot exercised).
   Document the outcome in the case files' "Re-authoring outcome"
   sections, including any pushback the frontier LLM produces
   per ADR 0012 §"Frontier LLM pushback discipline."

The fill-in pass converts the two 2026-05-12 cases from
"pending-spec-grounded" to "spec-grounded," which satisfies
Condition 3 for the CssSelect string. The re-authoring pass is
optional for gate-progress but is the only thing that produces
a "Corrected extraction spec" field — useful for predicate
training but not gate-required.

## What does NOT belong here

- **Class C, D, E** as defined in ADR 0012 §"The failure-mode
  taxonomy" — they have different root causes and different
  remediation paths. Re-authoring does not help any of them; logging
  them as Class B candidates pollutes the evidence base. If a Class
  C/D/E archive becomes useful later, create sibling directories.
- **Classifier failures** (UDB-style framing leak, etc.) — those go
  in `failure_cases/classification/` per the convention established
  in Session 15. Different taxonomy, different ADR.
- **Transient runtime failures** (429 rate-limits, network blips,
  DNS, transient TLS handshake failures) — these are not Class B by
  any reading; they're external state at fetch time. The recipe
  may be perfect; the fetch happened to fail. Logging them here
  inflates the gate count with non-evidence.
