# ADR 0003 — Six record types as the universal schema

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0004 (assertion promotion), ADR 0010 (topic-based
subjects), ADR 0007 (research function)

## Context

Stockpile is a research workstation. It ingests facts from public
sources — prices, production numbers, shipments, trade flows,
regulatory actions, corporate filings, news reports — and surfaces
them in a way that the user can query, cross-reference, and trace
to a source. Two kinds of design question have to be answered before
any of that works:

1. **What shapes can a fact take?** Prices and production numbers
   are clearly structured differently from news events, which are
   clearly different from corporate ownership links. But how many
   distinct shapes are there, really?
2. **How do we handle claims that aren't yet facts?** A news article
   reporting "Chile is expected to cut lithium output" is an
   important signal, but it's not a measurement. It's a claim, made
   by someone, with a stance. How does the schema make room for
   that without polluting the measurement layer?

The temptation was to grow the type list as new needs emerged: start
with Observation, add Event when news came in, add Entity when
companies needed tracking, add Geometry when satellite data
appeared, add Filing when SEC docs arrived, and so on. That path
leads to a schema with fifteen or twenty types, each with overlapping
fields, inconsistent envelopes, and ad-hoc cross-references. Every
panel query has to handle N cases. Every source adapter has to
decide which of the N types to emit. Every new idea threatens a new
type.

This ADR decides: **how many fundamental record shapes does
Stockpile need, and why exactly that number?**

## Decision

Stockpile recognizes **exactly six record types**, and no seventh
will be added:

1. **`Observation`** — a measurement of a metric. Price, production
   volume, utilization rate, inventory level, percentage, count.
2. **`Event`** — a discrete thing that happened in the world at a
   point or interval. Export restriction enacted, mine closed,
   election held, fab announced.
3. **`Entity`** — an actor or object of interest. Company, mine,
   vessel, port, person, agency, fab, facility.
4. **`Relation`** — a typed link between two or more entities.
   Subsidiary, supply contract, joint venture, fab operator.
5. **`Document`** — a source document that other records were
   extracted from. News article, SEC filing, press release, PDF
   report.
6. **`Assertion`** — a *claim* rather than a fact. Wraps one of the
   other content shapes and adds a claimant, a stance, and
   provenance pointing to where the claim was made.

All six share the same `Envelope` (provenance, subjects, tags,
timestamps, confidence). The unified `Record` enum wraps them with a
type discriminator so every downstream consumer — queries, UI, API —
can work with them uniformly.

Three further commitments flow from this choice and are part of the
same decision:

**Geometry is a field, not a type.** Spatial data lives as an
optional `Geometry` field on `Entity`, `Event`, and
`ObservationContent`. There is no `GeoFeature` or `SpatialRecord`
top-level type. A mine has geometry because mines have locations,
not because there's a separate "places that are mines" record.

**Content types exist bare.** `ObservationContent`, `EventContent`,
`RelationContent`, `EntityAttributeContent` are defined without an
envelope or an id. They are pure claim shapes. The full record types
(`Observation`, `Event`, etc.) wrap these with an envelope and a
UUIDv7. `Assertion` references `AssertedContent`, an enum of the
bare content types, so an assertion about a production number wraps
`ObservationContent` — not `Observation` — and therefore doesn't
carry a spurious id/envelope for a record that doesn't exist yet.

**No seventh type.** If a new concept seems to need its own type,
the answer is either "it's a specialization of one of the six"
(e.g. an SEC filing is a `Document` with specific tags; an election
result is an `Event` of type `election_result`) or "it's derived
data that can be computed from the base six" (e.g. trends,
aggregates, anomalies). Adding a seventh would require revisiting
this ADR and every cross-cutting consumer (storage schema, query
layer, UI panels, ts-rs bindings).

## Rationale

**Why six and not more.** The six emerge from answering "what
shapes does a fact take?" exhaustively:

- Some facts are *measurements* → `Observation`.
- Some facts are *occurrences* → `Event`.
- Some facts are *existences* → `Entity`.
- Some facts are *links between existences* → `Relation`.
- Some facts are *the sources the above were derived from* →
  `Document`.
- Some "facts" aren't facts yet — they're *claims* → `Assertion`.

These are orthogonal. A relation isn't a degenerate event; an event
isn't a degenerate observation. Each answers a different query
pattern. Collapsing any two would force one of them to pretend to be
the other, and the query layer would pay for it forever.

**Why six and not fewer.** A version of this schema could have only
three or four types by collapsing some:

- *Merge Observation into Event?* ("a price measurement is an event
  of type 'price observed'.") Rejected because time-series queries
  on observations are fundamentally different from event timelines
  — observations have `metric`, `value`, `unit`, `period`; events
  have `event_type`, descriptions, typed impact. Forcing both
  through one shape means every observation carries unused event
  fields and every event carries unused value/unit fields.
- *Merge Entity and Relation?* ("a relation is an entity whose kind
  is 'link'.") Rejected because entities have identity and
  persistence; relations have directionality and endpoints. The
  graph topology is different.
- *Drop Document in favor of provenance URLs?* Rejected because
  documents are themselves queryable — a user wants to ask "what
  articles mentioned this topic in the last week" and get documents
  back as first-class results, not as URL strings embedded in other
  records' provenance.
- *Drop Assertion and treat everything as direct fact?* Rejected
  because stance matters. An article saying "Chile will ban lithium
  exports" and an article saying "Chile banned lithium exports" are
  very different signals. Flattening them into "fact" erases the
  distinction the anomaly-detection layer depends on.

Six is the smallest number that preserves the query patterns we
actually need.

**Why Assertion was added last.** Early drafts had five types:
Observation, Event, Entity, Relation, Document. The design assumed
extraction would produce records directly — the LLM reads an article
and emits an `Observation` if the article contained a number. But
that conflates measurement with reporting-of-measurement. If Reuters
reports that USGS says Chile produced 142kt, the number in the
article is not a measurement Stockpile made; it's a claim Reuters
relayed about a claim USGS made. Treating it as an `Observation`
throws away the claim structure.

`Assertion` was added as the sixth type to carry exactly this
structure: claimant (who is making the claim — USGS), stance (how
they're making it — asserted vs hedged vs predicted), content (what
they're claiming — an `ObservationContent`), and envelope
(provenance points to the article, where Stockpile learned the
claim). ADR 0004 covers how Assertions get promoted into the other
five types when the claim is trusted enough to treat as fact.

**Why geometry isn't a seventh type.** An early draft proposed
`GeoFeature` as a top-level type for spatial data. This had intuitive
appeal — maps want features, and features are their own thing. But
the query patterns don't support it: you don't ask for "the
features" independently of what they're attached to. You ask for
"where is this mine" (geometry of an Entity), "where did this event
occur" (geometry of an Event), "where was this satellite reading"
(geometry of an Observation). Geometry is always *about* something
else. Making it a field on the thing it's about keeps the query
"show me all entities within this polygon" as a single join, not two.

**Why bare content types.** The same claim shape needs to show up
in two places: inside a full record (with id and envelope) and
inside an Assertion (as the content being claimed, without an id
because the record doesn't exist yet). Defining `ObservationContent`
bare and composing it into both `Observation` and `AssertedContent`
means the claim shape is defined once. Promotion (Assertion →
Observation) is then a literal copy of the content into a new
envelope. If the content types were tangled with ids and envelopes,
we'd have to either duplicate the shape or synthesize fake envelopes
for unpromoted assertions. Both are bad.

This pattern is tested end-to-end in
`crates/core/src/schema/records/mod.rs::tests::promotion_preserves_content`.

## Alternatives considered

**Three types: "fact / claim / thing".** Collapse Observation and
Event into "fact", Entity and Relation into "thing", keep Assertion
and Document. Rejected: loses the query structure that distinguishes
metrics from events and entities from links.

**Fifteen+ types, one per source kind.** Observation, Price, Flow,
Stock, Event, Filing, Article, Report, Entity, Company, Mine,
Vessel, Relation, Ownership, Contract, ... Rejected: multiplies
cross-cutting consumers by the number of types, makes every new
source adapter a decision about which of N types to emit, and
duplicates fields (every "thing"-ish type would re-declare
identity, every "event"-ish type would re-declare timestamps).

**Seven types with Geometry as top-level.** Rejected: query
patterns always attach geometry to something else, so the
independent type is unused. See above.

**Five types, extraction emits direct records.** Rejected: conflates
claim with measurement, erases stance, and breaks the anomaly
detector's ability to distinguish rumor-driven moves from
measurement-driven ones.

**Open record-type system, extensible at runtime.** Let sources or
configurations register new types. Rejected: every cross-cutting
consumer (storage, query, UI, ts-rs) would have to handle unknown
types. Governance would need to prevent name collisions. Adding
types is cheap; removing them is expensive — an open system makes
removal effectively impossible.

## Consequences

**Positive**

- Storage schema has six tables, each with a known shape, and
  junction tables for shared structure (subjects, derived-from).
  Phase 2e migrations are bounded by a small, closed set.
- Query layer is uniform. Every panel filter, every time-window,
  every subject join works the same way across record types.
- ts-rs binding generation produces a bounded set of frontend types.
  The frontend can exhaustively handle `Record` with a six-way
  pattern match.
- LLM structured output is constrained: classification (ADR 0007
  Level 1) nominates which of six types each expectation maps to;
  extraction (Assertion-producing pipeline stage) picks from six;
  neither can invent.
- The governance question — "can we record X?" — has one answer:
  it's one of the six or it's derived from them. No domain
  registry, no per-domain schema.

**Negative**

- The six-type commitment is load-bearing. If we discover a genuine
  seventh shape later, undoing this is expensive: new table, new
  query paths, new UI, new ts-rs bindings, migration of existing
  data. We accept this risk in exchange for the consumer-side
  simplicity. If the question comes up, the default answer is "it's
  one of the six" until proven otherwise.
- Some concepts feel awkward as specializations. An SEC 10-K filing
  is "just a `Document`" with some tags and a derived relationship
  to its filer — the richness of the filing lives in the tags and
  in the Observations/Assertions extracted from it, not in the
  `Document` type itself. This is deliberate but initially
  counterintuitive.

**Neutral**

- The bare-content-types pattern means two parallel hierarchies
  exist: full records (with id + envelope) and content types (bare).
  Contributors have to understand both. The tradeoff is that
  promotion is a copy, not a transformation.

## Code references

- `crates/core/src/schema/records/mod.rs::Record` — the unified
  enum with the six variants and uniform accessors.
- `crates/core/src/schema/records/{observation,event,entity,relation,document,assertion}.rs` — the six record types.
- `crates/core/src/schema/content.rs` — bare content types
  (`ObservationContent`, `EventContent`, `RelationContent`,
  `EntityAttributeContent`) and `AssertedContent` enum.
- `crates/core/src/schema/envelope.rs::Envelope` — the shared
  metadata shape.
- `crates/core/src/schema/records/mod.rs::tests::promotion_preserves_content` —
  end-to-end test for Assertion → Observation promotion preserving
  content identity.

## Review notes

Reviewed 2026-04-20. This ADR codifies a decision reached across
Phase 2a and 2c and was previously only captured in the handoff
document. The human reviewer's pushback during Phase 2a drove the
addition of `Assertion` as the sixth type (the original design had
five and assumed direct extraction); that pushback is the reason
the schema cleanly separates claim from measurement.

The "geometry is a field" commitment also came from that review —
the initial design had seven types with `GeoFeature` as a top-level
entry, and the reviewer observed that geometry was always attached
to something else in the query patterns that mattered, which
collapsed it to an optional field on the types that need it.

No new decisions are made in this ADR. It captures existing
commitments in a form that future contributors can reference
without having to reconstruct the reasoning from code archaeology
or the handoff document.
