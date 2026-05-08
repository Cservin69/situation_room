# Session 42 — Patch 3 of 4

Items 3 and 6 from the Session 42 handoff: JSON shape outline in the
prefetch excerpt, plus the integration test pinning that the
mode-agnostic validator catches the Session-32 World Bank null trap
through the same dispatch the runtime uses at apply.

## Apply

Files were edited in place by the desktop-agent session. To verify
from a clean state:

```
cd ~/Documents/Claude/Projects/SituationRoom
cargo build --workspace
cargo test --workspace
cd apps/desktop && npm run check
```

## Files changed

- `crates/pipeline/src/fetch_executor.rs` — `is_json` sniffer, JSON
  outline constants, `JsonPathStats`, `walk_json`,
  `render_json_shape`, `resolve_array_at_path`, dispatch branch in
  `prefetch_excerpt`, plus six unit tests and one integration test.
- `crates/pipeline/src/recipe_apply.rs` — one new test:
  `validate_recipe_json_path_rejects_world_bank_leading_null_trap`.
- `config/prompts/recipe_author.md` — `json_path` bullet extended
  with the new outline reference; `Document excerpt` section
  extended to cover the JSON case.

No changes to `recipe_author.rs`, schemas, types, or DTOs. Item 3 is
authoring evidence; item 6's validator dispatch was already
mode-agnostic by construction (Session 41 patch 1).

## What this patch does

### Item 3 — JSON shape outline in `prefetch_excerpt`

`prefetch_excerpt` is now a **four-way** dispatch (PDF / HTML / JSON
/ raw). The new JSON branch sniffs by leading `{` or `[` (after
optional UTF-8 BOM and whitespace), parses with `serde_json::Value`,
and renders an outline of the form:

```
--- JSON shape (parsed by serde_json) ---
$ : object
$.data : array[24]
$.data[].country : string
$.data[].value : null|number   ← polymorphic; first 5 values: ["null", "null", "1234", "1100", "950"]
$.data[].date : string
$.meta.total : number

--- First 2 elements of $.data ---
[
  {"country":"...","value":null,"date":"2026"},
  ...
]
--- end JSON shape ---
```

Below the outline, the **raw bytes** (truncated to the remaining
budget) are kept. This is the key asymmetry vs. the PDF and HTML
branches — there, the rendered text fully replaces the raw bytes
because the rendered form *is* the parseable structure. For JSON,
the LLM may still need to read specific values to author a filter
expression (`$[1][?(@.value)].value`); the outline is a navigation
aid, not a replacement.

**Polymorphic-leaf annotation** is the principle that catches the
Session-32 World Bank null trap class: a path observed with both
`null` and `number` types, no container types in the union, gets
the `← polymorphic` marker plus a sample of leading values so the
LLM sees the leading-null pattern at authoring time and writes a
filter expression on the first attempt rather than a positional
index that lands on null forever.

**Per-section caps** (constants at top of the new section):

| Constant | Value | What it bounds |
|---|---|---|
| `JSON_OUTLINE_PATH_LIMIT` | 50 | distinct paths listed |
| `JSON_OUTLINE_SAMPLE_LIMIT` | 5 | leaf samples per polymorphic path |
| `JSON_OUTLINE_LEAF_PREVIEW_BUDGET` | 80 | bytes per sample preview |
| `JSON_OUTLINE_FIRST_ELEMENTS` | 2 | head-elements rendered as JSON |

Pathological JSON (10000-key flat object, 1 MiB string values, deeply
nested) cannot crowd the prefetch's overall byte budget.

### Item 6 — `json_path` authoring-time validation

**Status: no new validator code.** Patch 1's
`validate_recipe_against_bytes` dispatches through the closed
`ExtractionSpec` enum to `extract_json_path`, which carries the
runtime's null-skip and no-match contracts (Session 32). What patch
3 adds is a **scenario test**:
`validate_recipe_json_path_rejects_world_bank_leading_null_trap`
exercises the dispatch against a fixture that exhibits the canonical
class — a JSON array whose leading elements have `value: null` and a
positional-index recipe. The validator inherits the runtime's
all-nulls failure verbatim (the reason text names `null` and
suggests the filter-expression fix), and the test asserts both. It
also pins the **positive case** — the canonical filter-expression
fix validates cleanly against the same bytes — so the negative test
can't pass for the wrong reason.

The forward-pin test from Session 41 patch 1
(`validate_recipe_json_path_inherits_runtime_null_skip_contract`)
still pins the architectural intent.

### Plumbing

No plumbing changes. `prefetch_excerpt`'s return type is unchanged
from patch 1 (`Option<(String, Vec<u8>)>`).

### Prompt

Two principle-only edits in `config/prompts/recipe_author.md`:

1. The `json_path` bullet is extended with guidance to author
   against the entries listed under
   `--- JSON shape (parsed by serde_json) ---` and to treat the
   `← polymorphic` marker as the trigger to write a filter
   expression.
2. The `Document excerpt` section's framing paragraph is extended
   to cover the JSON case under the same principle: the outline is
   the runtime's view of the bytes (not a separate
   interpretation), and a path read off the outline is one the
   runtime will resolve unchanged. The asymmetry vs. PDF/HTML
   (outline above raw bytes, not in place of them) is named so the
   LLM knows where to look for specific values.

No source-specific routing was introduced. The
World Bank / OECD / Eurostat mention in the existing `json_path`
bullet is principle-anchored (it teaches the leading-null trap, it
doesn't say "if host is X").

## Tests

Unit tests in `fetch_executor.rs`:

- `is_json_recognizes_json_objects_and_arrays` — `{`, `[`, with
  leading whitespace / UTF-8 BOM.
- `is_json_rejects_non_json_payloads` — PDF, HTML, bare scalars
  (`42`, `"a"`, `true`, `null`), CSV, empty.
- `render_json_shape_surfaces_paths_types_and_array_cardinality`
  — root-object fixture; pins path/type lines and `array[N]`
  cardinality.
- `render_json_shape_annotates_polymorphic_leaf_with_samples` —
  the World-Bank-shaped fixture; pins `null|number` union, the
  `← polymorphic` marker, leading-null sample sequence, and
  presence of a real numeric value in samples.
- `render_json_shape_caps_path_count_at_limit` — flat object
  with > `JSON_OUTLINE_PATH_LIMIT` keys; pins the truncation
  marker.
- `render_json_shape_renders_first_elements_of_first_array` —
  pins the head-section header and that the third element does
  *not* appear (cap honored).

Integration test
`prefetch_excerpt_for_json_url_yields_shape_outline_to_recipe_author`
walks the full prefetch + propose-URL + recipe-author retry loop and
asserts the recipe-author prompt carries the outline header, the
`null|number` polymorphic annotation, and the raw bytes underneath.

Validator integration in `recipe_apply.rs`:
`validate_recipe_json_path_rejects_world_bank_leading_null_trap`.

All eight new tests pass; all pre-existing tests still pass.

## What to expect

Live runs against JSON-shaped sources should now show the LLM:

- **Authoring against polymorphic leaves correctly** — a leading-
  null path will surface the `← polymorphic` marker, prompting a
  filter-expression recipe on the first attempt.
- **Declining empty/landing-page JSON** more directly — when
  `serde_json` parses to a tiny structure with no relevant data,
  the outline makes that visible at the LLM's input layer.
- **Catching authoring mistakes at validation** — a positional-
  index recipe against a leading-null array converts to
  `AuthoringError::Declined` rather than persisting and failing on
  every fetch forever.

## Out of scope (still)

- Item 7 (xAI tier discipline) — patch 4.
- The PDF prefetch truncation gap — Session 43+, its own session.
- The `apps_common` test race — Session 43+, its own 1-line drive-by
  (`AtomicUsize` counter on the tempdir name).
- The Session 40 network-layer issues — Session 43+ per the handoff's
  hard rule.
