# ADR 0006 — Design language

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0002 (Tauri + Svelte)

## Context

Situation_room's product is a research workstation. The user spends
hours in front of it reading numbers, cross-referencing claims,
and looking for anomalies. The visual language has to support
that mode of use — high information density, low friction, no
cognitive overhead on the chrome — and it has to do so without
looking like every other SaaS dashboard on the market.

The space of existing analytical UIs falls into two broad camps:

1. **The Bloomberg-clone.** Maximum information density,
   orange-on-black terminal aesthetic, blinking fields, every
   surface carrying data. Emotionally: "serious tool for serious
   people." Risks: visually oppressive, signals importance
   through volume rather than through information.
2. **The generic modern SaaS dashboard.** Spacious, rounded
   corners, soft gradients, illustration-rich empty states,
   airy typography. Emotionally: "friendly, approachable." Risks:
   indistinguishable from CRM tools, marketing dashboards, and
   fitness apps; low information density per screen; signals
   polish but not substance.

Neither serves situation_room's user. A commodities analyst tracking
lithium prices does not need illustrated empty states and does
not need orange-on-black nostalgia. They need a lot of numbers
that are legible, small multiples they can scan, sparklines
everywhere, and a signal when something is unusual.

## Decision

situation_room's visual language is:

**Tufte information-density discipline on a warm-charcoal
foundation, with surgical color use and ambient kinetic moments.**

Five principles, enforced via the design-token system and via
code review on new components:

1. **80% of the screen is shades of charcoal; 20% carries all
   the signal.** Chrome is quiet; data is bright. A user scanning
   the screen should be able to locate the numbers by contrast
   alone.
2. **Color means something.** Categorical assignments are reserved
   for categorical data (anomaly severity, disagreement signal,
   positive/negative delta). Chrome, labels, and borders never get
   color. If a color appears, it carries information.
3. **Numbers are monospace, tabular-figured, right-aligned.**
   Comparing two numbers vertically requires digit alignment. This
   is not a style preference; it's a legibility requirement.
4. **Animations are kinetic but ambient; never theatrical.** A
   new value fades in over 200ms. A loading state has subtle
   motion. No spinners, no skeleton shimmers, no celebratory
   transitions. The user is not being entertained; they are
   working.
5. **Anomalies pulse briefly then disappear. Disagreement has a
   signature color.** When a detector fires, the affected cell
   pulses for a few seconds and then returns to baseline — the
   anomaly is in the audit log, not tattooed on the UI. The
   disagreement / contrarian panel gets a signature violet that
   no other surface uses.

### The tokens

Design tokens live in `apps/desktop/src/lib/design/tokens.ts`.
CSS variables in `apps/desktop/src/app.css` (or `global.css`)
mirror them. **Components import only from these.** A component
that hardcodes a hex color or a pixel value is a code-review
bug.

The tokens are the enforcement mechanism. A new contributor who
doesn't know the design philosophy can still produce compliant
components by using tokens. Drift happens when tokens get
bypassed, not when they're used.

## Rationale

**Why Tufte.** Edward Tufte's data-ink ratio principle (maximize
the proportion of ink used to display data, minimize ink used on
decoration) maps cleanly onto screen pixels. Small multiples,
sparklines, and dense tables are the native medium of an
analytical workstation. The principles are old and well-tested;
this is not design novelty.

**Why warm charcoal, not black.** Pure black (`#000`) on a
modern display is surgical and cold. Warm charcoal (a slight
brown-red tint in the dark end of the palette) is easier on the
eye over long sessions. The intended use is multi-hour focus,
not a quick glance, so fatigue matters.

**Why surgical color.** If every surface has a color, no surface
signals anything. The discipline of reserving color for
categorical-and-meaningful use means that when a user sees
color, they know to look. The disagreement panel's violet is
the clearest case: a user who has internalized the palette knows
that violet means "the sources don't agree," without reading a
label.

**Why ambient kinetic and not static.** A pure-static UI for
real-time data feels dead when new values arrive — the user
misses the update, or has to actively verify nothing changed.
Ambient motion (a fade-in on new data, a subtle pulse on
anomalies) communicates liveness without demanding attention.
The counter-model is Slack-style notifications: theatrical,
demanding, exhausting. situation_room is the opposite.

**Why the "80/20" rule is stated as a percentage.** Precision
forces discipline. "Mostly charcoal, some color" is a vibe;
"80% charcoal, 20% signal" is a design check. A reviewer looking
at a new panel can ask "is this roughly 80/20 or has color crept
into the chrome?" and get a clear answer.

**Why tokens are the enforcement mechanism.** Hand-coded hex
values drift. Token names (`color-text-default`,
`color-signal-positive`, `spacing-panel-gutter`) are stable and
meaningful; components built from tokens inherit the design
discipline without each author having to re-derive it.

## Alternatives considered

**Bloomberg-clone aesthetic.** Rejected: signals seriousness
through volume, not through information. situation_room wants to
be quiet and clear, not loud.

**Modern SaaS dashboard.** Rejected: looks like every other
product, undercuts the "this is a serious analytical tool"
positioning. Also low-density, which fights the product.

**Pure light mode.** Rejected: long analytical sessions on a
bright background are more fatiguing than dark charcoal. Not a
strong preference — a light-mode variant may appear later — but
the default is dark.

**Pure dark mode (black).** Rejected: warmer charcoal is easier
on the eye over multi-hour sessions.

**No motion at all.** Rejected: real-time data needs liveness
signals. Ambient motion is the compromise between "dead UI" and
"distracting theatrics."

**Color-coded everything (Bloomberg-style).** Rejected:
information saturation that drowns the actual signal.

## Consequences

**Positive**

- Information density is high; a user can see a lot of a
  research session at once.
- Color carries meaning; anomalies and disagreements are
  visible at a glance.
- The look is distinctive — situation_room doesn't look like every
  other dashboard.
- Tokens enforce consistency without per-component discipline.

**Negative**

- The aesthetic is polarizing. Some users will want more
  color, more spacing, softer visuals. We accept this; the
  target user is the one who appreciates the discipline, not
  the one who wants friendly.
- Tufte-style density is harder to design for than card-based
  modern UIs. New panels take more thought. We accept this;
  it's the reason the product is differentiated.

**Neutral**

- The signature violet for disagreement is a brand-level choice.
  If the brand changes, the specific color does; the principle
  ("disagreement has a signature color") does not.
- Ambient motion implies a motion library in the frontend
  (likely `svelte/motion`); small bundle cost, worth it.

## Code references

- `apps/desktop/src/lib/design/tokens.ts` — design tokens.
- `apps/desktop/src/app.css` — CSS variables mirroring tokens.
- Component library (to grow during Phase 4) — imports tokens,
  never hardcodes values.

## Review notes

Reviewed 2026-04-20. Codifies the Phase 1 visual-language choices.
The five principles were already implicit in the tokens file; this
ADR makes them explicit so future contributors to the component
library have a stated standard rather than an inferred one.

The key commitments — Tufte density, warm charcoal, surgical color,
ambient kinetic, disagreement-violet — are not revisited and do not
need to be. A light-mode variant and a higher-density
"professional" mode may appear later as user options; neither
changes this ADR's defaults.
