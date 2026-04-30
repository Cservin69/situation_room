# ADR 0002 — Tauri + Svelte over Leptos / Rust-WASM

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0001 (monorepo layout), ADR 0006 (design language)

## Context

Situation_room is a desktop application with a heavy visualization
surface — small multiples, sparklines, time-series charts, maps,
flow diagrams. The frontend choice has two dimensions: the shell
(how the app runs on the user's machine) and the UI framework
(how the actual panels are built).

The shell choice was effectively settled: Tauri offers a
Rust-native desktop shell with small binaries, no Electron bloat,
and good cross-platform support. Electron was never seriously
considered.

The framework question was real. Two credible options:

1. **Rust-native UI (Leptos / Dioxus / Yew).** The frontend is
   also Rust, compiled to WASM. Single language across the stack,
   rich type sharing between backend and frontend, no IPC
   serialization boundary (almost).
2. **Web-framework UI (Svelte / React / Solid).** The frontend is
   TypeScript, communicating with Rust via Tauri's IPC. Traditional
   web-stack tooling, traditional web-stack ecosystem.

The appeal of option 1 was real — "Rust all the way down" is an
aesthetically attractive property for a Rust-heavy project. We
rejected it anyway.

## Decision

**Tauri shell + Svelte (SvelteKit) frontend, with TypeScript types
generated from Rust structs via `ts-rs`.**

Leptos is explicitly not used. Rust-WASM UI frameworks are not
used for the panels.

The IPC boundary is typed in both directions:
- Rust `#[derive(TS)]` on types that cross the boundary.
- `ts-rs` generates `.ts` definitions consumed by the Svelte frontend.
- Tauri commands are enumerated in the `api` crate (per ADR 0001)
  and exposed to the webview with explicit allow-listing (per
  ADR 0009).

## Rationale

**The chart ecosystem is the decisive factor.** Situation_room renders
densely — uPlot for sparklines and time series at scale, Observable
Plot for analytical charts, deck.gl for maps and flows. None of
these have production-quality Rust-WASM equivalents as of this
writing. The Rust-WASM charting ecosystem (Plotters, egui_plot,
etc.) is functional but not in the same league for the
information-density surface Situation_room wants to render (see ADR
0006). Picking Leptos would mean either rebuilding those libraries
in Rust or paying WASM→JS interop costs on every render.

**Iteration speed during panel design.** The panel surface is going
to be iterated on heavily during Phase 4+. Design iterations in
Svelte are fast; iteration in WASM-compiled UI is slower (rebuild,
reload, bundle size). During the phase of the project where the
product is being shaped, the faster iteration loop matters more
than language uniformity.

**Contributor surface for OSS.** The Rust-WASM UI ecosystem has a
small contributor pool. Svelte has a large one. A project that
wants community contribution to its visual surface is better
served by the larger pool. "Rust all the way down" narrows the
contributor set to the intersection of "Rust developer" and "UI
developer," which is small.

**The IPC boundary isn't as costly as feared.** With `ts-rs`
generating types from Rust, the Rust-to-TypeScript boundary is
still mostly type-safe. Serialization cost is real but small —
for the data volumes Situation_room moves across IPC (per-panel
refresh, not streaming), it doesn't matter.

**Why Svelte over React.** Smaller runtime, compiled output
(fewer surprises about component re-renders), less boilerplate,
better fit for a dashboard-style UI than React's component
model. This is a softer preference than the others; React would
also work. The `svelte/motion` primitives are a good fit for the
ambient-kinetic aesthetic in ADR 0006.

## Alternatives considered

**Leptos.** The leading Rust-WASM framework. Rejected as above:
chart ecosystem, iteration speed, contributor surface. No
criticism of the framework itself — it's well-designed. Wrong
choice for *this* project.

**Dioxus.** Similar tradeoffs to Leptos. Same rejection reasoning.

**Yew.** Older Rust-WASM framework, less active. Ruled out
earlier in the consideration.

**React.** Credible alternative to Svelte. Rejected on runtime
size and fit; would not have been a wrong choice.

**Electron.** Not seriously considered. Binary size, memory
footprint, and security posture (contrast with ADR 0009) are all
worse than Tauri.

**Native desktop (egui / iced / gtk).** Rejected: losing the web
chart ecosystem entirely. The visualization surface is where
Situation_room competes; giving up the tools to render it well would
undercut the product.

## Revisit conditions

This decision should be revisited — seriously, not just
reaffirmed — when the Rust-WASM charting ecosystem reaches
production parity with the web stack. Rough expected window:
2027–2028. The specific thresholds:

- A sparkline/time-series library at uPlot's performance level
  (100k points at 60fps with interactive pan/zoom).
- An analytical charting library at Observable Plot's expressivity
  level (grammar-of-graphics composition, not just built-in
  chart types).
- A geospatial rendering library at deck.gl's level (WebGL-
  accelerated, handles the scales of trade-flow and shipping data
  Situation_room will use).

When all three exist in Rust-WASM, the tradeoff inverts —
language uniformity becomes a clear win, the IPC boundary
becomes pure overhead, and "Rust all the way down" becomes
practical. Not today.

## Consequences

**Positive**

- Chart ecosystem is the broadest possible.
- Panel iteration is fast.
- Contributor surface is larger.
- TypeScript frontend devs can contribute without learning Rust.

**Negative**

- Two languages, two toolchains, two build systems.
- IPC boundary is a real seam; every cross-boundary type needs a
  `TS` derive and regeneration when changed.
- If Rust-WASM ever becomes clearly better, migration cost is real
  (rewriting the frontend).

**Neutral**

- ts-rs-generated types mean the frontend can't invent its own
  data shapes; it consumes what Rust gives it. Good discipline,
  occasional friction when a frontend-only derived type would be
  convenient.

## Code references

- `apps/desktop/src-tauri/` — Tauri shell.
- `apps/desktop/src/` — SvelteKit frontend.
- `crates/api/` — Tauri command surface and ts-rs derives.
- Tauri capabilities config — allowed IPC commands (see ADR 0009).

## Review notes

Reviewed 2026-04-20. Codifies the Phase 1 framework decision. No
changes from what's shipped. The "revisit conditions" section is
new — previous-me's handoff flagged the 2027–2028 timeframe, and
putting the specific thresholds in writing means a future reader
can check them against ecosystem state rather than re-arguing from
scratch.
