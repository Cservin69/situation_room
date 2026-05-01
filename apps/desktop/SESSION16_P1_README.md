# Session 16 P1 — Recipe-author prompt v1.5

Bumps `config/prompts/recipe_author.md` from v1.4 to v1.5 and
opens a new `failure_cases/recipe_author/` directory parallel to
`failure_cases/classification/` and `failure_cases/class_b/`.

The motivation is Session 15 Imperfection #1: the C.2 fetch run
("EU AI Act high-risk system enforcement timeline", a three-event-
type plan against `eur_lex`) produced a recipe pointing at the
EUR-Lex CELEX URL for one specific regulation with a `"title"`
selector — an instance URL where the plan needed a listing
endpoint, naive selector, single binding for a three-expectation
bucket. Failed gracefully at apply (no garbage records), but the
silent partial-coverage shape was invisible without the
SatisfactionPanel.

This is a prompt-only patch. No Rust code changes. No schema
changes. No migration. Output contract is unchanged — same JSON
Schema, same field-source kinds, same binding rules. Recipes
already authored remain valid; recipes that exhibit the v1.5
symptoms can be rejected and re-authored.

Apply on top of the green Session 15 build:

    tar -xzf ~/Downloads/session16_p1_patch.tar.gz --strip-components=1 -C .
    cargo test --workspace
    cargo test --workspace --doc
    cargo clippy --workspace --all-targets -- -D warnings
    cd apps/desktop && npm run check

No tests parse the prompt content (it's `include_str!`-ed at the
binary level only — see `apps/desktop/src-tauri/src/main.rs:49`
and `crates/pipeline/src/fetch_executor.rs:216`). The verification
gate is the live ignored test plus a manual GUI re-run:

    cargo test -p situation_room-pipeline live_author_recipe \
        -- --ignored --nocapture

Then re-run the C.2 fetch in the GUI ("EU AI Act high-risk system
enforcement timeline") and confirm:

- The recipe's `source_url` is on `eur-lex.europa.eu/search.html`
  (or another listing endpoint), not on `/legal-content/EN/TXT/?uri=CELEX:...`.
- The recipe's `produces` array is honestly narrow (one binding
  for the most-load-bearing expectation) or honestly broader
  (multiple bindings each pointing at a distinct expectation
  index where the single extracted scalar genuinely populates
  each), with no padding.

If verified, mark the failure-case writeup's "Verification" section
done and commit.

## What this patch does

### Prompt (1 file)

`config/prompts/recipe_author.md` — bumped v1.4 → v1.5. Three
concrete additions:

- New subsection inside "URL discipline" titled **"Endpoint
  discipline — instance vs listing"**. Tells the LLM: when the
  matching bucket holds two or more expectations of the same
  record type, the URL must be a listing endpoint; if the source
  has a registered `endpoint_hint`, prefer it; deviate only with
  a clear source-specific reason. Includes a worked anti-example
  using the EUR-Lex CELEX failure pattern.
- New top-level section **"Coverage discipline — bindings vs
  expectations"**. Names the runtime constraint (one scalar per
  fetch in `recipe_apply.rs`), describes when multiple bindings
  off one scalar constitute genuine vs fake coverage, and
  instructs the author to prefer honest narrow coverage over
  padded bindings when the single extraction can't honestly
  populate the full bucket.
- Two new bullets in **"What NOT to produce"**: one against
  instance URLs for multi-expectation buckets, one against
  padded `produces` arrays.

Changelog entry at the top of the changelog tail names the
failure-case writeup, references the runtime-constraint code
sites, and notes the output contract is unchanged.

### Failure-case directory (2 files)

`failure_cases/recipe_author/` — new sibling directory to
`classification/` and `class_b/`.

- `README.md` — establishes the three-surface taxonomy
  (classification / recipe-author / Class B runtime), explains
  why recipe-author failures need their own evidence trail (they
  fit neither classifier misframings nor ADR 0012's runtime
  classes), enumerates when to add a file (instance-vs-listing
  mismatch, dishonest coverage, naive selectors, framing-leak
  field mappings), specifies the seven-section file format
  matching `classification/`'s discipline.
- `2026-05-01-eur-lex-celex-instance-naive-selector.md` — the
  C.2 case writeup. Topic and plan; observed recipe; what was
  wrong (three failures in increasing severity); chain of
  contamination through prompt v1.4's two specific gaps;
  diagnosis pointing at "URL discipline" being silent on tier
  and "What to produce" being silent on coverage; fix linked
  to v1.5 sections; verification "Pending" pending re-run.

## Architectural note flagged in the chat

The handoff's P1 phrasing — "produce N bindings (or N recipes if
the source can't cover them all in one)" — was traced against
the runtime:

- `recipe_apply::apply` extracts one scalar per fetch and applies
  all bindings to that single scalar
  (`crates/pipeline/src/recipe_apply.rs:117-130`).
- `load_or_author_recipes` calls `author_recipe` once per source
  per call (`crates/pipeline/src/fetch_executor.rs:424-462`); the
  authoring API returns one recipe.

The "or N recipes" path isn't expressible in today's API. The
"N bindings off one scalar" path is genuine coverage only when
the same datum legitimately populates each expectation; otherwise
it's padding. v1.5 reflects this honestly: endpoint discipline as
the unambiguous fix for the EUR-Lex case, coverage discipline
that prefers honest narrow coverage to padded breadth.

The bigger architectural lift (multi-value extraction modes, or
multi-recipe-per-source authoring) is real but outside Session 16
scope. Worth its own ADR if/when it lands.

## Files in this patch

    SESSION16_P1_README.md                                  this file
    config/prompts/recipe_author.md                         v1.4 → v1.5
    failure_cases/recipe_author/README.md                   new
    failure_cases/recipe_author/2026-05-01-eur-lex-celex-instance-naive-selector.md
                                                            new
