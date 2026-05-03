# STOCKPILE — Session 22 handoff

You are starting Session 22. Session 21 shipped P3 from the Session
21 handoff (the architectural provenance gap surfaced by the
Session 20 live run, addressed via ADR 0014) plus P4 (the Svelte 5
runes warning in `RecipeFlagDialog`). P1 and P2 from the Session 21
handoff still carry forward — same blockers, same shape.

Read this whole document before writing any code. ADR 0014 (new) is
load-bearing for any work touching authoring provenance; ADR 0007
(provenance thread + runtime-is-LLM-free invariant) and ADR 0013
(recipe-feedback channel) are still the rulebook for adjacent work.

## What Session 21 shipped

ADR 0014 — stub-authored recipe provenance, full design + end-to-end
implementation.

| Layer | What landed |
|---|---|
| ADR | `docs/adr/0014-stub-authored-recipe-provenance.md` resolves option 1 + 2 (visible chip + dialog hint); option 3 (silent self-healing on first real-bytes fetch) is **deferred** with explicit amendment-trigger conditions in §"When to amend or supersede". |
| Migration | v10 — `migrations/0010_recipes_authored_from.sql`. Additive nullable `authored_from TEXT` column. Same DuckDB-ALTER discipline as 0008. |
| Storage | `AuthoredFrom` enum (`FetchedBytes` / `StubExcerpt` / `Unknown`) with `Default == Unknown`, `Display`/`FromStr`, threaded through `RecipeRow`/`StoredRecipe`/all SQL. NULL → Unknown coercion in `row_to_stored`. Re-exported from `situation_room_storage`. |
| Pipeline | `FetchRecipe.authored_from` field with `#[serde(default)]`. Threaded through `recipes_store::recipe_to_row` / `stored_to_recipe`. Validator (`build_validated_recipe`) defaults to Unknown; **executor (`author_one`) stamps the real value** based on whether `prefetch_excerpt` returned Some. New `info!` log line `"recipe authored; provenance stamped"` makes the signal observable in tracing output without opening the UI. |
| API | New `AuthoredFromDto` enum with `From<AuthoredFrom>` impl. `RecipeDto.authored_from` field. |
| TypeScript | `apps/desktop/src/lib/api/types/AuthoredFromDto.ts` (new). `RecipeDto.ts` updated to import + carry the field. Hand-mirrored to match ts-rs's existing output; regenerated cleanly by `cargo test -p situation_room-api`. |
| Frontend | `STUB-AUTHORED` chip in `RecipesPanel.svelte` recipe head, sized + colored consistently with `BAKED` and `FLAGGED` (same `--signal-warning` hue as BAKED — both mean "deserves attention, non-default runtime path"). Hint banner in `RecipeFlagDialog.svelte` above the textarea, visible iff the flagged recipe was stub-authored. New `authoredFrom?: AuthoredFromDto` prop on the dialog. |
| P4 fix | `RecipeFlagDialog.svelte` — `wasNonEmpty = initial.trim().length > 0` → `$derived(...)`. Comment block names the trap so the next reader finds the rationale. |

### Test count posture

Expected delta over Session 20's 366 baseline:

- storage: +5 (variant strings, default, FromStr rejects unknown, two
  round-trip variants, NULL → Unknown via raw SQL UPDATE,
  recipes_for_plan carries through)
- pipeline `recipes.rs`: +2 (each variant round-trips, snake_case
  wire form). The legacy-recipe-deserializes test was strengthened
  (added an `Unknown` assertion) — same count.
- pipeline `recipes_store.rs`: +1 (StubExcerpt round-trip through
  marshalling).
- pipeline `fetch_executor.rs`: +3 (FetchedBytes happy path,
  StubExcerpt on prefetch failure, StubExcerpt on missing
  descriptor).
- api: +3 (wire-form parity with storage, GDELT-style stub case,
  legacy unknown case).

Total **+14**, landing at **380** green. Doc-tests still 0 active.
Frontend `svelte-check` should be clean (the runes warning P4
addressed is gone).

### Security posture

No new HTTP path. No new LLM call. No new IPC string field
(`authored_from` is an enum, not free text — no `check_user_text`
gate needed). The recipe-author prompt is unchanged at v1.8 — the
provenance signal lives in code, not in the LLM's view. ADR 0009
§"The rule" satisfied.

## What was *not* shipped, intentionally

### P1 — live xAI verification of v1.8 (carry-forward)

Three slots from the Session 20 handoff, owed by the operator's
network-enabled machine:

1. **HTML-equivalent path** — confirm v1.8 picks the HTML route when
   both PDF and HTML are addressable. Candidate: USGS MCS.
2. **CELEX re-run** — closes the deferred-by-design EUR-Lex case.
   See P2 below for the failure-cases blocker that affects how this
   one is run.
3. **BAKED PDF source** — confirm `static_payload` end-to-end on a
   PDF-only source.

**Newly relevant to P1 this session:** with ADR 0014 landed, every
new authoring run stamps `authored_from` accurately. The chip will
appear on any recipe authored against a stub. This makes P1
incidentally a verification of ADR 0014 too: the FetchedBytes case
should produce no chip, the StubExcerpt case (e.g. a source whose
hint URL 429s during the run) should produce one.

The cost-saving variant from the Session 21 handoff (re-run fetch
on the south-Korea-election plan `019de792-…`) still applies: zero
LLM tokens because recipes are pre-authored. Important caveat: pre-
v10 recipes load as `Unknown` (no chip). Re-running fetch on an
existing plan does *not* re-author its recipes — it only re-applies
them. To get a chip on the existing GDELT recipe, the path is (a)
flag it via the dialog, (b) run fetch again with GDELT now reachable
— which triggers re-author on the flag, producing a recipe authored
from real bytes, stamped FetchedBytes, with no chip. That's the
system working as designed.

### P2 — EUR-Lex CELEX failure-case banner (still blocked)

Same blocker as Sessions 20 and 21: the `apps/desktop/failure_cases/`
tree is **absent from the archive** and is **not in `.gitignore`**
either. The failure-case file simply isn't committed. The Session 21
session declined to make a unilateral decision; the operator's
disposition is owed.

Two concrete next-step options were named in the Session 21 README:

1. **Commit the failure_cases tree.** Future sessions inherit the
   context. Session 21 handoff said "probably yes." Mechanics: a
   `git add` pass on the directory plus a `.gitignore` audit to
   confirm nothing sensitive leaks (no credentials, no run
   artifacts).
2. **Keep failure_cases local, document the convention.** Short
   `CONTRIBUTING.md` paragraph naming the directory as an
   operator-local research-artifact directory; remove the
   references to it from session handoffs so it stops frustrating
   sandbox-only sessions.

Either disposition is fine. Until one is chosen, P2's banner work
sits idle.

## Known imperfections (carry-forward + new)

These are conscious leftovers, not surprises. Each is small enough
to fit in a focused session if it earns the slot.

### 1. Pre-ADR-0014 recipes load as `Unknown` (legacy data)

Migration v10 added the column nullable; existing rows read NULL,
the load path coerces NULL → `AuthoredFrom::Unknown`, and the chip
hides for Unknown. That's the intentional posture (no UI noise on
every existing recipe the moment migration runs). But it means the
GDELT recipe from the Session 20 live run — the *motivating case*
for ADR 0014 — currently shows no chip until it's re-authored.

The chip will appear correctly on every *new* authoring run. If the
operator wants to back-fill the legacy rows with a defensible value,
that needs a follow-up session: walk every existing row, infer the
authoring branch from the recipe's source's current `endpoint_hint`
status (a heuristic that may be wrong for any source whose
configuration has changed since), and `UPDATE` the column. ADR 0014
explicitly does **not** commit to this — back-filling is a
retroactive truth claim and warrants its own decision.

### 2. Option 3 (silent self-healing) deferred

Documented exhaustively in ADR 0014 §"What the user does NOT see".
The amendment triggers are named there; the next session weighing
self-healing should answer the four enumerated questions before any
code lands.

### 3. The chip cluster's layout headroom

Three chips (BAKED, STUB-AUTHORED, FLAGGED) is currently the
maximum that fits glanceably in the recipe-card head. A fourth chip
would warrant rethinking the layout — possibly a chip strip below
the head, possibly grouping chips into a single cluster with
overflow. No fourth chip is on the roadmap, so this is a future
problem; flagging it now so the next contributor adding a head-level
signal stops to think rather than just `{#if}`'ing it in.

### 4. Carry-forward from Session 20

- `apps/desktop/failure_cases/` tree absent (see P2 above).
- The `sanitize_for_fence` helper is duplicated across `research_classifier.rs`
  and `recipe_author.rs`. Session 20 fixed both copies in parallel; the
  followup to extract a shared helper in `pipeline::common` is still owed.
  Defer-to-when-it-matters territory.

### 5. Carry-forward from Session 19 / earlier

Same as Session 20's list:

- Anthropic provider and others still stubs (the Session 17
  refactor left `crates/llm/src/providers/{anthropic,openai,gemini}.rs`
  as ~200-byte stubs).
- Apply-runtime strict deserialization permissive.
- PdfTable extractor unimplemented.
- Authoring latency 30–60s (xAI gateway, not us).
- `SecureHttpClient` doesn't surface response headers.
- Crate-level `#![allow(...)]` lint suppressions still need a sweep
  outside the api crate.

## Suggested Session 22 priorities

In rough order of leverage:

### Priority 1 — Run P1 against ADR 0014

This is field-work, not code. Pick one or two of P1's three slots,
run them on the network-enabled machine, capture the recipes the LLM
authors, confirm the chips appear (or don't) accurately. Specifically
expect:

- **HTML-equivalent USGS MCS / SEC EDGAR** — should produce
  `FetchedBytes` recipes, no chip. (Both have stable HTML endpoints.)
- **CELEX re-run** — likely `FetchedBytes` if the hint URL is
  reachable, but the deferred-by-design selector failure may still
  produce useful output for a feedback note. The chip's absence
  here means "the LLM saw the page; the bug is in what it picked,
  not what it had to look at" — useful diagnostic.
- **A source whose hint URL is intermittently rate-limited** (e.g.
  GDELT) — should produce `StubExcerpt` recipes, chip appears. This
  is the case ADR 0014 was written for; verifying it lights up
  correctly closes the architectural loop end-to-end.

### Priority 2 — Decide P2's failure_cases disposition

The operator has two choices, named in Session 21's README and again
in §"P2" above. Either choice unblocks the EUR-Lex banner work. The
decision is small; it just needs to be made.

### Priority 3 — Optional: chip on the FetchReport panel

ADR 0014 §"Why not a `RecipeOutcome` field" deliberately kept the
chip off the FetchReport panel — the recipes panel is the canonical
surface, and duplicating the signal in two surfaces risks drift. But
the FetchReport's chip strip already shows other per-recipe metadata
(records produced, failed @ stage). A *passive* indicator there
might be worth the duplication if operators tend to look at the
report before opening the recipes panel.

This is a UX call, not an architectural one. Run P1 first; if the
chip on the recipes panel proves sufficient (operators see it before
needing to flag), don't add a second surface. If P1 reveals operators
miss the chip until they're already typing in the dialog, a small
parallel chip on the FetchReport's outcome row would help.

### Priority 4 — Anthropic provider stub → real

Carried forward from every session since 17. Lets the operator A/B
classification quality on real plans. Same scaffolding pattern as
the xAI provider.

## What Session 22 is explicitly NOT

- **Not implementing option 3 (silent self-healing).** ADR 0014's
  amendment triggers are the gate. Don't pre-empt.
- **Not back-filling pre-v10 `authored_from` rows.** Same reasoning:
  retroactive truth claim. The visible-chip surface is the user's
  hook for triggering the manual path themselves.
- **Not adding a fourth chip to the recipe head.** Three is the
  visual ceiling; a fourth needs layout rework first.
- **Not bumping the recipe-author prompt past v1.8.** ADR 0014's
  signal lives in code, not in the LLM's view. The prompt is unaffected.
- **Not consolidating `sanitize_for_fence`.** Session 20's followup;
  not blocking.

## Hard rules (carry-over, unchanged from Sessions 5–21)

- ADR 0009 §"The rule": no fresh `reqwest::Client::new()`. All HTTP
  through `SecureHttpClient`.
- Bounds checking on every IPC string input. `check_user_text` is
  the gate for user-supplied text. (`authored_from` is an enum, not
  free text — no gate needed.)
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` written by
  ts-rs via `cargo test -p situation_room-api`. Never hand-edit
  except as a temporary measure shipped alongside the matching Rust
  change (which Session 21 did, and the operator regenerates).
- ts-rs DTOs and pipeline structs are mirrored, not shared. Pipeline
  crate does not depend on ts-rs. (Session 21 added an
  `AuthoredFromDto` mirror of storage's `AuthoredFrom` for the same
  reason.)
- Components only use CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`, not `.ts`.
- Migrations: read the prior migration's comment block before
  writing the next. The DuckDB `ALTER TABLE` constraint trap is
  real.
- xAI API key never read, written, or referenced anywhere visible.
  `ApiKey::from_env` only.

Standing-order priority: **security > generalisation > simplicity**.

## First thing to do in Session 22

1. Read this file.
2. Read ADR 0014 if you haven't (Session 21's deliverable; load-
   bearing for any work touching authoring provenance).
3. Decide P1-vs-P2-vs-something-else with the operator. P1 is
   field-work the operator's machine can do; P2 is a small
   posture decision the operator owes; P3+ are speculative until
   P1 produces fresh evidence.

If P1 surfaces material that justifies a v1.9 prompt bump or an
ADR 0014 amendment, those are deliberate next-session shapes —
don't do them in the same patch as the verification run.

## Continuity note

The continuity note from Sessions 19/20/21 still applies. The
operator is rigorous about security ("paranoid about security" —
earned, not affected), prefers honesty about uncertainty over false
confidence, reacts well to direct disagreement when warranted, and
has explicitly asked for "do not deviate" discipline.

Session 21's deviation was structurally invited by the operator
("do a large scope as session 20 was shy"). That phrase is unusual
and important: the standing posture is "stick to the plan" — Session
21 had explicit operator permission to take a bigger swing, and used
it on the architectural P3 work rather than splitting attention
across all four priorities. The standing posture for Session 22 is
unchanged from the default: stick to the plan; deviation requires a
real defect against a published ADR's invariant, or an explicit
operator instruction that broadens scope.

The codebase has a strong existing style. Read three files in any
crate before writing a fourth. The hardest part of contributing
well here is matching the existing voice in the code comments and
the ADR cross-references — the comments aren't decoration, they're
load-bearing for the next reader.

End of handoff.
