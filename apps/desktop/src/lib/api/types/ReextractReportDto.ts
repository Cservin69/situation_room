// Session 92 — wire-shape mirror of
// `situation_room_pipeline::reextract::ReextractReport`. Hand-written
// (the Rust `ReextractReport` derives `Serialize` + `Deserialize` but
// not `TS`; the desktop binary doesn't run ts-rs over the pipeline
// crate). When the Rust shape gains a field, mirror it here.
//
// All counters are `u32` on the Rust side; the wire form is a plain
// number on the TS side. Zero counts surface as `0`, never `null` —
// the renderer treats every counter uniformly.

export interface ReextractReportDto {
  /**
   * Documents the gate (article-kind MIME + non-empty body)
   * accepted. Documents the pipeline already wouldn't extract from
   * are silently skipped before this counter ticks.
   */
  documents_considered: number;
  /**
   * Documents whose `source_id` failed to parse to a recipe owned by
   * this plan. Almost always pre-Session-22 rows or synth-path rows
   * that don't carry the canonical
   * `{source}#recipe:{uuid}@v{ver}` shape. Counted but skipped.
   */
  documents_unrouted: number;
  /**
   * Raw count of LLM-emitted Assertion drafts that survived the
   * validator across all per-Document passes (the sum of
   * `ExtractionReport.extracted` from each call).
   */
  assertions_extracted: number;
  /**
   * Of the extracted drafts, how many actually landed in storage
   * (sum of `ExtractionReport.persisted`).
   */
  assertions_persisted: number;
  /**
   * Per-Assertion `insert_assertion` failures across all
   * per-Document passes. Should be zero in steady state; non-zero
   * signals a storage-layer regression.
   */
  assertion_insert_failures: number;
  /**
   * Documents whose LLM call returned `Err(_)`. The orchestrator
   * absorbed the error and continued; this counter surfaces the
   * rate so the operator can spot a provider outage.
   */
  llm_call_errors: number;
}
