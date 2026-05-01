# Session 19 — handoff

**Prior session:** Session 18 (2026-05-01)
**Repo:** `/Users/aben/RustroverProjects/situation_room`
**Baseline test count:** 340 green (was 327 pre-Session-18; +13 from
the static_payload work)

---

## What Session 18 shipped

### Recipe authoring (LLM-only specialist, ADR 0007)

- **Prompt v1.4 → v1.7** (single-session bump in three increments).
  - **v1.5** added an *Endpoint discipline — instance-vs-listing*
    section and a *Coverage discipline* section, plus the EUR-Lex
    CELEX-as-instance anti-example. Two new "What NOT to produce"
    bullets.
  - **v1.6** added an *Hunt the URL end-to-end* subsection covering
    refining (not substituting) the search-URL — the listing-stays-
    listing rule. One search-skeleton anti-example. Two more "What
    NOT" bullets.
  - **v1.7** added a *Strategy for PDF sources — HTML first, static
    payload fallback* section between the URL discipline and the
    Document excerpt block, with three worked examples
    (HTML-found, HTML-absent-bake, anti-example bake-when-HTML-
    exists). The `static_payload` field was added to the "What to
    produce" top-level shape with explicit empty-string default.

- **Failure-case writeup**
  `apps/desktop/failure_cases/recipe_author/
  2026-05-01-eur-lex-celex-instance-naive-selector.md` —
  Verification block split into v1.5-attempt and v1.6-attempt
  subsections; Status header updated. **Both attempts are
  unverified pending Session 19's run** (see P1 below).

### Bake path for un-addressable sources (ADR 0007 Amendment 3)

The architectural shape: a recipe-level optional `static_payload`
field on `FetchRecipe`. When `Some(payload)`, the runtime serves
the baked bytes to extraction in place of an HTTP fetch. The
closed extraction-mode enum stays at five (`csv_cell`,
`json_path`, `css_select`, `pdf_table`, `regex_capture`). The
bytes' provenance is orthogonal to the extraction mode — the
runtime branches at byte-acquisition time only; `apply()` never
sees the distinction.

Wire shape (xAI structured-output idiom): `String` with empty-
string-as-absent on `RecipeAuthoringOutput`, validated to
`Option<String>` in `build_validated_recipe`. JSON-only payload
validation (`serde_json::from_str::<Value>`). Existing recipes
deserialize unchanged via serde defaults.

Persistence: migration `0008_recipes_static_payload.sql` adds a
nullable TEXT column. Same DuckDB-ALTER discipline as
migrations 0005 and 0007.

UI: visible **BAKED badge** in the recipe head (warning-tone,
`--signal-warning` per ADR 0006) plus a collapsible payload
preview block. Tooltip on the badge explains the bake-time-
frozen freshness contract.

ADR 0007 has Amendment 3 appended (~135 lines) covering the
design rationale, why option (b) recipe-level field over (a)
sixth extraction mode, validation discipline, code references,
and bounded scope (clarifying that `pdf_table` stays unwired
and the runtime stays LLM-free).

### Housekeeping

- ADR 0012 moved from `adr/` to `docs/adr/` — alignment with
  the rest of the ADR series. No content change. The `adr/`
  directory was removed from the working tree.

---

## Codebase state at session close

### Test count: 340 green

Per-crate baseline:
- `situation_room-api`: **42** (was 40, +2 RecipeDto static_payload tests)
- `situation_room-core`: **31**
- `situation_room-llm`: **24** + 2 ignored (live xAI tests)
- `situation_room` binary: **9**
- `situation_room-pipeline`: **137** + 4 ignored (live xAI / live
  network) — was 129, +8 (3 recipes serde tests, 4 build_validated
  recipe-author tests, 1 fetch_executor short-circuit test)
- `situation_room-secure`: **44**
- `situation_room-storage`: **53** (was 50, +3 storage round-trip tests)

Doc-tests: 0 active (all ignored as before).
Frontend: `svelte-check` 0 errors / 0 warnings.

### ADRs (status)

| ADR  | Subject                                | Status       |
|------|----------------------------------------|--------------|
| 0001 | Workspace shape                        | settled      |
| 0002 | Tauri + DuckDB + xAI stack             | settled      |
| 0003 | Six record types                       | settled      |
| 0004 | UUIDv7 + dedup_key + provenance        | settled      |
| 0005 | Plan status / lifecycle                | settled      |
| 0006 | Color is meaning, not decoration       | settled      |
| 0007 | Research function (LLM-only)           | **Amendment 3** (Session 18) |
| 0008 | Offline mode                           | settled      |
| 0009 | Security primitives (`stockpile_secure`) | settled    |
| 0010 | Topic-based subjects                   | settled      |
| 0011 | Plan lifecycle and fetch executor      | settled      |
| 0012 | Re-author on failure                   | gate not met (need ≥10 Class B failures) |

### Documented failure cases

`apps/desktop/failure_cases/recipe_author/`:
- `2026-05-01-eur-lex-celex-instance-naive-selector.md`
  — Class A (prompt-discipline). v1.5 and v1.6 attempts both
  unverified pending re-run.

### Repository hygiene flags

- Repo root carries accumulated `SESSION*` patch READMEs and
  `situation_room.duckdb*.broken-2026-05-01` files. Not blocking.
  Deferred to a deliberate housekeeping pass.
- `RecipeDto.ts` is currently a hand-mirror of the ts-rs output;
  ts-rs will overwrite on first `cargo test --package situation_room-api`.

---

## Session 19 priorities

### P1 — Empirical verification of v1.7

The v1.7 prompt has not been run against real plans yet. The
verification matrix has three slots:

**a. The CELEX-as-instance failure case (re-run).**
Topic: *"EU AI Act high-risk system enforcement timeline"*.
Plan ID can be reused from Session 17's reclassify lineage.
Expected behaviour: the LLM produces a search-listing recipe
against EUR-Lex (refined via query string, not substituted to a
known CELEX), with `static_payload = ""`. Update the failure-
case writeup's Verification block with the v1.7 outcome (PASS
fills the v1.6-attempt subsection's outstanding result; FAIL
opens a v1.7-attempt subsection).

**b. The BAKED path against a PDF-only source.**
Pick a source from `config/sources.toml` whose primary publication
format is PDF and whose website does not provide an HTML
equivalent. Candidate: a small central bank's press-release
endpoint, or a regulator without a structured-data feed. If no
such source is registered today, the verification can be
synthesized: stand up a topic that requires bake-time
transcription, observe the LLM's `static_payload` output, confirm
the runtime serves the baked bytes through the existing
extraction modes, confirm the **BAKED badge** appears in the
RecipesPanel, confirm the collapsible payload preview renders the
verbatim bytes.

**c. The HTML-equivalent-found path.**
The anti-example in v1.7 §"Strategy for PDF sources" warns against
baking when an HTML route exists. Confirm the LLM picks HTML
when both formats are addressable. Candidate: USGS MCS
(`mcsYYYY-<commodity>.html` companion to the PDF) — a chapter
that's already wired in the registry.

Do **not** treat green tests as verification; per project
discipline, verification is an LLM run against real data with
critical reading of the records produced. The Session 18 closing
position empirically validated the deterministic runtime, the
storage shape, and the type plumbing — not the prompt.

The output of P1 is either: PASS recorded in failure cases (and
the EUR-Lex failure case marked Resolved), or one or more new
failure-case writeups in
`apps/desktop/failure_cases/recipe_author/` for the specific
patterns observed.

### P2 — Per-recipe rejection feedback (Session 17 carry-over P3)

When a fetched recipe fails or produces empty / nonsensical records,
the user currently has two choices: re-author (which is gated by
ADR 0012's threshold) or accept the bad output. There's no path
to give targeted, recipe-scoped feedback that the next authoring
attempt can use.

The feature shape (still in design):

- A "report this recipe" affordance per recipe in the
  RecipesPanel. Probably a small text field + submit, scoped to
  one recipe.
- Storage: a new `recipe_feedback` table keyed by `recipe_id`,
  carrying the user's note + UTC timestamp + the run that
  triggered the feedback (FK to `fetch_runs`).
- Authoring loop: when the recipe author next runs for a plan,
  feedback rows for the same `source_id` are surfaced in the
  prompt as additional context — same channel as the existing
  `user_feedback` slot in the classifier prompt.
- ADR 0007 boundary check: this is the LLM as the only specialist.
  Feedback flows from the user to the LLM through a new prompt
  channel; the deterministic runtime gains nothing.

Open questions to resolve in Session 19 *before* writing code:
- Does feedback persist across `dedup_key`-bumped re-authoring,
  or get cleared with the recipe? (Likely: persist by
  `(plan_id, source_id)` rather than `recipe_id`, since recipes
  rotate and feedback is about the *source's behaviour for this
  plan*.)
- Is feedback visible in the failure-case workflow, or is it a
  separate channel? (Probably separate: failure cases are global
  prompt artifacts; feedback is plan-local.)
- Maximum feedback length — same `check_user_text` bound as the
  classifier topic? (Likely yes — same security primitive.)

ADR-track: this work justifies a new ADR, **0013 — recipe
feedback channel**, drafted before code lands.

### P3 — Endpoint_hint coverage sweep (Session 16 carry-over)

7 of 12 sources in `config/sources.toml` lack an `endpoint_hint`.
The fetch executor falls back to a placeholder URL when the hint
is absent (covered by
`author_one_falls_back_to_placeholder_when_no_endpoint_hint`
test), so this is degradation, not breakage. Per Session 16
notes: "reading source documentation, better as a user-driven
sweep with pairing than an LLM session."

Recommended Session 19 posture: leave for an explicit session that
focuses on the source registry, OR fill 1–2 hints opportunistically
if P1's verification picks an un-hinted source.

---

## Items deferred (gates and triggers)

### `pdf_table` extraction-mode removal

Session 17 handoff's gate condition: "defer until at least one
full session goes by where the LLM, given v1.7's strategy, never
authors a `pdf_table` recipe." Session 19 is exactly the
observation window. Tally `pdf_table` recipes authored during
Session 19's verification work; if the count is zero across all
runs, **Session 20 can cleanly delete the `pdf_table` variant**
(prompt + extraction enum + apply branch + tests). If the count
is non-zero, the variant stays; the cases that produced it become
prompt-improvement targets.

### Per-expectation SatisfactionPanel (Session 16 P4)

UI work — independent of the prompt and runtime layers. Show, per
expectation in a research plan, whether records covering that
expectation have been ingested yet (count + freshness). Useful
once verification runs accumulate a non-trivial record corpus.
Defer to a UI-focused session.

### Repo-root cleanup

Accumulated `SESSION*_README.md` files at repo root; old
`situation_room.duckdb.broken-2026-05-01*` DB files. Risk of
deleting something with non-obvious value. Defer to a deliberate
housekeeping pass with the Git remote question on the table
(currently no remote configured; local backups remain essential).

---

## Apply discipline reminders for Session 19

- Patches arrive as `.tar.gz`; apply via
  `tar -xzf ~/Downloads/<patch>.tar.gz --strip-components=1 -C .`
  from the repo root. Run each line independently if any has
  inline comments — keeps shell quote-state clean.
- `cargo clean` between major patch families if results look stale;
  incremental compilation can occasionally lag the reality on disk.
- ts-rs files (`apps/desktop/src/lib/api/types/*.ts`) are
  regenerated on first `cargo test --package situation_room-api`;
  do not hand-edit between patches unless explicitly part of a
  patch's hand-mirror delivery.
- xAI API key never echoed, logged, or printed — same hard rule.

---

## Open questions for the start of Session 19

1. **Which P1 verification first?** The CELEX re-run (a) is the
   shortest path to closing an open failure case; the BAKED
   path (b) is the highest-risk new behaviour; the HTML-
   equivalent path (c) is the cheapest sanity check. A reasonable
   order is **c → a → b** — sanity, then known-failing, then new
   feature — but Ervin's call.
2. **P2 ordering vs P1.** P1 is empirical and may surface prompt
   issues that change P2's design assumptions. Default: complete
   P1 before drafting ADR 0013.
3. **Has the parallel external Session 17 implementation been
   reconciled?** Session 18's transparency note flagged that
   `code/` had concurrent edits. If those were merged or
   discarded, no further action; if they're still drifting, a
   reconciliation pass should precede new feature work.
