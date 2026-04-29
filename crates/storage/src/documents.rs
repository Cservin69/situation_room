//! Document storage: insert + get.
//!
//! Documents have flat columns for title/kind/mime/body/published_at/
//! author rather than a JSON content column. The body is kept inline
//! so extraction doesn't need a second round-trip to the filesystem;
//! binary originals (PDFs etc.) live in the file archive keyed by id.

use duckdb::params;
use stockpile_core::Document;
use uuid::Uuid;

use crate::connection::Store;
use crate::envelope_io::{reconstruct_envelope, EnvelopeColumns, EnvelopeRow};
use crate::{Result, StorageError};

impl Store {
    pub fn insert_document(&self, doc: &Document) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;
        let tx = conn.transaction().map_err(StorageError::DuckDb)?;

        let cols = EnvelopeColumns::from_envelope(&doc.envelope)?;

        tx.execute(
            "INSERT INTO documents (
                id, dedup_key, title, doc_kind, mime, body, published_at, author,
                source_id, source_url, source_published_at,
                license, tags, subject_time, observed_at, valid_at, confidence
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                doc.id,
                doc.dedup_key,
                doc.title,
                doc.kind,
                doc.mime,
                doc.body,
                doc.published_at,
                doc.author,
                cols.source_id,
                cols.source_url,
                cols.source_published_at,
                cols.license,
                cols.tags_json,
                cols.subject_time_json,
                cols.observed_at,
                cols.valid_at,
                cols.confidence,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        crate::envelope_io::insert_subjects_and_derivation(
            &tx,
            doc.id,
            "document",
            &doc.envelope,
        )?;

        tx.commit().map_err(StorageError::DuckDb)?;
        Ok(())
    }

    pub fn get_document(&self, id: Uuid) -> Result<Document> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        #[allow(clippy::type_complexity)]
        let (
            row_id,
            dedup_key,
            title,
            kind,
            mime,
            body,
            published_at,
            author,
            raw,
        ): (
            Uuid,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<String>,
            EnvelopeRow,
        ) = conn
            .query_row(
                "SELECT id, dedup_key, title, doc_kind, mime, body, published_at, author,
                        source_id, source_url, source_published_at,
                        license, tags, subject_time, observed_at, valid_at, confidence
                 FROM documents
                 WHERE id = ?",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        EnvelopeRow {
                            source_id: r.get(8)?,
                            source_url: r.get(9)?,
                            source_published_at: r.get(10)?,
                            license: r.get(11)?,
                            tags_json: r.get(12)?,
                            subject_time_json: r.get(13)?,
                            observed_at: r.get(14)?,
                            valid_at: r.get(15)?,
                            confidence_f: r.get(16)?,
                        },
                    ))
                },
            )
            .map_err(|e| match e {
                duckdb::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("document {id}"))
                }
                other => StorageError::DuckDb(other),
            })?;

        let envelope = reconstruct_envelope(&conn, row_id, raw)?;

        Ok(Document {
            id: row_id,
            dedup_key,
            title,
            kind,
            mime,
            body,
            published_at,
            author,
            envelope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use stockpile_core::schema::envelope::{Envelope, Provenance, Subjects};
    use stockpile_core::vocab::{Confidence, Topic};

    fn sample_document() -> Document {
        let envelope = Envelope {
            provenance: Provenance {
                source_id: "reuters".into(),
                source_url: Some("https://reuters.com/article/li-report-q4".into()),
                source_published_at: Some(Utc.with_ymd_and_hms(2025, 3, 10, 14, 0, 0).unwrap()),
                license: "fair_use".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![Topic::new("Li").unwrap()],
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc.with_ymd_and_hms(2025, 3, 10, 14, 5, 0).unwrap(),
            confidence: Confidence::new(0.7).unwrap(),
        };
        let mut doc = Document::new(
            "article",
            "Full article body text goes here. Multi-line content is fine.",
            envelope,
        );
        doc.title = Some("Lithium prices steady amid Q4 production reports".into());
        doc.mime = "text/html".into();
        doc.author = Some("Reuters Staff".into());
        doc.published_at = Some(Utc.with_ymd_and_hms(2025, 3, 10, 14, 0, 0).unwrap());
        doc
    }

    #[test]
    fn document_roundtrips_through_storage() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let doc = sample_document();
        store.insert_document(&doc).unwrap();

        let back = store.get_document(doc.id).unwrap();
        assert_eq!(back.id, doc.id);
        assert_eq!(back.title, doc.title);
        assert_eq!(back.kind, doc.kind);
        assert_eq!(back.body, doc.body);
        assert_eq!(back.author, doc.author);
        assert_eq!(back.published_at, doc.published_at);
    }
}
