# Session 53 — Patch 2

Three prompt-only edits, applied in one commit. Closes the gaps
the 2026-05-10 06:14 lithium re-run exposed in Patch 1's
prompt-side pieces (A.1/A.3/D). The Rust pieces (B/C/F) are
unchanged and still firing as designed; this patch is about
disposition language the LLM was reading too weakly.

## Live-test observations (2026-05-10 06:14, fresh DB)

- **1 record / 11 expectation slots** — only USGS reserves
  authored. 9 declines surfaced.
- **Piece B** — clean hit. USGS `obs_metric:3` declined at
  authoring time with `invalid type: string "Domestic",
  expected f64`. Pre-Patch-1 this was an apply-stage failure
  consuming a recipe slot; Patch 1 turned it into a
  per-target decline that flows back to run N+1's proposer.
- **Piece A two-step pivot** — partial hit on Reuters 401 →
  Reuters search 401 → mining.com/tag/lithium/ on attempt 3.
  The off-host pivot fired; the resulting page was a news
  aggregator without per-article structured figures, so the
  recipe-author declined all four targets. The pivot worked;
  the target landing-page choice was wrong.
- **Piece F** — every nomination logged
  `prior_decline_count=0 effort="Low"` correctly. Fresh DB,
  so escalation never had reason to fire. Verifies the read
  path; behaviour-test waits for run N+1.
- **Pieces E.1 / E.2** — host-backoff strip reads
  `host backoff · 1 host · this session` with the dot;
  review header pinned. Both verified visually.

Three failure modes that suppressed the success ceiling:

1. **Piece D didn't help USGS production** because the LLM
   declined preemptively at recipe-author time with
   *"PDF table cells contain comma-formatted numbers and 'e'
   prefixes preventing clean numeric extraction via
   pdf_table"* — exactly the shapes Piece D was added to
   accept. The author prompt didn't know the normaliser
   existed; it self-rejected on shapes the runtime would
   parse.
2. **The "reasonable shot" amendment didn't fire on
   auth-primary exhaustion.** SEC 403 → mining.com/results/
   404 → decline citing *"without fabricating parameters or
   paths"*. Same pattern on World Bank, Fastmarkets,
   Australia's RE Quarterly, IEA. The v1.2 amendment was
   present but read as advice, not as the default
   disposition; the surrounding "Don't guess" / "Honest
   decline beats wrong commit" discipline language was the
   dominant signal.
3. **The on-host refinement amendment didn't fire on IEA.**
   Two attempts both on overview landing pages
   (`***2023`, `global-ev-outlook-2024`). The amendment was
   buried in the middle of the
   `recipe author declined: no extractable structure` bullet
   and was outweighed by the bullet's earlier sentences.

## Pieces landed

### Piece D.2 — recipe-author normaliser awareness

`config/prompts/recipe_author.md`. Header bumped v1.15 → v1.16.

The "Type honesty" section's *"Numeric strings where a
number was expected"* bullet was rewritten. Previously it
said the comma-thousands form would be rejected by `f64`
deserialisation and that the LLM should pick a different
selector or decline. v1.16 says the apply-stage normaliser
**does** accept the common human-readable numeric shapes
and enumerates them so the LLM can see the surface area:

- ASCII thousand-separator commas at canonical positions.
- Currency markers as leading or trailing tokens (`$`, `€`,
  `£`, `¥`, ASCII `USD` / `EUR` case-insensitive).
- Estimate prefixes (`est. `, `est `, `~`, `≈`, and the bare
  `e ` form common in agency tables).
- Trailing units beyond the leading numeric prefix
  (`49,000 t`, `12.5%`).
- Scientific notation (`1.5e9`).
- Internal whitespace (`1 234.5`).

And names two shapes the normaliser refuses, where decline
remains the honest answer:

- EU-locale numerics (`1.234,56`) — ambiguity gate;
  `normalize_numeric_candidate` returns `None` rather than
  guess US- vs EU-locale.
- Strings in a numeric slot — the Piece B shape-validator's
  decline class. Re-author against a different selector or
  decline citing the column type.

Targets the USGS MCS production case head-on. Closes the
"Piece D doesn't help if the LLM doesn't know" gap.

### Piece A.4 — promote "reasonable shot" to top-level disposition

`config/prompts/propose_source_url.md`. Header bumped
v1.2 → v1.3. Three coordinated edits:

1. **Promote**: the `### The "reasonable shot" principle`
   subsection inside `## How to weight priority_tier` is
   removed. A new top-level section
   `## The "reasonable shot" disposition — when prior
   auth-primary attempts have exhausted` lands immediately
   above `## What NOT to propose`. The new section reads as
   a peer of Discipline rather than as advice nested under
   priority-tier weighting. Frames the disposition as the
   *default* on auth-primary exhaustion (not a permission
   slip), names the cost arithmetic explicitly (wrong URL on
   coverage host = same cost as decline; right URL = a
   record), enumerates the standard coverage-host path
   schemes (`/tag/<topic>/`, `/topic/<topic>/`,
   `/markets/<commodity>/`, `/commodity/<commodity>/`) so
   the LLM has anchors, and adds a worked example pair
   (forbidden vs reasonable, principle-only).

2. **Reword Discipline's guess bullet**: *"Synthetic guesses
   with no grounding"* → *"Fabricated paths or query
   parameters on opaque hosts"*. The old wording read as a
   blanket "if you don't know, decline"; the new wording
   names exactly which class is forbidden (path/parameter
   fabrication on auth-primary opaque hosts) and explicitly
   carves out the reasonable-shot class as not a guess.

3. **Reword Discipline's decline bullet**: *"Honest decline
   beats wrong commit"* → *"Honest decline beats fabrication,
   not coverage shots"*. Names the failure mode the live-test
   exposed: a decline rationale that reads "without
   fabricating parameters or paths" while no coverage-host
   tag/topic listing was tried is *skipping the disposition*,
   not honest exhaustion. Closes the loop with the
   reasonable-shot section's worked example.

The reasonable-shot disposition is now stated three times in
the prompt — at the new top-level section, in the discipline
guess bullet, and in the discipline decline bullet — at three
different layers (disposition, anti-pattern, fallback). The
LLM's reading order can reach it from any of them.

### Piece A.5 — lead-with-on-host-refinement in the no-structure bullet

`config/prompts/propose_source_url.md`, same v1.3.

The `recipe author declined: no extractable structure`
bullet under `## Reading prior attempts` was reordered. v1.2
had the on-host refinement guidance buried mid-bullet behind
two clauses on what the decline means. v1.3 leads with **the
default move is on-host refinement, not off-host pivot**,
followed by an explicit sub-list of path shapes to try
before pivoting off:

- Single-chapter PDF (`*/<report>/<chapter>.pdf`,
  `*/chapters/<chapter>.pdf`).
- Fact-sheet PDF (`*/factsheets/<topic>.pdf`,
  `*/briefs/<topic>.pdf`).
- Press release (`*/news/<slug>`, `*/press/<slug>`,
  `*/newsroom/<slug>`).
- Data-explorer export URL
  (`*/data-and-statistics/...`, `*/data/...`,
  `*/explorer/...`).
- API endpoint at documented base
  (`*/api/v<n>/...`, `*/services/...`).

These are class-only path shapes, not host strings — same
discipline as Patch 1. Off-host pivot is now explicitly the
*second* move, after every plausible focused on-host surface
is exhausted. Closes the IEA two-overview-pages gap from the
live-test.

## Files touched

```
config/prompts/recipe_author.md     (v1.15 → v1.16; Piece D.2)
config/prompts/propose_source_url.md (v1.2 → v1.3; Pieces A.4, A.5)
```

No Rust changes. No schema migrations. No test changes —
prompt-only edits don't move any compiled contract. The
existing Patch 1 Rust pieces (B/C/F) and UI pieces
(E.1/E.2) flow through unchanged.

## Apply

Files were edited in place. To verify (operator runs the
desktop binary against a fresh DB, same lithium classify +
fetch as the 06:14 run):

- **Piece D.2** — USGS MCS production target authors a
  recipe (instead of declining preemptively on
  comma/'e' grounds). The runtime's normaliser handles the
  cells at apply time. If apply itself fails on these
  shapes, that's a Piece D bug, not a D.2 prompt bug.
- **Piece A.4** — at least one of the four auth-primary
  exhaustion cases (SEC, World Bank, Fastmarkets, RE
  Quarterly) takes a `/tag/<topic>/` or `/markets/<commodity>/`
  shot at a coverage publisher on attempt 2 or 3 instead of
  declining with "without fabricating". The decline rationale
  on a true exhaustion now references the reasonable-shot
  language (e.g. "no plausible coverage publisher with a
  static tag listing on this metric class").
- **Piece A.5** — the IEA nomination's attempt 2 proposes a
  focused on-host surface (a chapter PDF, fact sheet, press
  release, or data-explorer URL on iea.org) rather than
  another overview HTML page. Off-host pivot only on
  attempt 3, after focused on-host has been tried.

End of patch.
