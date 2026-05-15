/**
 * Record summary formatters (Session 22).
 *
 * Each record type has a different shape; the record card needs to
 * surface a one-line summary from each. The DTOs carry `content` as
 * `unknown` (matching `RecipeDto`'s opacity convention), so this
 * module reaches into the JSON shape informally — the per-type
 * shapes are documented in `crates/core/src/schema/content.rs` and
 * `crates/core/src/schema/records/`.
 *
 * If a future session promotes content fields to typed DTOs, this
 * module's per-type shape assumptions become redundant — replace
 * with property access. Until then, `safeGet` handles the read with
 * a fallback rather than throwing.
 *
 * ## Why these formatters live here, not on the card component
 *
 * The card component is a pure render unit. Pulling the formatting
 * logic into a sibling module means the per-type shape knowledge
 * sits in one place — easy to update when the per-type DTOs harden.
 * It's also testable in isolation (the card isn't, without a Svelte
 * test harness).
 */
import type { ObservationDto } from './types/ObservationDto';
import type { EventDto } from './types/EventDto';
import type { EntityDto } from './types/EntityDto';
import type { RelationDto } from './types/RelationDto';
import type { DocumentDto } from './types/DocumentDto';
import type { AssertionDto } from './types/AssertionDto';

/**
 * Best-effort property read on an `unknown` value. Returns the
 * primitive at `key` if `obj` is an object and the key exists with
 * a primitive value; otherwise `undefined`. Used to read the
 * informally-known content shapes without committing the wire DTO
 * to a typed mirror.
 */
function safeGet(obj: unknown, key: string): unknown {
  if (obj && typeof obj === 'object' && key in obj) {
    return (obj as Record<string, unknown>)[key];
  }
  return undefined;
}

/**
 * Coerce `unknown` to a string for display. Number → toString,
 * string → as-is, anything else → empty string. Empty string
 * means "no displayable summary," and the caller decides whether
 * to fall back to a placeholder.
 */
function asDisplayString(v: unknown): string {
  if (typeof v === 'string') return v;
  if (typeof v === 'number') return String(v);
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  return '';
}

/**
 * One-line summary for an Observation: `metric: value unit`.
 * Examples: `voter_turnout: 77.1 %`, `production: 49000 t`.
 *
 * Falls back to `metric` alone if value/unit aren't readable, and
 * to a placeholder if metric isn't readable either (which would
 * indicate corrupt content — surface it rather than render an
 * empty card).
 */
export function summarizeObservation(o: ObservationDto): string {
  const metric = asDisplayString(safeGet(o.content, 'metric'));
  const value = asDisplayString(safeGet(o.content, 'value'));
  const unit = asDisplayString(safeGet(o.content, 'unit'));
  if (!metric) return '(observation)';
  if (!value) return metric;
  if (!unit) return `${metric}: ${value}`;
  return `${metric}: ${value} ${unit}`;
}

/**
 * One-line summary for an Event: prefer the headline; fall back to
 * `event_type` for older / minimally-populated content.
 */
export function summarizeEvent(e: EventDto): string {
  const headline = asDisplayString(safeGet(e.content, 'headline'));
  if (headline) return headline;
  const eventType = asDisplayString(safeGet(e.content, 'event_type'));
  return eventType || '(event)';
}

/**
 * One-line summary for an Entity: `canonical_name (kind)`.
 * Both fields are typed on the DTO, so this is the strongest
 * formatter — no JSON-path indirection.
 */
export function summarizeEntity(e: EntityDto): string {
  if (!e.canonical_name) return e.kind || '(entity)';
  return `${e.canonical_name} (${e.kind})`;
}

/**
 * One-line summary for a Relation: `kind: from → to`.
 */
export function summarizeRelation(r: RelationDto): string {
  const kind = asDisplayString(safeGet(r.content, 'kind'));
  const from = asDisplayString(safeGet(r.content, 'from'));
  const to = asDisplayString(safeGet(r.content, 'to'));
  if (!kind) return '(relation)';
  if (!from || !to) return kind;
  return `${kind}: ${from} → ${to}`;
}

/**
 * One-line summary for a Document: prefer the title; fall back to
 * the kind. Title is typed on the DTO.
 */
export function summarizeDocument(d: DocumentDto): string {
  if (d.title && d.title.trim().length > 0) return d.title;
  return d.kind || '(document)';
}

/**
 * One-line summary for an Assertion: `claimant {stance}`.
 * The actual content is dispatched on `content.asserted_kind`
 * (Session 78 — was `content.kind` until the duplicate-field
 * collision with `RelationContent.kind` surfaced), but for the
 * card-level summary the claimant + stance is the most identifying
 * pair.
 */
export function summarizeAssertion(a: AssertionDto): string {
  const stance = a.stance || 'asserted';
  const claimant = a.claimant || '(unknown)';
  return `${claimant} ${stance}`;
}
