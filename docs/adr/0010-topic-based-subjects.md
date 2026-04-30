# ADR 0010 — Topic-based subjects, no domain governance

**Status**: Accepted
**Date**: 2026-04-19
**Supersedes**: the `CommodityId` dimension that existed in Phase 2a
**Related**: ADR 0003 (six record types), ADR 0007 (research function)

## Context

Phase 2a shipped a schema in which `Subjects` had a first-class
`commodities: Vec<CommodityId>` field alongside `entities`, `countries`,
and `event_types`. The working assumption was that situation_room was — at
minimum — a commodity workstation, and that commodities deserved a
typed, validated, registry-backed dimension the way entities and
countries did.

During Phase 2c review we reached the opposite conclusion. A schema
that privileges "commodity" as a top-level dimension is a commodity
workstation dressed up as a general tool. The moment a user types
"Taiwan 2028 election" or "EU AI Act compliance" or "container
shipping rates", the privileged dimension goes unused, and the absence
of an analogous privileged dimension for sectors / policy areas /
technologies / elections becomes conspicuous. We would have had to
either (a) keep adding first-class dimensions forever, (b) cram
unrelated concepts into the `commodities` field, or (c) admit that the
field was a historical accident and migrate.

The question this ADR decides is: **how does situation_room represent the
"what is this record about" axis so that any subject a user might
research fits naturally, without periodic schema surgery?**

## Decision

situation_room's `Subjects` has exactly four dimensions, in this order:

1. **`entities: Vec<EntityId>`** — actors (companies, mines, vessels,
   agencies, people). Typed because entity-joins are the most heavily
   queried axis.
2. **`places: Vec<PlaceRef>`** — geography, enumerated (country /
   region / point). Typed because geographic filters need structure.
3. **`time: Option<TimeScope>`** — an optional scope that the record is
   *about*, distinct from `Envelope::valid_at` which is when the record
   is *true*. Typed because "records about Q3 2025" is a common filter.
4. **`topics: Vec<Topic>`** — free-form string tags carrying all
   domain-specific categorization.

`Topic` is a validated newtype: non-empty ASCII, 1–64 characters,
alphanumeric plus `_` and `-`. That is the full extent of its
validation. There is no topic registry, no domain governance, no
schema-per-domain, no allow-list.

Commodities, sectors, technologies, policy areas, elections,
geopolitical themes, scientific subfields — all sit in `topics` as
equal-class citizens. `Topic("Li")`, `Topic("semiconductors")`,
`Topic("tw_2028_presidential")`, and `Topic("ai_export_controls")` are
the same kind of thing as far as the schema is concerned.

Classification happens through `ResearchPlan::expectations`
(`RecordExpectations`): when the user types a query, the LLM
classifier produces a plan that says which metrics to capture as
`Observation`s, which event types to watch as `Event`s, which entity
kinds to track, etc. **The classifier cannot invent new record types
— the six are fixed — but it can freely populate which specific
metrics, events, entity kinds, and topic tags matter.** Classification
is per-session and does not mutate the schema.

## Rationale

**Why Topic is a validated newtype, not a String.** String-typing the
field would have meant accepting tabs, newlines, and 10 MB blobs at
the schema boundary. The 1–64 ASCII-alphanumeric-plus-`_-` constraint
catches obvious garbage at deserialization time without pretending to
be a registry.

**Why the three other dimensions stayed typed.** Entities, places,
and time are the axes on which situation_room will run structural queries:
"all records mentioning TSMC", "all records in East Asia", "all
records about Q3 2025". Those queries want indexed columns and
foreign keys. Topics, by contrast, are filter tags on a subject-search
query; they don't need the same structural rigor.

**Why no topic registry.** The registry would have to be either (a)
closed and curated by us, which makes the product unusable for
subjects we hadn't anticipated; (b) open with an approval workflow,
which is a product we don't want to build; or (c) open and
free-for-all, which is exactly what `Topic::new` already provides.

A fair objection: the classifier mechanic we chose — showing the
LLM the set of topics used in past sessions and asking it to reuse
them where they fit — is itself a kind of soft, emergent registry.
We accept that. The distinction that matters is where the constraint
lives: in the schema (hard, irreversible, bad classification output
pollutes forever) versus in the prompt (soft, per-session, a bad
output is recoverable and doesn't break old records). The prompt
layer is the right home for hygiene heuristics; the schema layer is
the wrong home. ADR 0007 describes the mechanic.

**Why governance happens through the six record types, not through
topics.** The six record types (`Observation`, `Event`, `Entity`,
`Relation`, `Document`, `Assertion`) are the real schema surface. A
record *has* to be one of the six — there is no seventh, and the
classifier cannot add one. Within those six, the bare content types
(`ObservationContent` etc.) constrain what a claim of each kind looks
like. That is the governance: the shape of what can be recorded, not
the taxonomy of what it's recorded about.

**Why classification produces `RecordExpectations`, not a domain
registry entry.** `RecordExpectations` is a per-session artifact, not
a schema change. It tells the source-matching layer "this session
wants wafer-starts metrics, fab-announcement events, equipment-vendor
entities"; those become filters over the six universal record types.
Two sessions on unrelated topics share zero machinery at the schema
level and everything at the query/render level. No runtime schema
evolution, no migrations for new domains, no governance step before a
user can research a new subject.

**Why this generalizes cleanly.** A research session on "lithium
production" populates `topics = [Li]`,
`expectations.observation_metrics = [production, reserves, price]`,
`expectations.entity_kinds = [mine, refinery, producer]`. A session
on "Taiwan 2028 election" populates
`topics = [tw_2028_presidential, taiwan_politics]`,
`expectations.event_types = [candidacy_announced, poll_published,
coalition_formed]`, `expectations.entity_kinds = [candidate, party,
pollster]`. Nothing in the schema changes between these two sessions.
Panels render from the same six-type query vocabulary. That is the
definition of "general".

## Alternatives considered

**Domain registry with per-domain schemas.** Each domain
(`commodity`, `election`, `supply-chain`) would have its own typed
`Subjects`, own `*Content` variants, own event-type enum, registered
at runtime. *Rejected*: multiplies the schema surface by the number of
domains; forces a governance step (who approves new domains? what
happens at the boundaries?); duplicates shared concerns (every domain
still needs entities and places); makes cross-domain queries hard
(a record that touches both "semiconductors" and "Taiwan politics"
has to choose a home).

**Enum of known topics.** `enum TopicKind { Commodity, Sector,
Technology, Policy, ... }`. *Rejected*: either the enum is closed,
which reintroduces the "what if the user types something outside it"
problem, or it's `Other(String)`, which is a topic string with extra
steps. And the kind rarely matters for queries — users filter on "this
topic" not "topics of this kind".

**Content-addressed topics.** Hash-derived stable ids with
canonicalization. *Rejected*: the whole point of topics is that they're
human-readable filter strings surfaced in prompts, UI, and logs.
Hashing them away would make every log line useless for debugging.

**Tags only, no typed dimensions at all.** Flatten `entities`,
`places`, `time`, `topics` into a single `Vec<String>`. *Rejected*:
we actually do need structural queries on entities and places (entity
joins, spatial intersection), and reducing them to strings would
regress those capabilities for the sake of uniformity we don't need.

**Keep `CommodityId` as one of many typed dimensions.** *Rejected*
for the reason stated in Context: privileging one domain warps the
schema without doing the work to generalize to all domains. The
commodity-seed list that used to live in code now lives in
`config/vocab/commodities.toml` as a data file — a suggestion for the
classifier, not a type in the code.

## Consequences

**Positive**

- Schema stays general. New research subjects (whatever they are)
  fit without schema migration.
- Query layer is uniform. Every panel filters by `subjects` and the
  four-field structure is always the same shape.
- Classification is per-session and reversible. A bad classifier
  output for one session doesn't pollute the schema.
- The six record types remain the single point where "what can
  situation_room record" is defined.

**Negative**

- Topic hygiene is not schema-enforced. Two sessions could use `Li`
  and `lithium` as distinct topics, or `chip_production` and
  `wafer_supply` for what is effectively the same research target,
  and never realize they belong together. This is mitigated at the
  classification layer, not the schema layer: before the Level-1
  classifier runs, it is shown the topic strings already in use
  across past sessions and instructed to reuse them when a new query
  is plausibly about the same subject. The classifier retains the
  freedom to introduce genuinely new topics — the existing set is
  context, not a closed list. See ADR 0007 for the mechanism. Topics
  remain, additionally, filter tags layered over structural
  dimensions (entities, places) which *are* typed — so "records
  about TSMC" or "records in Taiwan" returns the right set
  regardless of which topic strings the classifier chose.
- No IDE autocomplete for topics. The tradeoff for not having a
  registry; topics are string constants at the prompt boundary, not
  in code.
- If we later discover a dimension that genuinely deserves first-class
  treatment (hypothetical: "time period" turns out to be
  under-served and needs more structure), we would need a schema
  migration to add it. We accept this risk as the cost of not
  over-fitting the schema to guesses about what will matter.

**Neutral**

- `ResearchPlan::topic_tags: Vec<Topic>` is the set of tags the
  session will attach to every record it produces. This is the ingest
  pathway — records get tagged when stored, not queried.
- `config/vocab/commodities.toml` and `config/vocab/event_types.toml`
  are seed suggestions for the classifier, not registries. They can
  drift without code changes.

## Code references

- `crates/core/src/vocab.rs::Topic` — the validated newtype (lines ~78–103).
- `crates/core/src/schema/envelope.rs::Subjects` — the four-dimension struct.
- `crates/pipeline/src/research.rs::ResearchPlan::expectations`
  (`RecordExpectations`) — classification output.
- Module docs on `crates/core/src/vocab.rs` explain the "no
  CommodityId" decision at the code level.

## Review notes

Reviewed 2026-04-20. The human reviewer pushed back on the original
"tag hygiene managed in the prompt" hand-wave and requested an
explicit mechanic. Result: the classifier is shown the set of
topics already used in past sessions before it decides, so related
queries (the canonical example: `chip_production` vs `wafer_supply`)
converge on shared topic strings rather than spawning parallel
trees. The mechanic itself is documented in ADR 0007; this ADR now
references it instead of glossing.

The same review acknowledged that this mechanic is a soft,
emergent registry and that pretending otherwise would mislead
future contributors. The "Why no topic registry" rationale was
rewritten to name the tension openly and explain why the prompt
layer is the right home for it (recoverable, per-session) and the
schema layer is the wrong home (irreversible, pollutes old
records).

Dependency noted: the "show existing topics to classifier" step
requires a storage-layer query path for topics-in-use. Captured
for ADR 0007 and for the Phase 2e storage work.
