// Session 93 — wire-shape mirror of
// `situation_room_pipeline::cull::CullReport`. Hand-written for the
// same reason as `ReextractReportDto`: the Rust struct derives
// Serialize/Deserialize but not TS, and the desktop binary doesn't
// run ts-rs over the pipeline crate.
//
// All counters are u32 on the Rust side; numbers on the wire. Zero
// counts surface as 0, never null — the renderer treats every counter
// uniformly.

export interface CullReportDto {
  /** Assertions visited by the scan. */
  assertions_considered: number;
  /** Assertions whose source Document couldn't be located inside the
   *  plan's record set — skipped. */
  assertions_unrouted: number;
  /** Assertions deleted because their source Document scored Index. */
  assertions_culled: number;
  /** Assertions kept because their source Document scored Article. */
  assertions_kept_article: number;
  /** Assertions kept because their source Document scored Unknown
   *  (non-HTML MIME, sparse body, non-UTF-8 bytes). */
  assertions_kept_unknown: number;
  /** Per-Assertion `delete_assertion` errors. Should be zero in
   *  steady state. */
  delete_failures: number;
}

/**
 * Read-only preview item for the cull pass. The frontend renders a
 * list of these before the operator confirms the destructive call,
 * so the COST-WARNING discipline ("never delete without showing what
 * would go") holds at the UI layer.
 */
export interface CullPreviewItemDto {
  /** Assertion id (UUID string). */
  assertion_id: string;
  /** content_kind discriminator — `observation`, `event`, `relation`,
   *  `entity_attribute`. */
  content_kind:
    | 'observation'
    | 'event'
    | 'relation'
    | 'entity_attribute';
  /** Source Document's hostless path (e.g. `/topic/aluminium`),
   *  truncated to 80 codepoints. The host is omitted so log fields
   *  and the preview list don't carry host strings into the UI. */
  source_path: string;
  /** The detector's verdict label — always `"index"` for items the
   *  preview surfaces. */
  detector_signal: 'index';
}
