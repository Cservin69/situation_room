//! Entity storage: insert + get.
//!
//! Entities differ from the other record types: they have no `content`
//! JSON column — the business data is `entity_id` + `kind` +
//! `canonical_name` + optional `geometry`, stored as flat columns.
//! They also have no `dedup_key` — the `entity_id` is the business key
//! with a UNIQUE constraint.

use duckdb::params;
use situation_room_core::vocab::EntityId;
use situation_room_core::Entity;
use uuid::Uuid;

use crate::connection::Store;
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
    /// rewriting them would either lose history or duplicate it. The
    /// refresh is scoped to the flat display columns (the operator's
    /// dashboard tile content) plus the license string (so subsequent
    /// `upsert_entity` calls compare against the post-refresh tier).
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
    pub fn upsert_entity(&self, ent: &Entity) -> Result<()> {
        match self.get_entity_by_business_id(&ent.entity_id) {
            Err(StorageError::NotFound(_)) => return self.insert_entity(ent),
            Err(other) => return Err(other),
            Ok(existing) => {
                let existing_tier =
                    entity_tier_from_license(&existing.envelope.provenance.license);
                let incoming_tier =
                    entity_tier_from_license(&ent.envelope.provenance.license);
                if incoming_tier <= existing_tier {
                    return Ok(());
                }
                // Refresh path — incoming is strictly richer.
                let conn = self
                    .conn
                    .lock()
                    .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
                conn.execute(
                    "UPDATE entities
                        SET kind = ?,
                            canonical_name = ?,
                            license = ?
                      WHERE id = ?",
                    params![
                        ent.kind,
                        ent.canonical_name,
                        ent.envelope.provenance.license,
                        existing.id,
                    ],
                )
                .map_err(StorageError::DuckDb)?;
                Ok(())
            }
        }
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
}
