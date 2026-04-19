# ADR 0006 — Design language

**Status**: Accepted
**Date**: 2026-04-19

## Decision

Stockpile's visual language follows information-density discipline (Tufte:
data-ink ratio, sparklines everywhere, small multiples) on a warm-charcoal
foundation, with surgical use of color and ambient kinetic moments.
Tokens live in `apps/desktop/src/lib/design/tokens.ts`; CSS variables in
`global.css` mirror them. Components import only from these.

## Principles

1. 80% of the screen is shades of charcoal; 20% carries all the signal.
2. Color means something — chrome and labels never get color.
3. Numbers are monospace, tabular-figured, right-aligned.
4. Animations are kinetic but ambient; never theatrical.
5. Anomalies pulse briefly then disappear; the contrarian/disagreement
   panel has a signature violet color.

## Rationale

[Full prose to be written together. Captures the conscious rejection of
the Bloomberg-clone aesthetic, why Tufte + ambient kinetic > "fancy
dashboards", and the discipline that prevents drift toward generic SaaS
look.]
