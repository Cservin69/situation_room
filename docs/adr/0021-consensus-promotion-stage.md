# ADR 0021 — Consensus promotion stage (Session 81)

**Status**: Accepted
**Date**: 2026-05-16
**Related**: ADR 0003 (six record types), ADR 0004 (assertion
promotion model), ADR 0007 (research function), ADR 0017
(closed-vocabulary discipline)

## Context

ADR 0004 set the assertion-promotion model: claims persist as
`Assertion` rows; either an authoritative-source registry fast-tracks
them, or a consensus pathway promotes a claim once N independent
claimants agree on compatible content. The pipeline crate's
`promote.rs` was stubbed at the time and stayed stubbed across
Sessions 1 → 80.

Session 81 lights up cross-source dedup as a product surface because
the operator-visible problem ("the same fact, surfaced from five
sources, lands as five Assertion rows") is now legible on the
dashboard. Sessions 77 / 78 / 79 / 80 wired per-Document extraction
for the four content shapes (relation / event / observation / entity
attribute), so a multi-source plan now produces the expected
duplicate-claims pile. The consensus stage is the right answer for
turning that pile into the *single* fact the dashboard's typed panels
want to surface, while preserving the per-source claim layer ADR
0004's anomaly-detection queries depend on.

This ADR resolves the implementation choices ADR 0004 left
deliberately open: the hashing function, the idempotency mechanism,
the trigger model, and which pathway lands first.

## Decision

Session 81 ships the **consensus pathway only**. The authoritative
pathway is structurally clear from ADR 0004 — config-driven
`(claimant, content-type, subject)` lookup, instant promote — and
unblocks once `config/authoritative.toml` exists (Phase 3 work). It
is deliberately deferred so this session can validate the consensus
path on its own.

The consensus stage is implemented in
`crates/pipeline/src/promote.rs` with these concrete choices:

### Default quorum N=3 independent claimants

Matches ADR 0004's default. "Independent" means distinct
`Assertion::claimant` `EntityId`s — five rows all claimed by
`agency:reuters` count as one. Per-call configurable via
`PromoteConfig::min_independent_claimants` so an operator running a
"preview consensus" sweep can lower it to 2 for exploration; per-
metric tuning ADR 0004 names belongs to a future session.

### Idempotency via content-derived dedup keys

Each promoted record carries
`dedup_key = "promotion:{content_hash}:{subject_hash}"`. ADR 0004
names content-derived keys as one of the few places where they're
appropriate — the question "has this been promoted" is genuinely
about content identity, not source identity. UUIDv7 stays the
record's primary key; the dedup_key is metadata on the pipeline
stage.

On re-run, DuckDB's UNIQUE constraint on the dedup_key column
rejects the second insert. The promote stage classifies the
resulting error against `"duplicate" / "unique" / "dedup_key"` and
counts the row into `PromoteReport::skipped_already_promoted` rather
than `insert_failures`. The stage is safe to run many times against
a growing assertion store.

### 128-bit hash via two `DefaultHasher` runs

The workspace does not currently take a hashing-crate dependency.
SHA-256 would be the natural choice; adding `sha2` for a
content-identity key whose adversarial-collision surface is "an LLM
emits 2^64 attempts to forge a dedup_key on the dashboard's behalf"
is overkill. The stage uses two `std::collections::hash_map::DefaultHasher`
runs (one with a salt) concatenated as a 32-char lowercase hex
string. Collision floor on the order of 2^64 — fine for a within-
session dedup table that won't grow past low millions of rows.

If a future stage with adversarial inputs (e.g. dedup keys that
cross trust boundaries) lands, the right move is to add `sha2` to
the workspace and swap `hex128` for a SHA-256 wrapper. The
`content_hash_for` / `subject_hash_for` signatures stay stable
across the swap.

### Canonical-JSON pre-hash normalisation

`serde_json::to_value(content)` produces a deterministic shape but
`serde_json::to_string` does NOT sort object keys. The promote stage
applies a recursive normalisation (`canonicalize_json` in
`promote.rs`) that sorts object keys before serialising to bytes, so
the same content produces the same hash regardless of serialiser
implementation detail.

`subject_hash` sorts topics and entity strings before hashing for the
same reason: `Subjects::topics` order is incidental.

### Operator-triggered today; auto-trigger deferred

A Tauri command `promote_consensus_for_plan(plan_id,
min_independent_claimants?)` runs the stage on demand. The frontend
operator surface (a button under the Records panel) is intentionally
not wired this session — Session 81 lands the stage and the IPC
boundary; a future session adds the surface once the operator
decides whether consensus should run on every fetch-run completion
or stay manual.

The trade-off: running it on every fetch completion would be
correct ("the dashboard never shows un-consensed duplicates") but
risks burning the operator's surprise budget the first time a
classification leaks across plans (cross-plan dedup is structurally
possible — same content + same topic on two different plans). The
manual button stays the safer first cut.

### Confidence is averaged across supports

`Confidence` on the promoted record is the arithmetic mean of the
supporting Assertions' envelope confidences, clamped to `[0.0, 1.0]`.
Matches the operator-readable "the more sources agree the more we
trust it" intuition without amplifying a single zealous claimant.

ADR 0004 left the consensus-confidence computation open. Other
sensible shapes — Bayesian update from a prior, median, max — are
not wrong; averaging is the most legible to operators and easiest
to compute deterministically.

### Provenance chain via `DerivedFrom { role: ConsensusSupport }`

Every supporting Assertion's id surfaces in the promoted record's
`Envelope::provenance.derived_from` with
`role: DerivationRole::ConsensusSupport`. ADR 0004 names this as the
auditability shape. The promoted record's `source_id` is the synthetic
string `"derived#consensus"`; `license` is `"derived"`. The plan's
topic tags propagate onto the promoted envelope so the
`records_for_plan` LIKE join surfaces the row under the originating
plan.

A `"consensus_promotion"` tag is added to `Envelope::tags` so the
dashboard can distinguish promoted records from directly-fetched
ones if a future presentational session wants to render them with a
distinct affordance.

### EntityAttribute promotion synthesises a consensus Assertion

ADR 0004 names EntityAttribute promotion as "updates the target
Entity's attributes." The Entity record itself doesn't carry an
attributes column today — attribute facts live on Assertion rows
with `AssertedContent::EntityAttribute` content. Session 81
implements promotion as "synthesise a consensus-stamped Assertion
with claimant=`agency:consensus`, stance=`Asserted`." The
synthesised row carries the same content + dedup_key + derived_from
chain the other three promotion paths use, so the dashboard's
Entity-pane attribute tiles (Session 81 candidate 2) automatically
surface the consensus value over the per-source values.

The Entity-attribute-column path is a future-session product call —
when the operator decides whether the Entity record should carry an
attributes column at all (currently the schema makes attributes a
stream on Assertions, by ADR 0003's reasoning that "an entity's
state is the aggregation of its attribute records up to T").

## Rationale

### Why consensus before authoritative

Two reasons. **Product visibility:** the operator-visible problem is
the duplicate-rows pile, which consensus closes; authoritative
fast-tracking is invisible until you compare time-to-promote on a
USGS feed vs a no-config baseline, and we don't have that delta to
measure yet. **Config dependency:** authoritative needs
`config/authoritative.toml` populated for any real source — that's
the next Phase 3 step, not a Session 81 shipment.

### Why operator-triggered

The first-run unknowns ADR 0004 enumerated (cross-plan dedup leaks,
consensus-on-stale-data, the right cadence) are easier to evaluate
with a manual trigger and a `PromoteReport` summary the operator
reads before deciding. Wiring on the fetch-run-completion hook is
cheap to add later; un-wiring an autorun that produced 200 surprise
records is more expensive than waiting for the manual run.

### Why DefaultHasher instead of adding sha2

Three reasons. **No new dep:** the workspace stays minimal and the
operator's "is this an honest stage?" judgement isn't muddied by a
new crate in the tree. **Adequate collision floor:** 2^64 is more
than enough for the within-session dedup table — record volumes are
in the thousands per plan, low millions per long-running install.
**Swap path is clear:** the hashing logic is encapsulated in
`hex128`; swapping to SHA-256 is a one-function change if real
adversarial pressure shows up.

### Why averaged confidence

Operators read the cell as "five sources agreed, my confidence rose";
average matches the intuition. Bayesian update needs a prior we don't
have; median washes out the signal at small N; max amplifies a single
high-confidence claimant. Average is the median voter's choice for
the small-N regime the consensus stage actually operates in.

### Why preserve Assertions through promotion

This is ADR 0004's commitment; this ADR reaffirms it. The promoted
record's `derived_from` chain points back to the supporting Assertions,
and the originals stay queryable. The anomaly-detection queries ADR
0004 named ("which claimants most often agreed with the eventual
promoted value?") depend on the originals remaining available.

## Alternatives considered

**Ship authoritative first.** Rejected because the config registry
isn't populated. Lands once Phase 3 names the first authoritative
sources.

**Skip dedup_key idempotency; rely on a transactional "delete then
insert" step.** Rejected because (a) the supporting Assertions
must stay alive — promotion is additive, not replacing — so a
delete-then-insert dance would have to skip them anyway, and (b) the
content-derived dedup_key is the auditable answer to "has this been
promoted." Transactional deletion is the alternative for promotion
*rollback*, which isn't a Session 81 concern.

**Add sha2 to the workspace.** Defensible; rejected on the
no-new-dep grounds above. The DefaultHasher swap path is a one-
function change if adversarial pressure or growth justifies it.

**Run on every fetch-run completion.** Rejected for Session 81; the
manual trigger is the safer first cut. Lands when the operator
decides the cadence question.

**Use median confidence.** Defensible; rejected because average is
more legible and the small-N regime (N=3 typical) makes the
distinction operationally invisible.

**Authoritative promotion via in-code list (skip the TOML).**
Rejected because ADR 0004 explicitly names the registry as
configuration. Bypassing it would re-litigate that decision.

**Promote EntityAttribute via an Entity-record column update.**
Rejected because the schema doesn't have one. Adding one is an ADR-
0003 schema rev (six record types include `EntityAttributeContent`
already, but `Entity` itself doesn't have an attributes column); the
consensus stage doesn't have to push that decision. Synthesising a
consensus-stamped Assertion preserves the stream model the schema
already commits to.

## Consequences

**Positive**

- Operator can run consensus promotion on demand. The dashboard's
  typed panels (Observations / Events / Relations) gain promoted
  records distinguishable from per-source claims via the
  `derived#consensus` source_id and the `consensus_promotion` tag.
- The duplicate-rows pile from cross-source extraction (Sessions 77
  / 78 / 79 / 80) becomes operator-actionable rather than just
  noise.
- Idempotent on re-run; no manual cleanup required between
  invocations.
- ADR 0004's "preservation" commitment holds — every Assertion the
  consensus pass touched stays in storage with its claim structure
  intact, queryable via the supporting `derived_from` chain on the
  promoted record.

**Negative**

- The consensus stage doesn't yet auto-run. Operators have to know
  to invoke `promote_consensus_for_plan` after a fetch run before
  the typed panels show consensus rows. UX-shaped problem; a future
  session adds the auto-trigger or surfaces a button on the records
  panel.
- The DefaultHasher choice has weaker collision properties than
  SHA-256. The 2^64 floor is fine for current scales; growth toward
  multi-million-row dedup tables, or any cross-trust-boundary use,
  warrants the swap.
- EntityAttribute promotion still goes through an Assertion-shape
  proxy rather than a typed Entity-attribute column. The dashboard
  treats the consensus row identically to the per-source rows in
  the Assertions panel today; the visual distinction comes only
  from the `consensus_promotion` tag.
- No retroactive promotion of Assertion rows ingested before Session
  81 — the stage runs against whatever's in the assertions table on
  invoke. Operators wanting to consense a stale-but-rich plan run
  the command once after first dashboard open.
- Authoritative pathway not implemented. Plans whose primary source
  is `agency:usgs` or another high-authority source still wait for
  N=3 corroboration before lighting up the typed panel — the
  "instant promote for a single authoritative claim" behaviour ADR
  0004 names is a follow-on.

**Neutral**

- The consensus stage's `Confidence` choice (average) is one of
  several defensible options. Changing it later is a one-function
  edit; old promoted records keep their stored value.
- The `derived#consensus` source_id string is opaque on the wire.
  The dashboard's source-host helper renders it as a single chip;
  the prov chain is the load-bearing surface, not the source_id
  string.

## Code references

- `crates/pipeline/src/promote.rs` — implementation:
  `PromoteConfig`, `PromoteReport`, `PromoteError`,
  `promote_consensus_for_plan`, `promote_consensus_from_assertions`,
  `content_hash_for`, `subject_hash_for`, the promoted-record
  builders, the insert-outcome classifier.
- `crates/api/src/commands_records.rs::promote_consensus_for_plan` —
  the Tauri command that surfaces the stage to the desktop binary.
- `apps/desktop/src-tauri/src/main.rs` — invoke_handler registration
  for the new command.

## Review notes

This ADR is the implementation companion to ADR 0004. None of the
ADR 0004 commitments are revisited — only the previously-deferred
implementation choices are pinned. If a future session changes any
of:

- the hash function (DefaultHasher → SHA-256),
- the trigger model (manual → automatic on fetch completion),
- the confidence aggregation (average → Bayesian / median),
- the EntityAttribute promotion target (consensus-Assertion proxy →
  typed Entity-attribute column),

it should add an amendment rather than rewrite the section. ADR 0004
is the contract; this ADR records how Session 81 honoured it.
