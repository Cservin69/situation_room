//! Entity storage: insert + get.
//!
//! Entities differ from the other record types: they have no `content`
//! JSON column — the business data is `entity_id` + `kind` +
//! `canonical_name` + optional `geometry`, stored as flat columns.
//! They also have no `dedup_key` — the `entity_id` is the business key
//! with a UNIQUE constraint.

use chrono::Utc;
use duckdb::params;
use situation_room_core::vocab::EntityId;
use situation_room_core::Entity;
use tracing::warn;
use uuid::Uuid;

use crate::connection::Store;
use crate::entity_refresh_log::{EntityRefreshEvent, ENTITY_REFRESH_LOG_CAP};
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

// ---------------------------------------------------------------------------
// Session 98 candidate #5 — Entity-provenance tier ordering
// ---------------------------------------------------------------------------
//
// Three independent pipelines emit Entity rows today:
//
//   - **entity_synth** (Sn-76, plan-accept-time exemplar materialisation)
//     stamps `provenance.license = "classifier-emitted"` and
//     `canonical_name = humanise(entity_id_slug)`. The slug-humanised
//     display name is a best-effort placeholder until a richer source
//     names the entity properly.
//   - **per-Document Entity extractor** (Sn-97 Lever A) stamps
//     `provenance.license = "extracted"` and `canonical_name =
//     LLM-emitted`. The LLM-emitted form is the best source for a
//     display name (e.g. "Panasonic Energy Corporation" vs the
//     humanised "panasonic energy").
//   - **recipe-emitted iterator rows** (Sn-97 Lever B, via
//     `recipe_apply::build_record`) stamp
//     `provenance.license = "unknown"` (the recipe-apply default;
//     ingest never overrides it for Entity rows in v1) and
//     `canonical_name = recipe-extracted scalar`.
//
// Tier ordering for refresh decisions (HIGHER = wins refresh on
// conflict). Picked so the operator sees the best display name we
// have access to:
//
//   DocumentExtracted > SlugHumanised > RecipeIterator > Unknown
//
// Closed-vocabulary discipline: the license values above are the
// only three we emit today. `Unknown` is the catch-all for license
// strings we don't recognise (defensive — refresh stays a no-op in
// that case rather than silently overwriting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntityProvenanceTier {
    /// Lowest precedence — we don't recognise the license string and
    /// don't want to silently overwrite a richer row.
    Unknown,
    /// `recipe_apply` default (`license = "unknown"`). Iterator rows
    /// from list-page recipes; canonical_name is the recipe-extracted
    /// scalar (often a slug or a noisy label).
    RecipeIterator,
    /// Sn-76 `entity_synth` exemplar (`license = "classifier-emitted"`).
    /// canonical_name is the humanised slug — useful placeholder until
    /// something better arrives.
    SlugHumanised,
    /// Sn-97 per-Document Entity extractor (`license = "extracted"`).
    /// canonical_name is LLM-emitted prose — the strongest signal we
    /// have today for display naming.
    DocumentExtracted,
}

/// Map a `provenance.license` string to its tier. Closed-vocab; the
/// three recognised strings are the only ones the entity-emitting
/// pipelines stamp today. Unknown strings fall through to
/// [`EntityProvenanceTier::Unknown`].
pub fn entity_tier_from_license(license: &str) -> EntityProvenanceTier {
    match license {
        "extracted" => EntityProvenanceTier::DocumentExtracted,
        "classifier-emitted" => EntityProvenanceTier::SlugHumanised,
        "unknown" => EntityProvenanceTier::RecipeIterator,
        _ => EntityProvenanceTier::Unknown,
    }
}

impl Store {
    pub fn insert_entity(&self, ent: &Entity) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&ent.envelope)?;
        let geometry_json = match &ent.geometry {
            Some(g) => Some(serde_json::to_string(g)?),
            None => None,
        };

        tx.execute(
            "INSERT INTO entities (
                id, entity_id, kind, canonical_name, geometry,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence,
                selector_path, raw_bytes_excerpt
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                ent.id,
                ent.entity_id.as_str(),
                ent.kind,
                ent.canonical_name,
                geometry_json,
                cols.source_id,
                cols.source_url,
                cols.source_published_at,
                cols.license,
                cols.tags_json,
                cols.subject_time_json,
                cols.observed_at,
                cols.valid_at,
                cols.confidence,
                cols.selector_path,
                cols.raw_bytes_excerpt,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        crate::envelope_io::insert_subjects_and_derivation(
            &tx,
            ent.id,
            "entity",
            &ent.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    pub fn get_entity(&self, id: Uuid) -> Result<Entity> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        #[allow(clippy::type_complexity)]
        let (row_id, entity_id_s, kind, canonical_name, geometry_json, raw): (
            Uuid,
            String,
            String,
            String,
            Option<String>,
            EnvelopeRow,
        ) = conn
            .query_row(
                "SELECT id, entity_id, kind, canonical_name, geometry,
                        source_id, source_url, source_published_at,
                        license, tags, subject_time, observed_at, valid_at, confidence,
                        selector_path, raw_bytes_excerpt
                 FROM entities
                 WHERE id = ?",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        EnvelopeRow {
                            source_id: r.get(5)?,
                            source_url: r.get(6)?,
                            source_published_at: r.get(7)?,
                            license: r.get(8)?,
                            tags_json: r.get(9)?,
                            subject_time_json: r.get(10)?,
                            observed_at: r.get(11)?,
                            valid_at: r.get(12)?,
                            confidence_f: r.get(13)?,
                            selector_path: r.get(14)?,
                            raw_bytes_excerpt: r.get(15)?,
                        },
                    ))
                },
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("entity {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;
        let entity_id = EntityId::new(entity_id_s)
            .map_err(|e| StorageError::Other(format!("entity_id round-trip: {e}")))?;
        let geometry = match geometry_json {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        };

        Ok(Entity {
            id: row_id,
            entity_id,
            kind,
            canonical_name,
            geometry,
            envelope,
        })
    }

    /// Idempotent insert with tier-aware refresh on conflict (Sn-98).
    ///
    /// Behaviour:
    ///   - **Row absent** → fresh insert via [`Self::insert_entity`].
    ///     Returns `Ok(())` (matches the Sn-97 first-write-wins
    ///     contract for the common case).
    ///   - **Row present and incoming tier ≤ existing tier** → no-op.
    ///     Bias toward keeping the richer display name (e.g. a
    ///     recipe-iterator scalar doesn't overwrite an LLM-extracted
    ///     "Panasonic Energy Corporation").
    ///   - **Row present and incoming tier > existing tier** →
    ///     refresh `kind`, `canonical_name`, and the provenance
    ///     license. The row's `id` and `entity_id` stay stable so
    ///     `record_subjects_entities` / `record_derived_from` joins
    ///     keep pointing at the same record.
    ///
    /// ## Tier ordering (Sn-98 #5)
    ///
    /// `DocumentExtracted > SlugHumanised > RecipeIterator > Unknown`
    ///
    /// See [`EntityProvenanceTier`] for the mapping. The ordering is
    /// picked so the operator sees the best display name available:
    ///   - Lever A (LLM-extracted prose name) refreshes Sn-76 slugs,
    ///   - but Sn-76 slugs DON'T overwrite Lever A names later,
    ///   - and Lever B iterator scalars never overwrite either.
    ///
    /// ## Why not full envelope refresh
    ///
    /// We deliberately leave `record_subjects_*` / `record_derived_from`
    /// rows untouched: those carry per-source provenance pointers, and
    /// rewriting them would either lose history or duplicate it.
    ///
    /// Sn-100 #5 extends the refresh to cover the **flat provenance
    /// columns** on the `entities` row itself: `source_id`,
    /// `source_url`, `source_published_at`, `selector_path`,
    /// `raw_bytes_excerpt`, alongside the existing `kind` /
    /// `canonical_name` / `license`. Before this change, a Lever A
    /// refresh updated the row's name but left `source_id` pointing at
    /// the original Sn-76 `plan:{plan_id}#entity_exemplar` source —
    /// the dashboard's "where did this name come from" answer cited
    /// the classifier slug while the name itself came from the
    /// Document. Updating the flat columns aligns the per-row
    /// provenance with the post-refresh tier without touching the
    /// junction tables.
    ///
    /// Session 97 Lever B — the recipe-driven Entity production path
    /// (iterator-bearing recipes against `entity_kind` expectations)
    /// reaches storage through [`Store::insert_record`] on
    /// `Record::Entity`. Iterator runs re-emit the same entity_ids on
    /// every refetch, and the entity_id UNIQUE constraint would
    /// otherwise turn the second run into a recipe failure. This
    /// method mirrors the existence-check pattern Sn-76's
    /// `entity_synth::try_materialize_one` uses for the same
    /// idempotency reason; the tier-aware refresh extends that.
    ///
    /// ## Session 99 #5 — back-compat shim
    ///
    /// This method now derives the incoming tier from the license
    /// string and delegates to [`Self::upsert_entity_with_tier`]. The
    /// tier-detection failure mode the Sn-98 handoff flagged — a
    /// future pipeline stamps `license="extracted"` without being
    /// structurally Lever A — is closed by migrating the three known
    /// call sites onto the explicit-tier path. This shim stays for
    /// tests + any non-Sn-97/98/99 call site that hasn't been
    /// migrated yet; production emitters should pass an explicit
    /// tier so the closed-vocab signal is structural, not textual.
    ///
    /// When the license string falls outside the closed three (Sn-98
    /// `entity_tier_from_license` maps it to `Unknown`), the shim
    /// emits a WARN log naming the offending string — same posture
    /// the storage layer takes for unknown promote-trigger strings.
    pub fn upsert_entity(&self, ent: &Entity) -> Result<()> {
        let derived = entity_tier_from_license(&ent.envelope.provenance.license);
        if derived == EntityProvenanceTier::Unknown {
            warn!(
                entity_id = %ent.entity_id.as_str(),
                license = %ent.envelope.provenance.license,
                "upsert_entity (shim): license string outside the closed-vocab \
                 set; refresh will not fire — call upsert_entity_with_tier with \
                 an explicit EntityProvenanceTier to opt in"
            );
        }
        self.upsert_entity_with_tier(ent, derived)
    }

    /// Authoritative idempotent upsert. Sn-99 #5 makes the incoming
    /// tier an explicit closed-vocab parameter rather than deriving
    /// it from the license string at the storage boundary.
    ///
    /// **Why explicit tier.** Sn-98 #5 keyed the refresh decision off
    /// `entity_tier_from_license(license)`. That works today because
    /// the three emitting pipelines stamp distinct license strings,
    /// but it makes the license column structurally load-bearing —
    /// a future pipeline that stamps `license="extracted"` without
    /// being Lever A would silently inherit the
    /// [`EntityProvenanceTier::DocumentExtracted`] tier and start
    /// overwriting Lever A names. The Sn-98 handoff flagged this
    /// (`feedback_no_easy_wins`-shaped failure mode). Threading the
    /// tier as a typed argument keeps the closed-vocab signal at the
    /// call site where the pipeline's identity is known.
    ///
    /// **Divergence guard.** When the explicit tier disagrees with the
    /// license-derived tier, we emit a WARN log naming both. This is
    /// defence-in-depth: a future call site that opts into an
    /// explicit tier but stamps a misaligned license string still
    /// passes the tier check, but the WARN surfaces the disagreement
    /// to the operator's log without breaking the write.
    ///
    /// Refresh push side-effect: when the refresh branch fires, an
    /// [`EntityRefreshEvent`] is appended to the in-memory ring
    /// buffer (Sn-99 #4). Same-tier and absent-existing-row paths
    /// don't push — the log surfaces operator-visible *changes*.
    pub fn upsert_entity_with_tier(
        &self,
        ent: &Entity,
        incoming_tier: EntityProvenanceTier,
    ) -> Result<()> {
        // Divergence guard. Emit before the existence check so the WARN
        // fires even on the fresh-insert path (a misalignment is worth
        // surfacing regardless of whether the row already exists).
        let derived = entity_tier_from_license(&ent.envelope.provenance.license);
        if derived != incoming_tier {
            warn!(
                entity_id = %ent.entity_id.as_str(),
                license = %ent.envelope.provenance.license,
                explicit_tier = ?incoming_tier,
                license_derived_tier = ?derived,
                "upsert_entity_with_tier: explicit tier disagrees with \
                 license-derived tier; license string should match the \
                 closed-vocab mapping for the explicit tier"
            );
        }

        match self.get_entity_by_business_id(&ent.entity_id) {
            Err(StorageError::NotFound(_)) => self.insert_entity(ent),
            Err(other) => Err(other),
            Ok(existing) => {
                let existing_tier =
                    entity_tier_from_license(&existing.envelope.provenance.license);
                if incoming_tier <= existing_tier {
                    return Ok(());
                }
                // Refresh path — incoming is strictly richer. Sn-100 #5
                // also refreshes the flat provenance columns so the
                // dashboard's "where did this name come from" answer
                // cites the trigger (e.g. Lever A's recipe), not the
                // pre-refresh source. The record_subjects_* /
                // record_derived_from junction rows stay as-written by
                // the initial insert (see the doc-comment rationale).
                let previous_canonical_name = existing.canonical_name.clone();
                {
                    let conn = self.conn.lock().map_err(|e| {
                        StorageError::Other(format!("connection poisoned: {e}"))
                    })?;
                    conn.execute(
                        "UPDATE entities
                            SET kind = ?,
                                canonical_name = ?,
                                license = ?,
                                source_id = ?,
                                source_url = ?,
                                source_published_at = ?,
                                selector_path = ?,
                                raw_bytes_excerpt = ?
                          WHERE id = ?",
                        params![
                            ent.kind,
                            ent.canonical_name,
                            ent.envelope.provenance.license,
                            ent.envelope.provenance.source_id,
                            ent.envelope.provenance.source_url,
                            ent.envelope.provenance.source_published_at,
                            ent.envelope.provenance.selector_path,
                            ent.envelope.provenance.raw_bytes_excerpt,
                            existing.id,
                        ],
                    )
                    .map_err(StorageError::DuckDb)?;
                }
                // Conn lock released. Push the refresh event onto the
                // log; best-effort, never fails the write.
                self.push_entity_refresh_event(EntityRefreshEvent {
                    at: Utc::now(),
                    entity_id: ent.entity_id.as_str().to_string(),
                    previous_canonical_name,
                    new_canonical_name: ent.canonical_name.clone(),
                    previous_tier: existing_tier,
                    new_tier: incoming_tier,
                });
                Ok(())
            }
        }
    }

    /// Append one refresh event onto the per-process ring buffer.
    /// Capped at [`ENTITY_REFRESH_LOG_CAP`]; oldest entries fall off
    /// the front. Mutex-poisoned recovery follows the same posture
    /// the cost-ledger uses: log + recover, don't panic the request.
    ///
    /// Sn-100 #3 — also writes through to the persistent
    /// `entity_refresh_history` table (migration 0023) so the strip
    /// survives Cmd-Q + relaunch. Persistence is best-effort: a write
    /// failure warn-logs and the in-memory push still proceeds.
    fn push_entity_refresh_event(&self, event: EntityRefreshEvent) {
        // Persist first so the disk row carries the event's timestamp
        // before the in-memory ring observes it. Order doesn't matter
        // for correctness — both paths key off `event.at` — but doing
        // disk-first keeps the failure mode "the strip falls back to
        // in-memory only" rather than "in-memory ahead of disk".
        if let Err(e) =
            self.insert_entity_refresh_history_entry(&event, ENTITY_REFRESH_LOG_CAP)
        {
            warn!(
                error = %e,
                entity_id = %event.entity_id,
                "entity_refresh_history persist: insert failed; in-memory ring still updated"
            );
        }

        let mut guard = match self.entity_refresh_log.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                warn!(
                    "entity_refresh_log mutex poisoned; recovering and continuing"
                );
                poisoned.into_inner()
            }
        };
        guard.push_back(event);
        while guard.len() > ENTITY_REFRESH_LOG_CAP {
            guard.pop_front();
        }
    }

    /// Snapshot of the refresh-event ring buffer, oldest-first (the
    /// frontend reverses for newest-first display, matching the
    /// CostTimelinePanel convention).
    pub fn entity_refresh_log_snapshot(&self) -> Vec<EntityRefreshEvent> {
        let guard = match self.entity_refresh_log.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.iter().cloned().collect()
    }

    /// Fetch an entity by its business key. Uses the unique index on
    /// `entities.entity_id`.
    pub fn get_entity_by_business_id(&self, entity_id: &EntityId) -> Result<Entity> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let row_id: Uuid = conn
            .query_row(
                "SELECT id FROM entities WHERE entity_id = ?",
                params![entity_id.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("entity {entity_id}"))
                }
                other => StorageError::DuckDb(other),
            })?;
        drop(conn); // Release before re-acquiring in get_entity
        self.get_entity(row_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::Confidence;

    fn sample_entity() -> Entity {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "curated".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects::default(),
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Entity::new(
            EntityId::new("sqm").unwrap(),
            "company",
            "Sociedad Química y Minera de Chile",
            envelope,
        )
    }

    #[test]
    fn entity_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent = sample_entity();
        store.insert_entity(&ent).unwrap();

        let back = store.get_entity(ent.id).unwrap();
        assert_eq!(back.id, ent.id);
        assert_eq!(back.entity_id, ent.entity_id);
        assert_eq!(back.kind, "company");
        assert_eq!(back.canonical_name, ent.canonical_name);
    }

    #[test]
    fn entity_lookup_by_business_id_works() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent = sample_entity();
        store.insert_entity(&ent).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("sqm").unwrap())
            .unwrap();
        assert_eq!(back.id, ent.id);
    }

    #[test]
    fn duplicate_entity_id_violates_unique_constraint() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let ent1 = sample_entity();
        store.insert_entity(&ent1).unwrap();

        // A second entity with the same business id but a fresh UUID
        // should fail on the unique index.
        let ent2 = sample_entity();
        let result = store.insert_entity(&ent2);
        assert!(result.is_err(), "expected unique-violation error");
    }

    // -------------------------------------------------------------------
    // Session 98 #5 — tier-aware upsert_entity refresh
    // -------------------------------------------------------------------

    fn entity_with_license(
        business_id: &str,
        kind: &str,
        canonical_name: &str,
        license: &str,
    ) -> Entity {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: format!("test-source:{business_id}"),
                source_url: None,
                source_published_at: None,
                license: license.into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects::default(),
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Entity::new(
            EntityId::new(business_id).unwrap(),
            kind,
            canonical_name,
            envelope,
        )
    }

    #[test]
    fn tier_ordering_is_doc_gt_slug_gt_iterator_gt_unknown() {
        // Pin the load-bearing ordering. If this ever inverts, the
        // refresh semantics break silently.
        assert!(
            EntityProvenanceTier::DocumentExtracted > EntityProvenanceTier::SlugHumanised
        );
        assert!(
            EntityProvenanceTier::SlugHumanised > EntityProvenanceTier::RecipeIterator
        );
        assert!(
            EntityProvenanceTier::RecipeIterator > EntityProvenanceTier::Unknown
        );
    }

    #[test]
    fn tier_from_license_maps_known_strings() {
        assert_eq!(
            entity_tier_from_license("extracted"),
            EntityProvenanceTier::DocumentExtracted
        );
        assert_eq!(
            entity_tier_from_license("classifier-emitted"),
            EntityProvenanceTier::SlugHumanised
        );
        assert_eq!(
            entity_tier_from_license("unknown"),
            EntityProvenanceTier::RecipeIterator
        );
        assert_eq!(
            entity_tier_from_license("anything-else"),
            EntityProvenanceTier::Unknown
        );
    }

    #[test]
    fn upsert_entity_refresh_doc_extracted_overwrites_slug_humanised() {
        // entity_synth seeds with the humanised slug, then the
        // per-Doc Entity extractor finds the same entity_id with a
        // proper display name. The richer name must win.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug =
            entity_with_license("company:tsla", "company", "tsla", "classifier-emitted");
        store.upsert_entity(&slug).unwrap();

        let extracted = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "extracted",
        );
        store.upsert_entity(&extracted).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "Tesla, Inc.");
        assert_eq!(back.envelope.provenance.license, "extracted");
    }

    #[test]
    fn upsert_entity_refresh_slug_does_not_overwrite_doc_extracted() {
        // If Lever A wrote first, a later entity_synth call (e.g.
        // re-accept of a plan whose exemplar names the same entity)
        // must NOT clobber the LLM-extracted display name.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let extracted = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "extracted",
        );
        store.upsert_entity(&extracted).unwrap();

        let slug =
            entity_with_license("company:tsla", "company", "tsla", "classifier-emitted");
        store.upsert_entity(&slug).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "Tesla, Inc.");
        assert_eq!(back.envelope.provenance.license, "extracted");
    }

    #[test]
    fn upsert_entity_refresh_iterator_does_not_overwrite_slug() {
        // Lever B (recipe-iterator) writing into a row Sn-76 already
        // seeded — slug wins because iterator scalar names are noisier
        // than humanised slugs in expectation.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug =
            entity_with_license("driver:senna", "driver", "senna", "classifier-emitted");
        store.upsert_entity(&slug).unwrap();

        let iterator_row =
            entity_with_license("driver:senna", "driver", "S. Senna", "unknown");
        store.upsert_entity(&iterator_row).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("driver:senna").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "senna");
        assert_eq!(back.envelope.provenance.license, "classifier-emitted");
    }

    #[test]
    fn upsert_entity_refresh_doc_overwrites_recipe_iterator() {
        // Lever B writes first, Lever A writes later — Doc wins.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let iterator_row =
            entity_with_license("driver:hamilton", "driver", "L. Hamilton", "unknown");
        store.upsert_entity(&iterator_row).unwrap();

        let extracted = entity_with_license(
            "driver:hamilton",
            "driver",
            "Lewis Hamilton",
            "extracted",
        );
        store.upsert_entity(&extracted).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("driver:hamilton").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "Lewis Hamilton");
        assert_eq!(back.envelope.provenance.license, "extracted");
    }

    #[test]
    fn upsert_entity_refresh_same_tier_is_no_op() {
        // Two Lever A writes (e.g. two Documents mention the same
        // entity) must not flap the canonical_name. First-write-wins
        // at the same tier — refresh requires strictly-greater tier.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let first = entity_with_license(
            "company:catl",
            "company",
            "CATL",
            "extracted",
        );
        store.upsert_entity(&first).unwrap();

        let second = entity_with_license(
            "company:catl",
            "company",
            "Contemporary Amperex Technology",
            "extracted",
        );
        store.upsert_entity(&second).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:catl").unwrap())
            .unwrap();
        // First write wins at same tier.
        assert_eq!(back.canonical_name, "CATL");
    }

    #[test]
    fn upsert_entity_refresh_preserves_row_id() {
        // The refresh UPDATEs in place — `id` and `entity_id` stay
        // stable so `record_subjects_*` / `record_derived_from` joins
        // continue to resolve.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug =
            entity_with_license("mine:greenbushes", "mine", "greenbushes", "classifier-emitted");
        store.upsert_entity(&slug).unwrap();
        let before = store
            .get_entity_by_business_id(&EntityId::new("mine:greenbushes").unwrap())
            .unwrap();

        let extracted = entity_with_license(
            "mine:greenbushes",
            "mine",
            "Greenbushes Lithium Mine",
            "extracted",
        );
        store.upsert_entity(&extracted).unwrap();

        let after = store
            .get_entity_by_business_id(&EntityId::new("mine:greenbushes").unwrap())
            .unwrap();
        assert_eq!(after.id, before.id, "row id must stay stable across refresh");
        assert_eq!(after.canonical_name, "Greenbushes Lithium Mine");
    }

    // -------------------------------------------------------------------
    // Session 99 #4 — refresh-log push semantics
    // -------------------------------------------------------------------

    #[test]
    fn refresh_log_records_event_when_tier_strictly_elevates() {
        // Sn-98 #5 fires the refresh branch when DocumentExtracted
        // arrives after SlugHumanised. Sn-99 #4 surfaces that with a
        // ring-buffer event the dashboard tile reads from.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug = entity_with_license(
            "company:tsla",
            "company",
            "tsla",
            "classifier-emitted",
        );
        store
            .upsert_entity_with_tier(&slug, EntityProvenanceTier::SlugHumanised)
            .unwrap();
        // Pre-refresh: no events on the log.
        assert!(store.entity_refresh_log_snapshot().is_empty());

        let extracted = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "extracted",
        );
        store
            .upsert_entity_with_tier(&extracted, EntityProvenanceTier::DocumentExtracted)
            .unwrap();

        let snap = store.entity_refresh_log_snapshot();
        assert_eq!(snap.len(), 1, "exactly one refresh event recorded");
        let ev = &snap[0];
        assert_eq!(ev.entity_id, "company:tsla");
        assert_eq!(ev.previous_canonical_name, "tsla");
        assert_eq!(ev.new_canonical_name, "Tesla, Inc.");
        assert_eq!(ev.previous_tier, EntityProvenanceTier::SlugHumanised);
        assert_eq!(ev.new_tier, EntityProvenanceTier::DocumentExtracted);
        assert!(ev.name_changed());
    }

    #[test]
    fn refresh_log_skips_insert_path() {
        // Fresh-insert is not a refresh — the log only records *changes*
        // to existing rows.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let ent = entity_with_license(
            "company:newco",
            "company",
            "NewCo",
            "extracted",
        );
        store
            .upsert_entity_with_tier(&ent, EntityProvenanceTier::DocumentExtracted)
            .unwrap();
        assert!(
            store.entity_refresh_log_snapshot().is_empty(),
            "insert path must not push a refresh event"
        );
    }

    #[test]
    fn refresh_log_skips_same_tier_no_op() {
        // Second Lever A write at the same tier — no in-place mutation,
        // no event.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let first = entity_with_license(
            "company:catl",
            "company",
            "CATL",
            "extracted",
        );
        store
            .upsert_entity_with_tier(&first, EntityProvenanceTier::DocumentExtracted)
            .unwrap();
        let second = entity_with_license(
            "company:catl",
            "company",
            "Contemporary Amperex Technology",
            "extracted",
        );
        store
            .upsert_entity_with_tier(&second, EntityProvenanceTier::DocumentExtracted)
            .unwrap();
        assert!(
            store.entity_refresh_log_snapshot().is_empty(),
            "same-tier no-op must not push a refresh event"
        );
    }

    #[test]
    fn refresh_log_caps_at_max_entries() {
        // Force enough refreshes to exercise the ring-buffer eviction.
        // Each iteration creates a fresh entity_id, seeds it at
        // SlugHumanised, then upgrades to DocumentExtracted — one event
        // per pair.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let extra = 5usize;
        let total = ENTITY_REFRESH_LOG_CAP + extra;
        for i in 0..total {
            let bid = format!("company:e{i:04}");
            let slug = entity_with_license(
                &bid,
                "company",
                "slug",
                "classifier-emitted",
            );
            store
                .upsert_entity_with_tier(&slug, EntityProvenanceTier::SlugHumanised)
                .unwrap();
            let extracted = entity_with_license(
                &bid,
                "company",
                "Extracted Name",
                "extracted",
            );
            store
                .upsert_entity_with_tier(
                    &extracted,
                    EntityProvenanceTier::DocumentExtracted,
                )
                .unwrap();
        }
        let snap = store.entity_refresh_log_snapshot();
        assert_eq!(
            snap.len(),
            ENTITY_REFRESH_LOG_CAP,
            "ring buffer must cap at ENTITY_REFRESH_LOG_CAP"
        );
        // Oldest-first: the first `extra` events have been evicted; the
        // first surviving event names entity e{extra}.
        let first_surviving = &snap[0];
        let expected_first = format!("company:e{:04}", extra);
        assert_eq!(
            first_surviving.entity_id, expected_first,
            "ring buffer must evict oldest events first"
        );
    }

    #[test]
    fn snapshot_is_a_clone_not_a_drain() {
        // Reads must not mutate the buffer (the dashboard polls every
        // few seconds; a drain would empty the log after the first
        // poll).
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let slug = entity_with_license(
            "company:abc",
            "company",
            "abc",
            "classifier-emitted",
        );
        store
            .upsert_entity_with_tier(&slug, EntityProvenanceTier::SlugHumanised)
            .unwrap();
        let extracted = entity_with_license(
            "company:abc",
            "company",
            "ABC, Inc.",
            "extracted",
        );
        store
            .upsert_entity_with_tier(&extracted, EntityProvenanceTier::DocumentExtracted)
            .unwrap();

        let first = store.entity_refresh_log_snapshot();
        let second = store.entity_refresh_log_snapshot();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1, "snapshot must not drain the buffer");
    }

    // -------------------------------------------------------------------
    // Session 99 #5 — explicit-tier API + divergence guard
    // -------------------------------------------------------------------

    #[test]
    fn explicit_tier_overrides_license_derived_when_disagreement() {
        // A misaligned license string ("unknown") must NOT downgrade an
        // explicit DocumentExtracted tier — the explicit signal is
        // authoritative. The divergence WARN fires (we can't easily
        // capture log output here without a subscriber, but the
        // behaviour is the test): the upsert sees DocumentExtracted as
        // the incoming tier and refreshes accordingly.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug = entity_with_license(
            "company:tsla",
            "company",
            "tsla",
            "classifier-emitted",
        );
        store
            .upsert_entity_with_tier(&slug, EntityProvenanceTier::SlugHumanised)
            .unwrap();

        // Note: license="unknown" maps to RecipeIterator, but we pass
        // DocumentExtracted explicitly. The refresh decision keys off
        // the explicit tier, so the upsert refreshes the row.
        let misaligned = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "unknown",
        );
        store
            .upsert_entity_with_tier(
                &misaligned,
                EntityProvenanceTier::DocumentExtracted,
            )
            .unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(
            back.canonical_name, "Tesla, Inc.",
            "explicit tier must drive refresh, not license string"
        );
        // The refresh fired so the log captured one event.
        assert_eq!(store.entity_refresh_log_snapshot().len(), 1);
    }

    #[test]
    fn back_compat_shim_derives_tier_from_license() {
        // The Sn-98 back-compat shim still works: pass an Entity, the
        // shim derives the tier from license and delegates. Migrate
        // path producing the same refresh event as the explicit-tier
        // path.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug = entity_with_license(
            "company:tsla",
            "company",
            "tsla",
            "classifier-emitted",
        );
        store.upsert_entity(&slug).unwrap();

        let extracted = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "extracted",
        );
        store.upsert_entity(&extracted).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "Tesla, Inc.");
        assert_eq!(store.entity_refresh_log_snapshot().len(), 1);
    }

    // -------------------------------------------------------------------
    // Sn-100 #5 — flat-provenance refresh on tier elevation
    // -------------------------------------------------------------------

    /// Build an Entity with explicit source_id + source_url + selector_path
    /// so the flat-provenance refresh assertions have something to bite.
    fn entity_with_full_provenance(
        business_id: &str,
        kind: &str,
        canonical_name: &str,
        license: &str,
        source_id: &str,
        source_url: Option<&str>,
        selector_path: Option<&str>,
        raw_bytes_excerpt: Option<&str>,
    ) -> Entity {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: source_id.into(),
                source_url: source_url.map(|s| s.into()),
                source_published_at: None,
                license: license.into(),
                derived_from: vec![],
                selector_path: selector_path.map(|s| s.into()),
                raw_bytes_excerpt: raw_bytes_excerpt.map(|s| s.into()),
            },
            subjects: Subjects::default(),
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        Entity::new(
            EntityId::new(business_id).unwrap(),
            kind,
            canonical_name,
            envelope,
        )
    }

    #[test]
    fn refresh_updates_flat_provenance_source_id_and_url() {
        // Sn-76-shaped row first (plan-keyed source_id, no source_url).
        // Then a Lever-A-shaped row with a recipe source_id +
        // source_url. After refresh, the row's flat provenance must
        // reflect the incoming trigger so the dashboard's "where did
        // this name come from" answer matches the new tier.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug = entity_with_full_provenance(
            "company:tsla",
            "company",
            "tsla",
            "classifier-emitted",
            "plan:abc#entity_exemplar",
            None,
            None,
            None,
        );
        store
            .upsert_entity_with_tier(&slug, EntityProvenanceTier::SlugHumanised)
            .unwrap();

        let extracted = entity_with_full_provenance(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "extracted",
            "tesla.com#recipe:r1@v2",
            Some("https://www.tesla.com/press"),
            Some("css:.press-release h1"),
            Some("Tesla, Inc. announces…"),
        );
        store
            .upsert_entity_with_tier(&extracted, EntityProvenanceTier::DocumentExtracted)
            .unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        // Display columns refreshed (pre-Sn-100 behaviour).
        assert_eq!(back.canonical_name, "Tesla, Inc.");
        assert_eq!(back.envelope.provenance.license, "extracted");
        // Flat provenance refreshed (Sn-100 #5 behaviour).
        assert_eq!(
            back.envelope.provenance.source_id, "tesla.com#recipe:r1@v2",
            "source_id must reflect the refresh trigger"
        );
        assert_eq!(
            back.envelope.provenance.source_url.as_deref(),
            Some("https://www.tesla.com/press"),
            "source_url must reflect the refresh trigger"
        );
        assert_eq!(
            back.envelope.provenance.selector_path.as_deref(),
            Some("css:.press-release h1"),
        );
        assert_eq!(
            back.envelope.provenance.raw_bytes_excerpt.as_deref(),
            Some("Tesla, Inc. announces…"),
        );
    }

    #[test]
    fn refresh_clears_flat_provenance_when_incoming_is_none() {
        // Symmetric case: the incoming Entity carries no source_url /
        // selector_path. The refresh must write NULL into those columns
        // so the row's flat provenance fully matches the new tier
        // (not a mix of old + new).
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let iterator_row = entity_with_full_provenance(
            "driver:senna",
            "driver",
            "S. Senna",
            "unknown",
            "recipe:lever-b:driver:senna",
            Some("https://example.com/iter"),
            Some("css:tr.row td:nth-child(2)"),
            Some("noisy excerpt"),
        );
        store
            .upsert_entity_with_tier(&iterator_row, EntityProvenanceTier::RecipeIterator)
            .unwrap();

        let plan_keyed = entity_with_full_provenance(
            "driver:senna",
            "driver",
            "senna",
            "classifier-emitted",
            "plan:xyz#entity_exemplar",
            None,
            None,
            None,
        );
        store
            .upsert_entity_with_tier(&plan_keyed, EntityProvenanceTier::SlugHumanised)
            .unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("driver:senna").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "senna");
        assert_eq!(back.envelope.provenance.source_id, "plan:xyz#entity_exemplar");
        assert!(
            back.envelope.provenance.source_url.is_none(),
            "source_url must reset to NULL when the refresh trigger has none"
        );
        assert!(back.envelope.provenance.selector_path.is_none());
        assert!(back.envelope.provenance.raw_bytes_excerpt.is_none());
    }

    #[test]
    fn no_op_upsert_leaves_flat_provenance_untouched() {
        // Same-tier upsert is a no-op. The flat provenance must NOT
        // change — first-write-wins at the same tier.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let first = entity_with_full_provenance(
            "company:catl",
            "company",
            "CATL",
            "extracted",
            "first.com#recipe:r1@v1",
            Some("https://first.com/news"),
            Some("css:h1.headline"),
            Some("first excerpt"),
        );
        store
            .upsert_entity_with_tier(&first, EntityProvenanceTier::DocumentExtracted)
            .unwrap();

        let second_same_tier = entity_with_full_provenance(
            "company:catl",
            "company",
            "Contemporary Amperex Technology",
            "extracted",
            "second.com#recipe:r2@v1",
            Some("https://second.com/news"),
            Some("css:h2.title"),
            Some("second excerpt"),
        );
        store
            .upsert_entity_with_tier(
                &second_same_tier,
                EntityProvenanceTier::DocumentExtracted,
            )
            .unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:catl").unwrap())
            .unwrap();
        assert_eq!(back.canonical_name, "CATL", "first-write-wins at same tier");
        assert_eq!(
            back.envelope.provenance.source_id, "first.com#recipe:r1@v1",
            "source_id must NOT change on a same-tier no-op"
        );
        assert_eq!(
            back.envelope.provenance.source_url.as_deref(),
            Some("https://first.com/news")
        );
    }

    #[test]
    fn shim_with_unknown_license_does_not_refresh() {
        // Pre-Sn-99: an unknown license string mapped to
        // `EntityProvenanceTier::Unknown`, which is strictly less than
        // every other tier, so the refresh branch never fired. The
        // shim preserves that behaviour and adds a WARN so the
        // operator notices.
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let slug = entity_with_license(
            "company:tsla",
            "company",
            "tsla",
            "classifier-emitted",
        );
        store.upsert_entity(&slug).unwrap();

        let unknown_license = entity_with_license(
            "company:tsla",
            "company",
            "Tesla, Inc.",
            "non-vocab-license-string",
        );
        store.upsert_entity(&unknown_license).unwrap();

        let back = store
            .get_entity_by_business_id(&EntityId::new("company:tsla").unwrap())
            .unwrap();
        // Refresh suppressed — the SlugHumanised row survives.
        assert_eq!(back.canonical_name, "tsla");
        assert!(store.entity_refresh_log_snapshot().is_empty());
    }
}
