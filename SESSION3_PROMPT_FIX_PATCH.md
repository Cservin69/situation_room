# Session 3 — prompt fix (recipe_author v1.2)

Applies on top of Part 3. One file changed. No code changes.

    cd /Users/aben/RustroverProjects/situation_room
    tar -xzvf ~/Downloads/situation_room_session3_prompt_fix_patch.tar.gz

## What happened

The live e2e demo run showed xAI authoring a well-formed recipe
that nonetheless failed at apply time with:

    content assembly failed: observation content:
    unknown variant `2022`, expected one of `instant`, `daily`,
    `weekly`, `monthly`, `quarterly`, `annual`, `custom`

xAI had mapped the extracted value `"2022"` (a date string pulled
from the World Bank JSON response) to `ObservationContent.period`,
which is a closed enum that accepts only seven specific strings.

This is the deterministic runtime working exactly as designed:
closed enum, strict deserialization, failure surfaced at the
correct layer. The LLM's error is caught before any record is
persisted. But the prompt didn't tell xAI that `period` is a
closed enum, and a well-meaning LLM noticed the `"date": "2022"`
field in the JSON, heuristically matched it to the `period`
concept, and produced an `extracted` mapping that was almost
certain to fail.

## What this patch changes

One file: config/prompts/recipe_author.md.

- Bumped to v1.2.
- New "Content type reference" section that names the exact
  fields of each record type (observation, event, relation)
  and flags closed-enum fields explicitly: `period` and
  `direction`. Lists the allowed values. Tells the LLM to use
  a `literal` source for these, never `extracted`.
- Added a bullet to "What NOT to produce" that names the exact
  mistake xAI made this time: do not use `{"kind": "extracted"}`
  for closed-enum fields. The extracted value will almost always
  be in the source document's own spelling and will fail
  deserialization.
- Changelog entry naming the debugging run that caused the fix.

## What this patch does NOT change

- No code changes. The runtime rejection behavior is correct —
  we want a recipe that produces invalid content to fail loudly
  at apply time, not silently produce garbage.
- No schema-embedding automation. The content types in
  situation_room-core do not (and will not) derive schemars::JsonSchema
  just for the prompt — that would reverse-depend core on
  schemars for something core doesn't need. The content-type
  reference in the prompt is hand-maintained; if you add a new
  content type or change an enum, update the prompt alongside
  the code change.

## Running

No build step needed (prompt is included via `include_str!`, so
cargo will pick it up on next build). Re-run the live demo:

    cargo run -p situation_room-demo --bin situation_room-e2e -- --db /tmp/situation_room-e2e-live2.duckdb

Expected: step 4 still hits xAI, but the resulting recipe should
now map `period` to a `literal` with value `"annual"`. Step 7
should succeed and step 9 should print the Observation.

If xAI still produces a bad mapping for `period` (or any other
closed-enum field), that's a signal to escalate — either tier up
to Grok-4 Frontier, or we add a dry-run validation step in the
author path so a bad recipe is rejected before it hits storage.

## Latency note

The prompt grew from 172 to 256 lines. Expect authoring latency
to increase accordingly — probably by 30-50% over the 32s you
saw. If the growth is annoying, ask me to split the content-type
reference into a separate block that's only included when the
recipe targets that specific record type.

## Files in this archive

    SESSION3_PROMPT_FIX_PATCH.md
    config/prompts/recipe_author.md
