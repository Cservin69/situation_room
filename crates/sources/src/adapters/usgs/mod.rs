//! USGS Mineral Commodity Summaries adapter.
//!
//! Annual per-commodity PDFs at stable URLs on `pubs.usgs.gov`.
//! Public domain, no API key, authoritative for U.S. mineral
//! production and reserves.
//!
//! ## Current status (post-pivot, 2026-04-20)
//!
//! This adapter **does not emit Observations**. It fetches the PDF,
//! extracts text via `pdf-extract`, and emits a single `Document`
//! record. Observation emission is the job of the Level-2
//! `FetchRecipe` apply runtime (ADR 0007), which is under
//! construction in the next phase.
//!
//! An earlier Session B attempt shipped a hand-written regex parser
//! that produced visibly wrong results on real data (wrong years,
//! wrong metric kinds, non-country rows identified as countries).
//! That was backed out. **The lesson: we do not shortcut around the
//! architecture. Deterministic per-source parsers are exactly what
//! ADR 0007 rejected in favor of LLM-authored recipes.**
//!
//! See `STOCKPILE_HANDOFF_SESSION2.md` for the full story.

use async_trait::async_trait;
use chrono::Utc;
use stockpile_core::schema::envelope::{Envelope, Provenance, Subjects};
use stockpile_core::vocab::{Confidence, Topic, Unit};
use stockpile_core::{Document, Record};
use stockpile_secure::http::SecureHttpClient;

use crate::traits::{
    AuthoritativeDomain, Cadence, DataLicense, FetchContext, FetchOutcome, Source, SourceError,
    SourceMetadata,
};

/// USGS MCS source, scoped to one (year, commodity) pair per instance.
pub struct UsgsMcsAdapter {
    http: SecureHttpClient,
    year: u16,
    commodity_slug: String,
    topic: Topic,
    /// Unit carried for future use by the recipe-apply runtime. Not
    /// used by this adapter directly — the adapter just fetches.
    #[allow(dead_code)]
    unit: Unit,
    url_override: Option<String>,
}

impl UsgsMcsAdapter {
    pub fn new(
        http: SecureHttpClient,
        year: u16,
        commodity_slug: impl Into<String>,
        topic: Topic,
        unit: Unit,
    ) -> Self {
        Self {
            http,
            year,
            commodity_slug: commodity_slug.into(),
            topic,
            unit,
            url_override: None,
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn with_url_override(mut self, url: impl Into<String>) -> Self {
        self.url_override = Some(url.into());
        self
    }

    pub fn pdf_url(&self) -> String {
        if let Some(url) = &self.url_override {
            return url.clone();
        }
        format!(
            "https://pubs.usgs.gov/periodicals/mcs{year}/mcs{year}-{slug}.pdf",
            year = self.year,
            slug = self.commodity_slug
        )
    }

    pub fn source_id(&self) -> String {
        format!("usgs_mcs:{}:{}", self.year, self.commodity_slug)
    }
}

#[async_trait]
impl Source for UsgsMcsAdapter {
    fn metadata(&self) -> SourceMetadata {
        SourceMetadata {
            id: self.source_id(),
            display_name: format!(
                "USGS Mineral Commodity Summaries {} — {}",
                self.year, self.commodity_slug
            ),
            description: "Annual U.S. Geological Survey report on mineral \
                          commodity production, reserves, and trade. Public \
                          domain; authoritative for U.S. mineral production \
                          figures."
                .into(),
            license: DataLicense::PublicDomain,
            authoritative_for: vec![
                AuthoritativeDomain {
                    commodity: Some(self.commodity_slug.clone()),
                    metric: "production".into(),
                },
                AuthoritativeDomain {
                    commodity: Some(self.commodity_slug.clone()),
                    metric: "reserves".into(),
                },
            ],
            default_cadence: Cadence::YearlyOn { month: 2, day: 1 },
            homepage: Some(
                "https://www.usgs.gov/centers/national-minerals-information-center/mineral-commodity-summaries"
                    .into(),
            ),
        }
    }

    async fn fetch(&self, _ctx: FetchContext) -> Result<FetchOutcome, SourceError> {
        let url = self.pdf_url();
        tracing::info!(url = %url, "fetching USGS MCS PDF");

        let bytes = self
            .http
            .get_bytes(&url)
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        let now = Utc::now();
        let mut notes = vec![format!("fetched {} bytes from {}", bytes.len(), url)];

        // Extract text. If extraction fails, the Document still lands —
        // it's proof-of-fetch even if the body is empty.
        let extracted_text = match pdf_extract::extract_text_from_mem(&bytes) {
            Ok(t) => t,
            Err(e) => {
                notes.push(format!("text extraction failed: {e}"));
                String::new()
            }
        };
        notes.push(format!(
            "extracted {} chars of text (awaiting recipe-apply runtime for Observations)",
            extracted_text.len()
        ));

        let envelope = Envelope {
            provenance: Provenance {
                source_id: self.source_id(),
                source_url: Some(url.clone()),
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: vec![self.topic.clone()],
            },
            tags: vec!["source_tier:authoritative".into()],
            valid_at: None,
            observed_at: now,
            confidence: Confidence::ONE,
        };

        let mut doc = Document::new("report", extracted_text, envelope);
        doc.dedup_key = Some(format!("{}:mcs:{}", self.source_id(), self.year));
        doc.title = Some(format!(
            "USGS Mineral Commodity Summaries {} — {}",
            self.year, self.commodity_slug
        ));
        doc.mime = "text/plain".into();

        Ok(FetchOutcome {
            records: vec![Record::Document(doc)],
            new_watermark: Some(now),
            notes,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use stockpile_secure::http::SecureHttpConfig;

    fn test_http() -> SecureHttpClient {
        SecureHttpClient::new(SecureHttpConfig::default()).unwrap()
    }

    fn test_adapter() -> UsgsMcsAdapter {
        UsgsMcsAdapter::new(
            test_http(),
            2025,
            "lithium",
            Topic::new("Li").unwrap(),
            Unit::new("t").unwrap(),
        )
    }

    #[test]
    fn pdf_url_is_derived_from_year_and_slug() {
        assert_eq!(
            test_adapter().pdf_url(),
            "https://pubs.usgs.gov/periodicals/mcs2025/mcs2025-lithium.pdf"
        );
    }

    #[test]
    fn source_id_includes_year_and_commodity() {
        let adapter = UsgsMcsAdapter::new(
            test_http(),
            2025,
            "copper",
            Topic::new("Cu").unwrap(),
            Unit::new("t").unwrap(),
        );
        assert_eq!(adapter.source_id(), "usgs_mcs:2025:copper");
    }

    #[test]
    fn metadata_declares_authoritative_domains() {
        let meta = test_adapter().metadata();
        assert_eq!(meta.authoritative_for.len(), 2);
        let metrics: Vec<_> = meta
            .authoritative_for
            .iter()
            .map(|d| d.metric.as_str())
            .collect();
        assert!(metrics.contains(&"production"));
        assert!(metrics.contains(&"reserves"));
    }

    #[test]
    fn metadata_is_public_domain() {
        match test_adapter().metadata().license {
            DataLicense::PublicDomain => {}
            other => panic!("expected PublicDomain, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_errors_bubble_up_as_source_error() {
        let adapter =
            test_adapter().with_url_override("http://127.0.0.1:1/does-not-exist.pdf");
        let result = adapter
            .fetch(FetchContext {
                since: None,
                focus: vec![],
            })
            .await;
        assert!(result.is_err());
    }

    // Live test — hits real USGS, ignored by default.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_returns_document() {
        let outcome = test_adapter()
            .fetch(FetchContext {
                since: None,
                focus: vec![],
            })
            .await
            .expect("live fetch should succeed");

        // Expect exactly one Document. No Observations until the
        // recipe-apply runtime lands.
        assert_eq!(outcome.records.len(), 1);
        assert!(matches!(&outcome.records[0], Record::Document(_)));
        if let Record::Document(doc) = &outcome.records[0] {
            assert_eq!(doc.mime, "text/plain");
            assert!(!doc.body.is_empty());
        }
    }
}
