# ADR 0020 — Anthropic request-side `cache_control` breakpoints

**Status**: Accepted (Session 75). Symmetrises Session 72's xAI
prompt-caching plumbing across the second concrete provider.
**Date**: 2026-05-15 (proposed and accepted same session)
**Related**: ADR 0007 (research function: closed extraction
vocabulary), ADR 0009 (security posture: one HTTP client; ADR 0009
§"The rule" applies — no SDK), Session 72 memory
`project_sr_session_72_caching` (xAI `x-grok-conv-id` + cached_tokens
projection), Session 74 memory `project_sr_session_74_landing`
(`cached_input_tokens` field on `CompletionResponse`, v1.22 prompt
restructure).

---

## Context

Session 72 plumbed xAI prompt-caching: `x-grok-conv-id` header for
server routing, plus a `prompt_tokens_details.cached_tokens`
projection on the response. Session 74 then surfaced
`cached_input_tokens: Option<u32>` on every `CompletionResponse`,
parsed Anthropic's `cache_read_input_tokens` field into it on the
**read** side, and noted in code comments that the **write** side
(emitting `cache_control` breakpoints on requests) was follow-on
work.

Without the write side, the Anthropic projection on `LedgerEntry`
is always zero — the parser reads what the server reports, the
server only reports a non-zero value when at least one
`cache_control: {type: "ephemeral"}` block is on the request, and
no callsite emits one today.

## Decision

The Anthropic provider's `build_body` now declares two breakpoint
positions whenever the call shape allows it:

1. **`tools[0].cache_control = {type: "ephemeral"}`** — every
   structured-output call already declares a single forced tool
   carrying the caller's JSON Schema. The schema is identical
   across every call in the same authoring campaign, so this is a
   free cache lever for the structured-output path.

2. **User-content prefix block** — when `req.user` contains the
   literal marker `## Concrete inputs`, `build_body` splits the
   user turn at that marker into two text blocks:
   - prefix (everything before the marker) — marked
     `cache_control: ephemeral`
   - tail (marker and everything after) — uncached

   When the marker is absent, the user turn ships as a plain
   JSON string (legacy wire shape, byte-for-byte unchanged).

The marker matches the v1.22 recipe-author prompt restructure
(Session 74). Any other prompt that adopts the same trailing
`## Concrete inputs` heading gets the same lever for free; any
prompt that doesn't, ships unchanged.

## Why not put `cache_control` on the system field

The production system text on the recipe-author callsite is ~20
tokens — well below Anthropic's documented minimum cache prefix
size (1024 input tokens). A breakpoint on a sub-threshold prefix
is silently ignored by the server: harmless but performative.
Converting `system` from a plain string to the array-of-blocks
form just to attach a no-op `cache_control` would change the wire
shape for no observable benefit.

If a future caller adds a much larger system text, the conversion
becomes worthwhile; `build_body` makes that change a 5-line edit
without touching anything downstream.

## Why split on a literal marker rather than a token count

A token-count split would require either bundling a tokenizer (a
non-trivial dep) or making a call into the provider to count
tokens (a network round-trip on every authoring call). Both
options trade architectural complexity for a hypothetical edge
case — the v1.22 prompt restructure already factored the cacheable
prefix structurally, and a documentation change that moves the
marker is a code-review-visible event in the same commit as the
prompt edit.

The marker-based split also keeps `build_body` deterministic and
pure: same input → same output, no external state, easy to unit
test (and tested — see the four `build_body_user_content_*` tests
plus the `build_user_content_with_cache_breakpoint_unit` direct
test in `anthropic.rs`).

## What the closed-vocabulary discipline says

The decision rule is purely structural: "does the user text
contain this byte sequence?" No host, no model, no plan, no
domain-specific routing. ADR 0007's closed-vocabulary discipline
holds; the marker is the only string-comparison done in the path,
and it's a documentation-aligned constant, not a runtime input.

## What this does NOT do

- **It does not guarantee cache hits.** The wire shape only
  declares "this prefix is eligible for caching." The Anthropic
  server decides whether it has matching bytes in its cache pool.
  The Session-75 cost-by-tier dashboard tile surfaces the
  observed hit ratio so the operator can see whether the
  declaration is biting in practice.
- **It does not change response handling.** The parser already
  reads `usage.cache_read_input_tokens` (Session 74); the
  cost-ledger already records it (Session 75 piece 1). The
  request-side change here makes that read-side surface
  populate-able for Anthropic in the same way Session 72 made
  xAI's populate.
- **It does not add a knob.** No per-call `cache_control` field
  on `CompletionRequest`. The provider decides — same shape as
  the rest of the trait (the trait doesn't carry provider-
  specific wire knobs; ADR 0009 §"Provider-agnostic surfaces").
- **It does not target the classifier or the propose-URL
  prompts.** Both are smaller than the recipe-author prompt and
  do not (yet) carry the `## Concrete inputs` marker. They ship
  the legacy plain-string user content shape and pay the
  uncached cost. If a future prompt edit adopts the marker, the
  cache lever activates automatically.

## Verification

The verification path mirrors Session 74's xAI one. Operator
boots the desktop binary with `LLM_PROVIDER=anthropic`,
classifies a fresh plan, accepts it, runs fetch. The new
`CostByTierPanel` on the dashboard shows the
`(anthropic, frontier)` row; on the **second** authoring call
against any source whose prompt shares the v1.22 stable prefix,
the cache-hit chip should flip from "0%" (cold) to a positive
percentage. Pre-Session-75 baseline is `cache_read_input_tokens`
always `None` → "—" chip; post-Session-75 the chip carries a
number.

If the ratio stays "—" or "0%" after a second authoring call,
the most likely cause is that the prefix is below Anthropic's
minimum cacheable prefix size (1024 tokens at the time of this
ADR). Pin the operator's call to a recipe-author prompt rather
than the classifier/propose-URL ones; the recipe-author prompt
is orders of magnitude above the threshold.
