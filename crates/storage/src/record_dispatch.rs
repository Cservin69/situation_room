//! Generic insert dispatch over the `Record` enum.
//!
//! The per-type modules (`observations`, `events`, ...) expose typed
//! `insert_*` methods. Callers that work with `Record` values —
//! source adapters in particular — can use [`Store::insert_record`]
//! to avoid matching on the variant themselves.

use situation_room_core::Record;

use crate::connection::Store;
use crate::Result;

impl Store {
    /// Insert any record variant. Dispatches to the variant-specific
    /// `insert_*` method. Useful when the caller holds a `Record`
    /// (e.g. a source adapter's `FetchOutcome::records`).
    pub fn insert_record(&self, record: &Record) -> Result<()> {
        match record {
            Record::Observation(r) => self.insert_observation(r),
            Record::Event(r) => self.insert_event(r),
            // Session 97 Lever B — recipe-emitted Entity rows arrive
            // through this dispatch and re-collide on every refetch
            // (iterator-bearing recipes against entity_kind expectations
            // re-emit the same entity_ids). `upsert_entity` swallows
            // the UNIQUE-entity_id conflict the same way Sn-76's
            // `entity_synth` does; plan-accept-time exemplars and
            // recipe-time rows converge on idempotent write semantics.
            Record::Entity(r) => self.upsert_entity(r),
            Record::Relation(r) => self.insert_relation(r),
            Record::Document(r) => self.insert_document(r),
            Record::Assertion(r) => self.insert_assertion(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, Topic};
    use situation_room_core::Document;

    #[test]
    fn insert_record_dispatches_to_document() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let envelope = Envelope {
            provenance: Provenance {
                source_id: "test".into(),
                source_url: None,
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
                selector_path: None,
                raw_bytes_excerpt: None,
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        };
        let doc = Document::new("report", "body text", envelope);
        let id = doc.id;
        let record: Record = doc.into();

        store.insert_record(&record).unwrap();

        // Verify it actually landed.
        let back = store.get_document(id).unwrap();
        assert_eq!(back.id, id);
    }
}
