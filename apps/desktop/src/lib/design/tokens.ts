/**
 * situation_room design tokens.
 *
 * The visual language is documented in ADR 0006. The principles in short:
 *
 *   - 80% of the screen is shades of charcoal; the remaining 20% carries
 *     all the signal.
 *   - Color means something. Chrome and labels never get color; numbers and
 *     state changes do.
 *   - Data-ink ratio matters. Gridlines barely visible. Sparklines everywhere.
 *   - Numbers in monospace, right-aligned, so columns scan vertically.
 *   - Animations are kinetic but ambient; never theatrical.
 *
 * Edit these values to retheme. Components import only from this file.
 */

// ----- Color: a warm-charcoal foundation with surgical accents ----------

export const color = {
  // Background hierarchy — five steps of charcoal, warmer than pure black
  bg: {
    canvas:    '#0E1014',  // app background
    panel:     '#161A21',  // panel surface
    panelAlt:  '#1B2029',  // alternating rows, hover backgrounds
    inset:     '#10131A',  // inset sections inside panels (sparkline beds, etc.)
    overlay:   'rgba(14, 16, 20, 0.85)', // modals, command palette
  },

  // Foreground hierarchy — five steps from primary text to barely-there chrome
  fg: {
    primary:   '#E8EBF0',  // numbers, headlines
    secondary: '#A8AFBC',  // labels, secondary text
    tertiary:  '#6B7280',  // axis ticks, units, less-important metadata
    quaternary:'#3F4654',  // gridlines, dividers (barely visible)
    inverse:   '#0E1014',  // text on accent backgrounds
  },

  // Borders — thin and quiet
  border: {
    subtle:    '#1F2530',  // standard panel borders
    strong:    '#2C3340',  // emphasized borders (focused panel)
    accent:    '#3D4555',  // borders on interactive states
  },

  // Semantic — the 20% that carries signal.
  // These are saturated but not loud; chosen to read as "important" not "alarming".
  signal: {
    positive:  '#5BC685',  // confident green; up moves, supply-positive events
    negative:  '#E5604A',  // attention-needed red; down moves, supply-negative
    warning:   '#E0A52E',  // amber; data quality issues, stale data
    info:      '#5B9CE6',  // muted blue; neutral notifications
    contrarian:'#B57FE5',  // muted violet; the disagreement-panel signature color
  },

  // Anomaly severity colors — match signal palette but slightly desaturated
  anomaly: {
    info:      '#4A7AB8',
    notable:   '#C29024',
    high:      '#D45438',
    critical:  '#E5604A',
  },

  // Confidence indicator dots
  confidence: {
    high:      '#E8EBF0',  // filled, bright
    medium:    '#A8AFBC',  // filled, muted
    low:       'transparent', // outline only
  },
} as const;

// ----- Typography ------------------------------------------------------

export const font = {
  // Numbers, code, IDs — anything that benefits from columnar alignment
  mono: '"JetBrains Mono", "Berkeley Mono", "SF Mono", Menlo, monospace',
  // UI chrome, labels, prose
  sans: 'Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif',
  // Display — used sparingly for screen titles
  display: 'Inter, -apple-system, sans-serif',
} as const;

export const fontSize = {
  xxs: '10px',  // axis ticks, source-count badges
  xs:  '11px',  // labels, secondary metadata
  sm:  '12px',  // body text, panel content
  md:  '13px',  // primary numbers in panels
  lg:  '15px',  // panel titles
  xl:  '18px',  // featured numbers (price strip)
  xxl: '24px',  // screen titles
  display: '32px', // hero numbers (e.g. spot price in price strip)
} as const;

export const fontWeight = {
  regular: 400,
  medium:  500,
  semibold:600,
  bold:    700,
} as const;

// ----- Spacing — 4px base scale ----------------------------------------

export const space = {
  px1: '4px',
  px2: '8px',
  px3: '12px',
  px4: '16px',
  px5: '20px',
  px6: '24px',
  px8: '32px',
  px10:'40px',
  px12:'48px',
} as const;

// ----- Layout grid — Bloomberg-style addressable cells -----------------

export const grid = {
  // Default panel grid: 12 columns, variable rows
  cols: 12,
  gap: '8px',
  // Panels addressed by coordinate (A1, B3) for keyboard navigation
  cellMinHeight: '160px',
} as const;

// ----- Motion — kinetic but ambient ------------------------------------

export const motion = {
  // Standard easing — feels considered, not abrupt
  ease: 'cubic-bezier(0.4, 0, 0.2, 1)',
  // Ambient animations — slow, calm
  durationAmbient: '1200ms',
  // UI feedback — quick but not snappy
  durationUI: '180ms',
  // Anomaly pulse — brief attention-grab, then disappear
  durationPulse: '600ms',
} as const;

// ----- Borders & radii -------------------------------------------------

export const radius = {
  sm: '2px',
  md: '4px',
  lg: '6px',
  panel: '4px',
} as const;

export const borderWidth = {
  hairline: '0.5px',  // gridlines, dividers
  thin:     '1px',    // panel borders
  medium:   '2px',    // focused panel border
} as const;

// ----- Indicator language ----------------------------------------------

/**
 * Significance-tinted directional arrows. Returns the arrow character and
 * a color, scaled by the magnitude of the change.
 */
export function directionalArrow(pctChange: number): { char: string; color: string } {
  const abs = Math.abs(pctChange);
  if (abs < 0.1) return { char: '·', color: color.fg.quaternary };
  const char = pctChange > 0 ? '▲' : '▼';
  const base = pctChange > 0 ? color.signal.positive : color.signal.negative;
  // Below 0.5%, washed out; above 3%, fully saturated
  if (abs < 0.5) return { char, color: color.fg.tertiary };
  if (abs < 3.0) return { char, color: color.fg.secondary };
  return { char, color: base };
}

/** Confidence dot count for an Assertion */
export function confidenceDots(confidence: number): { filled: number; total: 3 } {
  const total = 3 as const;
  if (confidence >= 0.8) return { filled: 3, total };
  if (confidence >= 0.5) return { filled: 2, total };
  if (confidence >= 0.3) return { filled: 1, total };
  return { filled: 0, total };
}
