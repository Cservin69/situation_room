# ADR 0002 — Tauri + Svelte over Leptos / Rust-WASM

**Status**: Accepted
**Date**: 2026-04-19

## Decision

The frontend is built with Tauri + Svelte. Type safety across the IPC
boundary is preserved by generating TypeScript types from Rust structs via
ts-rs. Leptos is explicitly **not** used.

## Rationale

[To be written together. Covers the chart-ecosystem gap (uPlot, Observable
Plot, deck.gl have no WASM-native equivalents at production quality),
iteration speed during the panel-design phase, contributor surface for OSS,
and the revisit conditions: re-evaluate when Rust WASM charting reaches
parity, expected 2027–2028.]
