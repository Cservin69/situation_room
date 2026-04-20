# ADR 0004 — Assertion promotion model

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0003 (six record types), ADR 0007 (research function)

## Context

Stockpile distinguishes between *facts* (measurements Stockpile made
itself or pulled directly from authoritative structured sources) and
*claims* (things someone said in a document Stockpile ingested).
ADR 0003 codified this by making `Assertion` the sixth record type:
it wraps a content shape (`ObservationContent`, `EventContent`,
`RelationContent`, or `EntityAttributeContent`) with a claimant, a
stance, and an envelope pointing to where the claim was made.

That leaves a question ADR 0003 deliberately deferred: **when and
how does a claim become a fact?**

A Reuters article says "USGS reports Chile produced 142kt of
lithium in 2025." The extraction layer emits an Assertion:
claimant = USGS, stance = `Asserted`, content = an
`ObservationContent` with value = 142,000 tonnes, provenance = the
Reuters article. The Assertion captures the claim faithfully. But
the Price/Production panel doesn't want to render *a Reuters claim
about USGS's claim*; it wants the number. At some point, the
Assertion has to turn into an `Observation` that the panel can
render directly.

Two pressures point in opposite directions:

1. Panels need *facts*, not an ever-growing pile of claims. Rendering
   every Reuters mention of the USGS figure as a separate assertion
   is noise. The panel wants one number with a confidence score.
2. The claim structure is valuable on its own. When five sources
   say "Chile will cut exports" and USGS says "production is
   steady", the disagreement is a *signal*, not a problem to
   resolve. Collapsing everything to a single consensus fact
   destroys the signal.

The design question is: how do we satisfy both — give panels clean
facts to render, while preserving the claim structure underneath?

## Decision

**Assertions are promoted into other record types via two pathways.
The Assertion layer is preserved indefinitely alongside the promoted
records.**

### The two promotion pathways

**1. Authoritative-source promotion.** If an Assertion's claimant is
on the authoritative list for a given record type + metric + subject
combination, the Assertion is promoted immediately. A single
USGS-claimed production figure becomes an `Observation` on first
sight. No waiting, no consensus.

Authoritative status is *per-content-type × per-subject*, not
global. USGS is authoritative for US mineral production, not
authoritative for stock prices. The LME is authoritative for copper
warehouse stocks, not authoritative for policy events. The
authoritative registry is configuration, not code, and lives in
`config/authoritative.toml` (to be created in Phase 3).

**2. Consensus promotion.** If no single claimant on an Assertion
is authoritative, the promotion waits for a quorum: N independent
claimants making Assertions with compatible content. When quorum is
met, a single promoted record is emitted. "Compatible" is defined
per-content-type: for `ObservationContent`, value agreement within
a tolerance; for `EventContent`, same event_type and overlapping
time window; for `RelationContent`, same kind and endpoints; for
`EntityAttributeContent`, same attribute value.

Default quorum is **N=3 independent claimants**. Tunable per
metric, event type, or subject; configured alongside the
authoritative list. "Independent" means distinct `claimant`
EntityIds — two articles from the same wire service quoting each
other don't count as two.

### What gets promoted

Promotion produces records of the type the `AssertedContent`
indicates:

- `AssertedContent::Observation` → `Observation`
- `AssertedContent::Event` → `Event`
- `AssertedContent::Relation` → `Relation`
- `AssertedContent::EntityAttribute` → updates the target `Entity`'s
  attributes (not a new Entity record; attributes attach to the
  entity identified by the claim's subject).

There is no `AssertedContent::Entity` or `AssertedContent::Document`
— these aren't claim shapes. An entity either exists in the registry
or doesn't; a document is raw ingested content, not a claim about
the world. `AssertedContent::Assertion` doesn't exist either
(no meta-claims). This constraint is enforced at the type level by
the enum in `crates/core/src/schema/content.rs::AssertedContent`.

### The preservation commitment

**Promoted Assertions are not deleted.** They remain in storage,
queryable, with their full claim structure intact. The promoted
record references them via `Envelope::provenance.derived_from` with
a `DerivationRole::Promotion` tag (see
`crates/core/src/schema/records/mod.rs::tests::promotion_preserves_content`).

Query patterns the preservation enables:

- "Show me every claim about Chilean lithium production in Q3 2025,
  regardless of which ones got promoted." — returns the Assertions.
- "Which claimants' assertions most often agreed with the eventual
  promoted value?" — joins Assertions with their promoted
  descendants and scores accuracy over time.
- "Where did the anomaly detector spot a flurry of `Predicted`
  claims that contradicted the steady `Asserted` measurements?" —
  queries Assertions with stance ≠ `Asserted` and compares to
  Observations in the same window.

These queries are *the product*, not a debugging convenience. The
anomaly detectors in `crates/analytics/` depend on Assertion-level
data remaining available.

### Promotion mechanics

Promotion is a pipeline stage in `crates/pipeline/src/promote.rs`
(stubbed in Phase 1). It runs after extraction and before storage-
commit, consuming freshly-produced Assertions and producing
promoted records when conditions are met.

For **authoritative promotion**:

1. Extract Assertion with claimant `C`, content type `T`, subject
   tags `S`.
2. Lookup `(C, T, S)` in the authoritative registry.
3. If found: construct a new record of the appropriate type, copy
   the content, construct an envelope with provenance reading
   `derived_from = [Assertion.id with role = Promotion]`, emit.
4. Original Assertion is committed unchanged.

For **consensus promotion**:

1. Extract Assertion, commit it unchanged.
2. Query for other Assertions with the same content-type, compatible
   content, and subject-set overlap, within a configured time window.
3. If quorum met: construct the promoted record as above, with
   `derived_from = [all N Assertion ids with role = Promotion]`.
4. If quorum met on a subsequent Assertion (the one that tipped it
   over), promotion happens then — the promoted record's
   `observed_at` is the time of promotion, not the time of the
   first Assertion.

Consensus promotion is idempotent: re-running it on the same
Assertions after quorum has already produced a promoted record
should not produce a second. Dedup is via
`dedup_key = promotion:{content_hash}:{subject_hash}` — this is one
of the few places where content-derived keys appear in the system
(see ADR 0003's preference for UUIDv7 + `dedup_key`; promotion's
dedup key is computed from content because it's defining *what*
got promoted, not *what source said it*).

## Rationale

**Why two pathways.** A system with only consensus promotion can't
promote anything until quorum is met, which means USGS's annual
Mineral Commodity Summaries — the single most authoritative public
source for US minerals data — couldn't produce Observations until
two other sources independently reported the same numbers. That's
backwards; the point of authoritative status is that we've already
made the judgment that the source is trustworthy.

A system with only authoritative promotion can't handle subjects
where no single source is dispositive — emerging technologies,
geopolitical events, things where the "truth" is genuinely
distributed across multiple reporters. Those cases need consensus
or they remain forever stuck as un-promoted claims.

The two pathways cover the two regimes: known authoritative sources
fast-track; everything else waits for corroboration.

**Why keep Assertions after promotion.** Two reasons, both
product-critical:

1. *Disagreement as signal.* The anomaly-detection layer treats
   divergence between Assertions as one of its primary inputs. A
   cluster of `Predicted` Assertions followed by an `Asserted`
   contradiction is a known signature of rumor-driven market moves.
   You can't detect this if Assertions are deleted on promotion —
   you'd only see the consensus-survivor.
2. *Auditability.* "Here's the Observation, here are the seven
   claims it was derived from, here's who said what and when" is
   the traceability story Stockpile promises. Collapsing to only
   the promoted record turns that story into "trust me, the
   consensus said so."

Storage cost is real — Assertions don't get garbage-collected — but
bounded: the growth rate is tied to ingestion rate, not retention
forever-multiplied-by-sources, and compression on the
Assertion-heavy tables is straightforward if it matters. (Phase 2e
will benchmark.)

**Why the default quorum is N=3 claimants.** Two is too low — a
single wire-service story and its downstream aggregators would pass.
Four is too high — legitimate news often has a handful of
independent reporters, not many. Three is the smallest quorum that
actually requires independence. Per-metric tuning lets us raise it
for unreliable topics (Twitter speculation about earnings should
probably need five) and lower it for unambiguous ones (two
government-statistics-agency reports on the same figure should
probably suffice).

**Why content-derived dedup_keys for promotion, against the usual
preference for UUIDv7.** The question "has this already been
promoted" is inherently about content identity, not about which
source reported it. Two Assertions asserting the same fact should
not each trigger a separate promotion. A UUIDv7-based dedup would
have no idea these are "the same" promotion target. The hashed key
is local to the promotion table and does not leak into the record
layer's identity model — records still have UUIDv7 ids; the hash is
metadata on the pipeline stage, not on the record.

**Why the authoritative registry is config, not code.** The set of
authoritative sources is domain-specific and changes: USGS may lose
authoritative status for a specific metric if a better source
appears; a new agency may become authoritative when it starts
publishing. Treating this as configuration means updates don't
require recompiles and the registry is inspectable without reading
Rust source.

## Alternatives considered

**Delete Assertions on promotion.** Rejected: destroys disagreement-
as-signal queries and weakens auditability. See "Why keep
Assertions" above.

**Single pathway — consensus only.** Rejected: can't promote
authoritative single-source facts, which are the cleanest,
fastest-to-panel data Stockpile can surface.

**Single pathway — authoritative only.** Rejected: inverts the
problem. Topics without dispositive sources never leave the
Assertion layer.

**Three pathways — authoritative + consensus + user-trust.** Let
users mark individual sources as trusted for their session. Rejected
for now (not forever): adds UX complexity before the baseline is
built, and user-local trust is hard to reconcile with the
persistent-storage model. Could be revisited in Phase 4+ as a
"watchlist" feature layered on top.

**Reputation-weighted consensus.** Instead of fixed quorum, weight
each claimant by historical accuracy and promote when weighted
sum exceeds a threshold. Rejected for now: requires a reputation
model we don't have yet and introduces feedback loops (promoted
records become the ground truth that retroactively scores
historical claims). The fixed-quorum model can be upgraded to this
later without breaking the schema.

**Promote to "provisional" records with lower confidence.** Instead
of waiting for quorum, promote each Assertion immediately to a
low-confidence Observation and let the confidence drift up as
corroborating Assertions arrive. Rejected: pollutes the Observation
layer with records that haven't met the trust bar, and the
"confidence" slot already has a semantic meaning (the rubric in
`Confidence`) that doesn't map cleanly to "how many corroborators
so far."

## Consequences

**Positive**

- Panels render clean promoted records while the Assertion layer
  remains available for users who want to drill down.
- Anomaly detection works as designed — disagreement queries are
  supported natively.
- Authoritative sources produce panel-ready data on first ingest;
  non-authoritative sources build evidence toward promotion over
  time.
- The distinction between "measured" and "claimed" is load-bearing
  at the schema level, which prevents downstream code from
  forgetting it.

**Negative**

- Storage grows with Assertions plus promoted records. Acceptable
  for the product's research-workstation model (single-user or small-
  team deployment), may need a retention policy for multi-tenant or
  long-running deployments. Not a Phase 3 problem.
- Consensus-promotion timing is non-obvious: a user researching a
  new topic may see Assertions that haven't reached quorum yet, and
  those won't appear in panels until the third claim arrives. UX
  needs to surface this clearly ("3 claims needed, 2 received") so
  users don't think the panel is broken.
- The authoritative registry is a piece of state that has to be
  maintained and audited. Getting it wrong in the "too permissive"
  direction lets weak sources fast-track; getting it wrong in the
  "too restrictive" direction means slow quorum for things that
  should be immediate.

**Neutral**

- `AssertedContent` covers exactly four of the six record types —
  Observation, Event, Relation, and (via `EntityAttributeContent`)
  partial updates to Entity attributes. Entities as whole records
  and Documents are registered/ingested directly, not asserted.

## Code references

- `crates/core/src/schema/records/assertion.rs::Assertion` — the
  Assertion type with claimant + stance + content + envelope.
- `crates/core/src/schema/content.rs::AssertedContent` — the enum
  limiting what an Assertion can wrap.
- `crates/core/src/vocab.rs::Stance` — the six-variant stance enum
  (Asserted, Hedged, Denied, Reported, Predicted, Speculated).
- `crates/core/src/schema/envelope.rs::Provenance::derived_from`
  and `DerivationRole::Promotion` — the chain showing where a
  promoted record came from.
- `crates/core/src/schema/records/mod.rs::tests::promotion_preserves_content` —
  end-to-end test of the promotion mechanic.
- `crates/pipeline/src/promote.rs` — pipeline stage
  implementation (stubbed in Phase 1).
- `config/authoritative.toml` — authoritative-source registry
  (to be created in Phase 3).

## Review notes

Reviewed 2026-04-20. This ADR codifies decisions reached during
Phase 2a review. The two-pathway model was the human reviewer's
synthesis: authoritative for fast-track, consensus for the rest.
The commitment to preserve Assertions indefinitely came from the
same review, specifically to support the anomaly-detection queries
described in `crates/analytics/`.

Design decisions delegated to this ADR's author, captured for
traceability:

- Default consensus quorum is N=3 independent claimants, tunable
  per-metric.
- Authoritative registry lives in `config/authoritative.toml`, not
  in code.
- Promotion is idempotent via content-derived dedup_keys (one of
  the few places content-hashing appears; scoped to pipeline state,
  not record identity).
- Entity promotion is via `EntityAttributeContent` updates, not
  via new Entity records (entities are registered, not asserted).

None of these block Phase 2 or Phase 3 implementation work; all are
refinements that can be adjusted during Phase 3 when real USGS data
starts flowing through the promotion stage.
