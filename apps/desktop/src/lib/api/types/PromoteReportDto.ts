// Session 82 — wire-shape mirror of
// `situation_room_pipeline::promote::PromoteReport`. Hand-written
// (the Rust `PromoteReport` derives `Serialize` + `Deserialize` but
// not `TS`; the desktop binary doesn't run ts-rs over the pipeline
// crate). When the Rust shape gains a field, mirror it here.
//
// All counters are `u32` on the Rust side; the wire form is a plain
// number on the TS side.

export interface PromoteReportDto {
  /** Total Assertion rows the promote pass saw. */
  assertions_considered: number;
  /**
   * Consensus pathway: distinct (content_hash, subject_hash) groups
   * that met quorum and were inserted this run.
   */
  groups_promoted: number;
  /**
   * Inserts skipped because the content-derived dedup_key already
   * existed in storage (idempotency hit). Includes both consensus and
   * authoritative-pathway rows.
   */
  skipped_already_promoted: number;
  /** Observation rows emitted this run (auth + consensus). */
  observations_emitted: number;
  /** Event rows emitted this run (auth + consensus). */
  events_emitted: number;
  /** Relation rows emitted this run (auth + consensus). */
  relations_emitted: number;
  /**
   * EntityAttribute-shaped Assertion rows emitted this run (the
   * promotion target for `AssertedContent::EntityAttribute`,
   * synthesised under the `agency:consensus` /
   * `agency:authoritative` claimants).
   */
  entity_attributes_emitted: number;
  /** Per-row insert failures that weren't dedup-key collisions. */
  insert_failures: number;
  /**
   * Session 82 — N=1 fast-track promotions whose claimant matched an
   * entry in `config/vocab/authoritative_sources.toml`. Counted in
   * the per-shape `*_emitted` totals above; this field surfaces how
   * many of those came from the authoritative pathway specifically.
   */
  authoritative_promoted: number;
}
