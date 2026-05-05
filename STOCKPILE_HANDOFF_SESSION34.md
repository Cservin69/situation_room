# situation_room — Session 34 followup

**Trigger:** Session 33's followup shipped the runtime bound that
makes apply-stage failures legible. The next live run of
`hungarian barley production` produced exactly the legible failure
the bound was designed to surface — though not the failure shape
33's followup anticipated. The chip-readable error and
accompanying screenshot showed two separate prompt-relevant
failures stacked in the same recipe:

- `world_bank_indicators`: `Failed @ Apply` with a clean named
  error from Session 32b's null-filter helper:
  `extraction [json_path]: path "$[1][0].value" matched 1 node(s),
  all JSON null. The source publishes nulls for unavailable data;
  refine the path with a filter expression…`. Legible. Actionable.
  But the *deeper* problem the legible error half-revealed: the
  recipe's `source_url` was the registered `endpoint_hint` echoed
  verbatim — `…/country/all/indicator/NY.GDP.MKTP.CD?…`. The hint
  carried two illustrative parameters (`country=all`, indicator =
  default macro). The LLM authored against the GDP-shaped excerpt
  without substituting either parameter for the plan's subjects,
  so even if the path had been a filter expression rather than a
  static index, the recipe would still have fetched the wrong
  *dataset* on every refresh.

The session's first proposal — naming the specific landing-page
URL from `usgs_mcs` in prompt language — was correctly flagged
by the operator as a category violation: ADR 0007's golden rule
applies to prompt text as much as to code. Source-specific
routing rules ("if URL contains X, do Y") in the prompt are the
prompt-side equivalent of the hand-rolled adapters Session 5
purged. The right level for a prompt revision is *principles* —
shape-anchored, source-agnostic — that force the LLM to fit our
forms regardless of which source it's authoring against.

Session 34 ships that revision.

## What this patch does

A single-file change to `config/prompts/recipe_author.md`
bumping the prompt to **v1.10**. Two new subsections inside URL
discipline plus a changelog entry. The output contract is
unchanged — same JSON Schema, same field-source kinds, same
binding rules. Recipes already authored remain valid; no
re-authoring required, no migration, no DTO change, no Rust
code touched.

### 1. New subsection: "Plan coherence — the URL must serve the plan's subjects"

Inserted between the existing case-1/case-2 paragraphs at the top
of URL discipline and the "Endpoint discipline — instance vs
listing" subsection. The principle:

> A URL on the source's documented endpoint shape is necessary
> but not sufficient. The URL must also be about the plan's
> subjects.

The subsection introduces vocabulary the rest of the prompt does
not yet use: **subject placeholders** (URL components whose value
would change if the same source were asked about a different
country / indicator / filing — these are the parts to substitute
per plan) versus **envelope shape** (the API's design — leave
alone). It then prescribes an order of operations:

1. Read the plan first (topic, geographic scope, expectation
   bucket).
2. Identify subject placeholders vs envelope shape in the
   prefetch URL.
3. Substitute the plan's subjects into the placeholders.
4. *Then* refine for tier (instance vs listing).
5. *Then* refine for completeness (Hunt the URL end-to-end).
6. *Then* author the extraction.

The rationale folded in: the substituted-URL response shape
matches the prefetch envelope by API design, so the extraction
path written against the prefetch generally still applies. You
extract the same envelope's leaf, just from a different subject's
data. The prompt makes that explicit so the LLM doesn't think of
substitution as a step that invalidates its envelope-reading.

The anti-example that closes the subsection describes the failure
shape **without naming the source**: a country-indicator API with
illustrative `{country}` and `{indicator}` parameters; the LLM
echoed both unsubstituted; the recipe fetched the default-subject
series forever. The fix is described in terms of *what to
substitute to* (the plan's `geographic_scope.code`, the source-
catalog code matching the plan's actual metric), not in terms of
which API to do it for.

This is the deliberate departure from earlier sections' worked
examples (which name `eur_lex` and the AI Act CELEX number).
v1.10's anti-example is structurally generalizable — any
country-indicator API, any plan, any default subjects — so the
rule it teaches is unmistakably about the *shape*, not about one
source.

### 2. New subsection: "Pre-flight checklist"

Inserted at the end of URL discipline (after "Hunt the URL
end-to-end") and before "Strategy for PDF sources". A five-item
list pulling the most-violated rules into one short block
adjacent to where the LLM commits to `source_url` and
`extraction`:

1. Subjects (plan coherence).
2. Tier (instance vs listing).
3. Refinement (Hunt the URL end-to-end).
4. Path shape (filter expression vs static index — Type honesty).
5. Type fit (f64, String, closed enums — Type honesty).

Each item carries a parenthetical pointer back to the section it
summarizes. The closing rationale paragraph names *why* the
checklist exists despite the rules already appearing in earlier
prose: the rules are read in document order but committed to in
authoring order, and the rule the recipe is about to violate is
often the one furthest back in the prompt by the time it
matters. The checklist reorders the rules to be adjacent to the
moment of decision.

### 3. Changelog entry

A v1.10 entry at the top of the changelog, describing the
sub-clause-buried-rule diagnosis (v1.9 had the substitution rule
and the null-filter rule both already present, just not adjacent
to the moment of commitment), the lift-and-checklist remedy, and
the explicit "anti-example anchored in the failure shape, not in
the source's identity" architectural note.

## Why this is "the right lever"

The Session 33 followup left the question open: prompt-side or
runtime-side? With Session 33's bound applied, the failure shape
is now legible enough for the operator's chip mechanic to do its
job; the runtime-side work for this failure family is complete.
What's left is teaching the LLM to avoid the failure upstream.
That's prompt territory by definition.

Could the reauthor-on-failure flow (ADR 0012) be the lever
instead? In principle the legible apply error feeds back into a
re-author and the next attempt corrects itself. In practice the
reauthor flow is bounded — it's an escalation surface, not a
substitute for getting the first attempt right. A prompt that
ships recipes failing 100% on first author burns operator
attention even when reauthor eventually succeeds. The prompt is
where the cost-effective fix lives.

## What this patch is NOT

- **Not a fix for `sec_edgar` 403.** Still its own session,
  probably with an ADR — per-source HTTP overrides
  (User-Agent in particular, also possibly request headers more
  generally) need a config-shape decision the prompt cannot make.
  Carried forward from Session 33's followup.

- **Not source-specific routing.** The anti-example describes a
  failure *shape* — country-indicator API with illustrative
  parameters — without naming the source. Any prompt that named
  `world_bank_indicators` (or `usgs_mcs`, or any other source) as
  the *condition* of a routing rule would be the prompt-side
  equivalent of the per-commodity adapters Session 5 purged. ADR
  0007 forbids that level identically in code and in prose.

- **Not a code or DTO change.** The prompt's output contract is
  unchanged. `cargo check`, `cargo test`, ts-rs codegen, and the
  desktop UI are all unaffected.

- **Not a guarantee.** v1.9 already gestured at parameter
  substitution as a sub-clause of case 1 ("swap an indicator
  code") and v1.9's "Type honesty" already named the
  null-at-static-index failure shape with the verbatim fix. Both
  rules were present and the LLM violated both. v1.10 makes them
  more salient by lifting plan-coherence to its own subsection
  and consolidating the most-violated rules into a checklist
  adjacent to the moment of decision. If a re-run still produces
  an unsubstituted recipe, the next move is *not* more prompt
  prose — it is the reauthor flow with the legible apply error
  feeding back as `{{PREVIOUS_FAILURE_REASON}}`, possibly with
  an `{{OPERATOR_GUIDANCE}}` correction. The prompt has done
  what the prompt can do; further drift is an empirical signal
  to use the escalation surface, not to keep adding prose.

- **Not a re-author of existing recipes.** The output contract is
  unchanged. The previously-authored
  `019df6d0-e9eb-7ff3-a74a-e909a565c14c` recipe stays in the
  store — it remains valid as data, just visibly wrong. The
  operator flags it (chip mechanic) and the reauthor flow loads
  the v1.10 prompt for the next attempt.

## Apply

```bash
cd /Users/aben/RustroverProjects/situation_room
tar -xzf ~/Downloads/situation_room_session34.tar.gz --strip-components=1 -C .
```

The patch touches only `config/prompts/recipe_author.md`. There
is nothing to compile or test. Verify the bump by reading the
v1.10 line at the top of the file and the v1.10 changelog entry.

Then re-run `hungarian barley production` (or any plan whose
matching source has a parameterized `endpoint_hint`). The
expected new state of the run:

- The classifier's source nominations are unchanged.
- The recipe author's `source_url` should now substitute the
  plan's geographic scope into the URL's country parameter
  (`HU` rather than `all`) and the plan's metric into the URL's
  indicator parameter (an agriculture-family World Bank code,
  e.g. `AG.PRD.CREL.MT` or similar — the source's catalog
  contains the right one).
- The `json_path` should use a filter expression
  (`$[1][?(@.value)].value` at minimum, ideally
  `$[1][?(@.country.value=="…")].value` or similar identity
  filter) rather than `$[1][0].value`.
- If both improve, the recipe produces records on first apply.
- If only the URL improves but the path is still positional, the
  failure is the legible Session-32b null-filter error, the chip
  mechanic surfaces it, the operator flags. Reauthor on flag.
- If neither improves, the diagnostic moves to the reauthor
  flow: the legible apply error is captured as
  `{{PREVIOUS_FAILURE_REASON}}` and the next attempt sees both
  v1.10's prose *and* the verbatim error from the prior try.

## Honest expectation-setting

Sessions 30 → 32a → 32b → 33 → 34 form an arc whose layers get
smaller each session:

- **30** revealed the wrong-endpoint pattern.
- **32a** fixed the registered endpoint hint and revealed the
  JSON-null pattern.
- **32b** filtered nulls in the helper error and revealed the
  wrong-page pattern.
- **33** bounded the wrong-page failure so it's chip-readable
  and revealed the wrong-subject pattern.
- **34** (this) teaches the LLM to substitute subjects.

If 34 holds, the next layer the live run exposes is — by the
arc's pattern — smaller still. If it doesn't hold, the reauthor
flow is the next mechanism, not v1.11. The discipline is: prompt
revisions are empirical, prompted by observed classifications,
and one-at-a-time. v1.10 is the empirical revision for the run
in the screenshot; the next prompt revision waits for the next
empirical signal, not for speculation about what might happen.

## Architectural note for future sessions

ADR 0007's golden rule applies to prompt text identically to
code. Specifically:

- **Anti-examples may describe failure shapes by structure** (a
  country-indicator API, a search-form skeleton, an instance URL
  on a multi-expectation bucket) and may name a source only as
  *illustration* of that shape (the existing eur_lex examples in
  v1.5 / v1.6 / v1.9 are structurally about instance-vs-listing
  and search-refinement, not about eur_lex specifically).
- **Anti-examples may NOT bake source-specific routing rules**
  ("if `source_id` is X, prefer endpoint Y", "URL Z is a
  navigation page so do W"). Source-specific routing in prompt
  prose is the prose equivalent of the per-commodity adapters
  Session 5 purged.
- **The line:** if the rule generalizes — country-indicator APIs
  with illustrative parameters; instance URLs on multi-record
  buckets — write the principle. If the rule does not
  generalize — `usgs.gov/.../mineral-commodity-summaries` is a
  navigation page — the prompt is the wrong tool. Either
  describe the *property* that makes a navigation page a
  navigation page (no machine-readable data, only links to
  documents) and let the LLM apply that property generally, or
  do not name the property at all and rely on the chip-and-
  reauthor escalation surface.

The Session 34 anti-example is the first prompt anti-example
that names *no* source. That's the model going forward unless
the existing house style (named-source-as-illustration) is more
useful for the rule being taught.

End of followup.
