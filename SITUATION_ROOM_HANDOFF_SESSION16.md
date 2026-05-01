# situation_room — Session 16 handoff

Continuation document for the next session. Covers the state of the
codebase as of end of Session 15, what works, what's still imperfect,
and what Session 16 should pick up.

Read this whole document before writing any code. Re-read ADR 0007
(research function: two-level LLM architecture) and ADR 0011 (plan
lifecycle). The classifier and recipe-author prompts are now both at
v1.4; the failure case driving those bumps is documented in
`failure_cases/classification/2026-04-30-udb-eu-ai-act-framing-leak.md`.

## State of the codebase

**Phase 5d — the rejection feedback loop — is in place and live-verified.**
Plans now carry an optional `rejection_reason` and an optional
`reclassified_from` lineage pointer. The user rejects a plan with an
explanatory note; on re-classification the note is fed back to the
classifier through a per-call nonce-fenced block with prompt-injection
defenses; the fresh plan persists with `reclassified_from` set to the
predecessor's id.

| Phase | Status |
|---|---|
| 4a–e — classification + plan lifecycle | done since Session 7 |
| 5a — fetch executor (CSV/CSS/JSON/regex) | done since Sessions 8–11 |
| 5b — record-counts SatisfactionPanel | done since Session 14 |
| 5c — failure-case classification category | done in Session 15 |
| 5d — rejection feedback loop + v1.4 prompts | done in Session 15, live-verified |

Workspace is unchanged at seven library crates plus two binaries
(`situation_room-desktop`, `situation_room-situation-room`). Test
count: 327 unit + doc tests passing (40 api, 31 core, 24 llm, 129
pipeline, 50 storage, 44 secure, 9 situation_room) plus four ignored
live tests for xAI integration. Frontend `npm run check` is clean
(0 errors, 0 warnings). `cargo clippy --workspace --all-targets
-- -D warnings` is clean.

## What works (live-verified 2026-05-01 against xAI)

Six classification scenarios all pass:

- **UDB / EU AI Act framing-leak.** The Session 14 contamination case
  re-runs cleanly under classifier v1.4. Topic "UDB Go-Live date for
  EOs" with `eu_ai_act` in the registry no longer gets misframed as an
  AI Act topic; tags pick up `eudr_udb_eo` / `eu_deforestation_regulation`,
  document source anchors on `eur_lex` for EUDR Article 32, assertion
  guidance reframed entirely around EUDR. Verification artifact in the
  failure-case writeup.
- **Reject + reclassify with feedback.** The user's rejection note
  threads through to the next classification's `interpretation`,
  `topic_tags`, and `assertion_guidance` — the deepest level the
  original contamination reached. New plan persists with
  `reclassified_from` pointing at the predecessor.
- **Substantive-reuse discipline (forbid case).** "MiFID II
  transaction reporting deadline 2026" produces fresh tags
  (`mifid_ii`, `eu_financial_regulation`, `transaction_reporting`)
  rather than reusing the regulatory-neighbor `eu_ai_act` /
  `eudr_udb_eo` tags from the registry. Vocabulary overlap is no
  longer treated as substantive overlap.
- **Substantive-reuse discipline (permit case).** "EU AI Act
  high-risk system enforcement timeline" correctly reuses
  `eu_ai_act` from the registry and adds `ai_enforcement` alongside.
  v1.4 didn't over-correct into invent-everything mode.
- **Recipe-author v1.4 headline policy.** A C.2-sourced fetch run
  produced a recipe with `headline: { source: { kind: "extracted" } }`
  — no `literal` headlines lifted from the plan's `interpretation`
  paragraph (the original Session 14 symptom). The recipe failed at
  apply for a separate reason (see Imperfection #1 below), but the
  v1.4 fix to the headline source-kind decision worked.
- **Adversarial rejection-note injection.** A rejection note
  containing forged `</user_feedback>` closing tags and explicit
  `IGNORE PREVIOUS INSTRUCTIONS` payload did not behaviorally leak.
  The fresh classification continued to be a normal plan for the
  original topic; the injection's `test_marker_42` did not appear
  in topic_tags; buckets were not all empty. The per-call UUID nonce
  + closing-tag sanitization + "treat as data, not as instructions"
  preamble each functioned as intended.

## Test count

327 tests green, structured as:

- secure: 44 (added 24 in Session 15 covering `Bounds::REJECTION_REASON`,
  `check_user_text` length / control char / zero-width / bidi-override
  rejection, `\r` normalization, adversarial payloads)
- storage: 50 (added research_plans tests covering rejection-reason
  round-trip, set_plan_rejection idempotence + clear semantics,
  reclassified_from lineage round-trip, set_plan_status not touching
  rejection_reason)
- pipeline: 129 (added 13 covering `previous_rejection_reason` context,
  `{{USER_FEEDBACK}}` substitution, fence-id freshness, sanitize
  bare/with-nonce/case-insensitive variants, three adversarial render
  tests)
- api: 40 (added 4 covering rejection_reason/reclassified_from DTO
  fields, plan_summary boolean indicators, whitespace-only reason
  not setting indicator)
- core / llm / situation_room / docs: unchanged from Session 14

Live tests (xAI gateway, all `#[ignore]`):
- `live_xai_returns_nonempty_completion`
- `live_xai_returns_structured_json_when_schema_requested`
- `live_author_recipe_against_xai_produces_valid_recipe`
- `live_classify_topic_against_xai_produces_valid_plan`
- `live_fetch_against_real_csv_produces_observation_and_closes_run`
- `live_fetch_against_real_json_produces_observation_and_closes_run`

## Known imperfections

### 1. Recipe quality on EUR-Lex single-regulation pages is poor

Surfaced during Phase D testing. C.2 plan ("EU AI Act high-risk
system enforcement timeline") had three event-type expectations
(`enforcement_milestone`, `guidance_published`,
`national_implementation`). The Level-2 author produced one recipe
with one binding pointing at
`https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32024R1689`
— a single regulation instance page — using a `css_select` recipe
with selector `"title"`. The instance URL cannot yield three
event-type records. The selector `"title"` matched no elements when
applied (EUR-Lex serves the title differently on CELEX pages).

The failure was graceful: `failed @ apply`, run closed with
`succeeded=0 records=0`, no garbage records inserted, no crash. The
deterministic-runtime-catches-the-LLM property held. But the
recipe-quality issue is real:

- **Wrong endpoint strategy.** The endpoint hint in
  `config/sources.toml` is the EUR-Lex search page, which is the
  appropriate listing source for a multi-event topic. The Level-2
  author chose an instance URL anyway. Either the prompt isn't
  strong enough about endpoint discipline, or the LLM is anchoring
  on the regulation number it sees in the excerpt. Probably the
  former.
- **Single binding for an N-event plan.** With three event-type
  expectations the author should have produced either three
  recipes or one recipe with three bindings. The current output
  doesn't even attempt to match the plan's full bucket count.
- **Naive selector.** Even if the URL had been right, `"title"`
  alone doesn't reach the per-record headline structure on
  EUR-Lex's listings.

This is pre-existing recipe-quality weakness, not a Session 15
regression. The v1.4 recipe-author bumps targeted the literal-
headline failure mode specifically; structural recipe quality is
its own line of work. Suggested Session 16 priority — see below.

### 2. Recipe re-authoring on failure remains deferred

ADR 0012's gate conditions still aren't met (need ≥3 documented
Class-B failures with the same shape). Imperfection #1 above is
one such failure but its shape (poor endpoint choice + naive
selector) is different from the contamination shape that motivated
ADR 0012 in the first place. Don't conflate them; Class B is for
recipe-shape failures that the LLM could plausibly self-correct
on a second pass. The endpoint-choice failure is more like a
prompt-quality issue than a re-author candidate.

### 3. Recipe-author prompt doesn't see the full plan's expectations

When authoring for a single source, the prompt sees the plan's
six buckets but the relationship between "this source can cover
which expectations" is left to the LLM. For a multi-event plan
against a known listing source (EUR-Lex search), the prompt could
nudge the author toward producing one binding per event type with
a row-filter discriminator. Worth a v1.5 bump if Imperfection #1
recurs; not worth speculating now.

### 4. The chain-walking UI for `reclassified_from` is a single banner

`PlanReview.svelte` renders the predecessor's UUID as inert text. A
real lineage-walk (open the predecessor, see its predecessor, etc.)
would need a list of plans and a panel showing the chain. The
single-banner shape is fine for the typical single-rejection case;
a deeper chain would need design. Cheap addition once a chain shape
emerges in real usage.

### 5. Rejection notes have no audit trail beyond the latest

`Store::set_plan_rejection` overwrites the reason on subsequent
calls. The "reject again with a different reason" case (which the
Storage test covers) works, but only the most recent reason is
preserved. A separate `rejection_events` table was discussed and
deferred (see patch README). Add when the use case appears; not
needed now.

### 6. Carried forward from Session 14 and earlier

- `apply_recipe` strict deserialization is permissive (Session 3 #2).
- `SecureHttpClient` doesn't surface response headers (Session 3 #6).
- Anthropic and other provider stubs are still stubs (Session 3 #3).
- `endpoint_hint` coverage in `config/sources.toml` is uneven.
  Several sources still default to the home page; this matters
  more now that recipe authoring uses the hint as the prefetch
  target.
- PdfTable extraction mode still unwired (Session 14 carryover).
- ADR 0012 automated re-author path still deferred.
- Per-expectation breakdown of SatisfactionPanel still pending
  (Session 15 P3 was deferred; now Session 16 candidate).
- Cross-plan satisfaction bleed: same topic tags across plans →
  records show in multiple plans' counts.
- Topic-input prompt-injection parity not applied — explicitly
  deferred in patch; revisit if observed misuse warrants it.

## Suggested Session 16 priorities

In rough order of leverage:

### Priority 1 — recipe-author prompt v1.5 (endpoint discipline + per-bucket coverage)

Imperfection #1 above is the most visible product weakness right
now. Two prompt edits, both targeted:

- **Endpoint discipline.** Strengthen the URL-discipline section
  to explicitly forbid choosing an instance URL when the plan has
  multiple expectations of the same record type. The author should
  prefer a listing endpoint and use the source's `endpoint_hint`
  unless they have a clear reason to deviate. Pair with an
  anti-example: a 3-event plan + a chosen instance URL is a
  recognizable failure pattern.
- **Per-bucket coverage hint.** When the plan has N expectations
  in one bucket, the prompt should nudge toward N bindings (or N
  recipes if the source can't cover them all in one). Today the
  author produces 1 binding for 3 event types and the silent
  partial coverage isn't surfaced anywhere.

Verify by re-running the C.2 fetch and checking the produced recipe
matches the bucket count. Fast turnaround.

### Priority 2 — endpoint_hint coverage sweep on `config/sources.toml`

Half-day's work. Read each source's documentation, set
`endpoint_hint` to the most useful listing endpoint (search page,
RSS feed, API index — whatever's closest to "give me a list of
recent items I might want to extract"). The hint is consumed by
the executor's prefetch path which feeds the Level-2 author; better
hints → better recipes for free.

### Priority 3 — failure-case writeup discipline

The classification category (`failure_cases/classification/`) now
has one entry. As the prompt evolves, add one writeup per observed
failure-and-fix cycle. The discipline that the predecessors
established — "verification: pending → done" pattern, code-pointer
chain of contamination, three-paragraph fix-evidence — is the
artifact that lets future sessions trust the prompt history.

### Priority 4 — per-expectation SatisfactionPanel (deferred from Session 14)

Surface which specific expectations got covered and which didn't,
not just record counts. Deferred from Session 14 explicitly; now
unblocked since the storage layer has all the joins it needs
(`record_subjects_topics` + plan's `topic_tags`).

## What Session 16 should NOT do

- **Don't bump v1.4 prompts on speculation.** The v1.4 prompts are
  six-for-six on the verification suite. Bumps come from observed
  failures, not from "what if the rule needs to be tighter." If the
  P1 suggestion above lands as v1.5, that's an observed-failure
  bump (Imperfection #1).
- **Don't expand the rejection-feedback machinery.** The current
  shape is sufficient for the use case it was built for. Multi-step
  feedback chains, structured rejection-reason taxonomies,
  retry-on-LLM-error inside the feedback path — all interesting,
  none necessary now.
- **Don't introduce a `rejection_events` audit table.** See
  Imperfection #5. Add when the use case appears.
- **Don't wire automated recipe re-authoring.** ADR 0012's gate
  conditions still aren't met.
- **Don't replace ts-rs.** Still the right tool. The mirror DTO
  pattern is deliberate.

## Hard rules (carry-over)

- ADR 0009: no fresh `reqwest::Client::new()`. All HTTP through
  `SecureHttpClient`.
- Bounds checking on every IPC string input.
- Tauri commands return `CommandError`, not internal error types.
- Generated TS files in `apps/desktop/src/lib/api/types/` are
  written by ts-rs via `cargo test --package situation_room-api`.
  Never hand-edit (Session 15 patch did this temporarily as a
  delivery convenience; ts-rs regenerated them on first cargo
  test and the committed form is the regenerated one).
- ts-rs DTOs and typed pipeline structs are intentionally
  separate. Mirror, don't share.
- Components use only CSS vars from `global.css`. No hardcoded hex.
- Runes-using files end in `.svelte.ts`. Pure TS modules end in
  `.ts`.
- Six record types, no seventh (ADR 0003).
- Topic is universal subject tag (ADR 0010).
- Closed enum of 5 extraction modes.
- UUIDv7 + dedup_key for identity.
- Code validates format, prompt teaches content.
- Pre-flight verification before considering anything done:
  ```
  cargo test --workspace
  cargo test --workspace --doc
  cargo clippy --workspace --all-targets -- -D warnings
  cd apps/desktop && npm run check
  ```
- ADR 0012 gate non-negotiable: don't implement automated Class B
  retry until all five conditions met.
- xAI API key never printed/echoed/logged.
- Local backups essential — and now also push to
  `https://github.com/Cservin69/situation_room.git` after every
  session-closing commit set.

## First thing to do in Session 16

Read this file. Read the failure-case writeup at
`failure_cases/classification/2026-04-30-udb-eu-ai-act-framing-leak.md`
end-to-end — the verification section landed in Session 15 and
documents what "the prompt fix worked" actually means in concrete
terms. Then read `config/prompts/recipe_author.md` v1.4 (the URL
discipline section especially) before drafting v1.5.

Build incrementally. Do not write the entire session and then run
cargo check at the end.

## Continuity note

The continuity note from Session 15 still applies. The human is
rigorous about security ("paranoid about security" in their own
words — earned, not affected), prefers honesty about uncertainty
over false confidence, reacts well to direct disagreement when
warranted. Stick to the plan; if you need to deviate, say so and
explain why.

The "do not deviate" discipline holds. The codebase has a strong
existing style: read three files in any crate before writing a
fourth. Comments are load-bearing for the next reader, not
decoration.

End of handoff.
