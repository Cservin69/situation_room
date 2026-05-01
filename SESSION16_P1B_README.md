# Session 16 P1b — Recipe-author prompt v1.6

Bumps `config/prompts/recipe_author.md` from v1.5 to v1.6 and
updates the verification block of the existing failure-case
writeup at
`failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`.

The motivation is the v1.5 verification re-run on the C.2 plan
("EU AI Act high-risk system enforcement timeline"). v1.5 named
the EUR-Lex CELEX URL pattern as wrong in an anti-example, and
the LLM still produced exactly that URL. Diagnosis: the
pre-fetched `endpoint_hint` URL
(`https://eur-lex.europa.eu/search.html?scope=EURLEX&type=quick&lang=en`,
verified with curl) returned a search-form skeleton with no
result rows because it carries no `text=` query parameter.
Faced with v1.5's "use the listing endpoint" rule but no listing
items in the excerpt, the LLM substituted its training-data
knowledge of the AI Act's CELEX number and authored against the
instance URL.

v1.5 missed the third option: refine the URL. v1.6 names it.

This is a prompt-only patch. No Rust code changes. No schema
changes. No `endpoint_hint` rewrites in `config/sources.toml` —
the architecture rejects per-source coping logic; the LLM owns
the URL end-to-end. Output contract is unchanged. Recipes
already authored remain valid.

Apply on top of the green Session 16 P1 build:

    tar -xzf ~/Downloads/session16_p1b_patch.tar.gz --strip-components=1 -C .
    cargo test --workspace
    cargo test --workspace --doc
    cargo clippy --workspace --all-targets -- -D warnings
    cd apps/desktop && npm run check

No tests parse the prompt content, so the structural verification
should match the P1 baseline. The qualitative verification is the
GUI re-run:

1. From a fresh state (or after rejecting the v1.5-era plan),
   classify topic `EU AI Act high-risk system enforcement timeline`.
2. Accept the plan. Run fetch against `eur_lex`.
3. Open the Recipes panel. The `eur_lex` recipe should:
   - have `source_url` on `eur-lex.europa.eu/search.html?...`
     with a search-text query parameter populated (or another
     deterministic listing variant on the same listing endpoint
     family), **not** on `/legal-content/EN/TXT/?uri=CELEX:...`;
   - have an extraction selector that targets result-row
     structure, not a generic page tag like `h1` or `title`;
   - produce at least one record on apply.

If the URL is now a refined `search.html?...&text=...` and the
fetch produces records: open
`failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`,
change the `### v1.6 attempt — pending` block to a dated pass
record matching the v1.5 fail block's format, commit, push, P1
closes.

If the URL is still anchored on the CELEX page or some other
instance URL: that's a v1.7 signal. Surface the recipe and we
diagnose. Possibilities include moving the anti-example to
*after* the document excerpt (recency wins in long contexts),
adding a positive pre-submit checklist, or strengthening the
substitution-prohibition language.

## What this patch does

### Prompt (1 file)

`config/prompts/recipe_author.md` — bumped v1.5 → v1.6. Three
concrete additions:

- New subsection inside "URL discipline" titled **"Hunt the URL
  end-to-end"**, placed after "Endpoint discipline — instance
  vs listing". Tells the LLM: the pre-fetched URL is a starting
  clue, not a constraint; recognize when the excerpt isn't yet
  the listing of items the plan needs (language picker, empty
  search form, redirect notice, format chooser); refine the URL
  by adding language / format / query parameters or descending
  into a sub-resource; stop when the refined URL is the
  deterministic variant a human reader would land on; the
  schema has no decline path, so when in doubt ship the
  best-guess refinement and trust the rejection loop. Includes
  a worked anti-example using the EUR-Lex search-skeleton
  pattern from the v1.5 failure case.
- Two new bullets in **"What NOT to produce"**: one against
  authoring against an interstitial / chooser excerpt, one
  against substituting a training-data-known instance URL when
  the listing path underdelivers.

Changelog entry at the top of the changelog tail names the
verification result, the failure mode in concrete terms, and
the architectural reasoning (no decline path → must produce a
recipe → ship the refinement and trust the rejection loop).

### Failure case (1 file)

`failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md`
— updated `## Verification` section. The previous "Pending"
block is replaced with two subsections:

- `### v1.5 attempt — 2026-05-01: FAIL` records the observed
  recipe (URL still on CELEX:32024R1689, selector `h1`, 1
  binding for 2 expectations), the run id and outcome counts,
  the curl-verified diagnosis of the empty `endpoint_hint`,
  and the v1.6 fix in concrete terms.
- `### v1.6 attempt — pending` defines the next verification
  criteria: refined `search.html?...&text=...` URL, result-row
  selector, at least one record produced.

The file's other six sections (Topic, Observed recipe, What was
wrong, Chain, Diagnosis, Fix, What this case taught) are
unchanged.

## Architectural notes

- The schema check confirmed `RecipeAuthoringOutput` is a
  struct (no error/decline variant). The xAI structured-output
  schema constrains the LLM to produce
  `source_url` + `extraction` + `produces`. Architectural
  confirmation that the hunt is forced; v1.6's "ship the
  best-guess refinement" stop condition is the only honest
  shape under the current schema.
- The `endpoint_hint` in `config/sources.toml` for `eur_lex`
  was deliberately **not** changed. Rewriting it to
  `&text=<something>` would special-case one source; the
  architecture rejects that. The maintainer's hint stays as
  the maintainer's best-known starting point; the LLM's job is
  to refine it. v1.6 teaches the refinement.
- The recipe-author rejection loop (per-recipe rejection +
  feedback) is not yet implemented — only plans have the
  `rejection_reason` + `reclassified_from` machinery (Session
  15 P5d). The user's only path to correct a wrong recipe is
  reject the plan and re-classify. Adding per-recipe rejection
  feedback is a real future session, gated by observing the
  same recipe-author failure mode three times under a stable
  prompt (parallel to ADR 0012's gate conditions for runtime
  re-author).

## Files in this patch

    SESSION16_P1B_README.md                                                  this file
    config/prompts/recipe_author.md                                          v1.5 → v1.6
    failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md
                                                                             updated verification block
