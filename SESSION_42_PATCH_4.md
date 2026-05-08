# Session 42 — Patch 4 of 4

Item 7 from the Session 42 handoff: xAI tier discipline pass.

## Apply

```
cd ~/Documents/Claude/Projects/SituationRoom
cargo build --workspace
cargo test --workspace
```

Comment-only change; build is ~0.3s incremental from patch 3.

## Files changed

- `crates/llm/src/providers/grok.rs` — `XaiConfig::default()` comment
  block rewritten. No functional change to model strings.

## Why no model-string change

The Session 42 handoff observed that all three tiers point at
`grok-4.3` in the boot logs and asked for "the right cheap /
workhorse / frontier mapping" after a web search. The web search
(2026-05-08) found that **xAI consolidated their lineup in May
2026**:

- Eight legacy models — `grok-4-fast`, `grok-4-0709`, `grok-3`,
  `grok-code-fast-1`, the `grok-4-1-fast` variants,
  `grok-imagine-image-pro`, others — retire **2026-05-15 12:00 PT**
  (seven days from this patch).
- Surviving on the API: `grok-4.3` (1M ctx, $1.25/$2.50 per million
  doubling above 200K) and `grok-4.20` (long-context, 2M ctx,
  $2/$6 — *more* expensive than 4.3, not cheaper).

xAI's official guidance is that all callers should use `grok-4.3`.
The cost lever they now expose is `grok-4.3`'s **reasoning intensity**
(low / medium / high) — a request *parameter*, not a model name.

So there is no current xAI model string that meaningfully cost-
differentiates a cheap tier from a frontier tier the way our
`ModelTier` enum implies. Keeping `frontier=grok-4.3
workhorse=grok-4.3 cheap=grok-4.3` is xAI's actual recommendation
post-consolidation, not config drift. The patch rewrites the
default's comment block to say so explicitly, so the boot-log line
reads as intentional rather than oversight.

## What this means for cheap-tier cost

The handoff's complaint — cheap-tier propose-URL pays the same
per-token rate as a frontier-tier authoring call — is real and
**not solved by patch 4**. Its actual fix is architectural:

> Plumb a `reasoning_effort: Low | Medium | High` field through
> `XaiConfig` and `CompletionRequest`, mapped per-tier in the
> request body. xAI's `chat/completions` accepts a per-request
> reasoning intensity parameter; routing cheap-tier calls at low
> effort and frontier-tier calls at high effort would deliver the
> cost differentiation a model-string swap can't.

That belongs in its own session. It is captured in
`STOCKPILE_HANDOFF_SESSION43.md`.

Until then, operators can override per-tier via `XAI_CHEAP_MODEL` /
`XAI_WORKHORSE_MODEL` / `XAI_FRONTIER_MODEL` (already wired in
`XaiConfig::from_env`) when xAI rebrands again.

## No test changes needed

Existing tests use literal `f` / `w` / `c` strings or fixture model
names (`grok-4-1-fast-reasoning` in mock response payloads, testing
parser behavior) — none depend on the actual default values. Comment
edits don't alter behavior.

## Out of scope (carried into Session 43)

- `reasoning_effort` plumbing — the actual fix for cost-tier
  differentiation. Architectural, not config.
- The PDF prefetch truncation gap.
- The `apps_common` test race.
- The Session 40 network-layer issues.
