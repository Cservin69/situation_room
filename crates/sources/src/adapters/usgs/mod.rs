//! USGS Mineral Commodity Summaries adapter.
//!
//! Annual per-commodity PDFs at stable URLs on `pubs.usgs.gov`.
//! Public domain, no API key, authoritative for U.S. mineral
//! production and reserves.
//!
//! Session A (done): fetch PDF bytes, emit a `Document` record.
//! Session B (this): extract text with [`parse::extract_text`], emit
//! Observations for recognized production / reserves rows.
//!
//! ## Parser quality
//!
//! Pure-Rust PDF text extraction produces imperfect output on tabular
//! layouts. The parser is best-effort — see [`parse`] module docs.
//! Rows that don't cleanly match a country × numbers pattern are
//! silently skipped; missing data is not an error. The robust
//! extraction path is the Level-2 LLM recipe pipeline (ADR 0007).

pub mod parse;

use async_trait::async_trait;
use chrono::Utc;
use stockpile_core::schema::content::{ObservationContent, ObservationPeriod};
use stockpile_core::schema::envelope::{Envelope, Provenance, Subjects};
use stockpile_core::vocab::{Confidence, CountryCode, Topic, Unit};
use stockpile_core::{Document, Observation, Record};
use stockpile_secure::http::SecureHttpClient;

use crate::traits::{
    AuthoritativeDomain, Cadence, DataLicense, FetchContext, FetchOutcome, Source, SourceError,
    SourceMetadata,
};

use parse::{ParsedRow, RowKind};

/// USGS MCS source, scoped to one (year, commodity) pair per instance.
pub struct UsgsMcsAdapter {
    http: SecureHttpClient,
    year: u16,
    commodity_slug: String,
    topic: Topic,
    /// Unit for this commodity's production values. USGS MCS reports
    /// most commodities in metric tons; a few (platinum-group metals,
    /// gemstones) use kilograms or carats. Caller supplies to avoid
    /// per-commodity guessing in the parser.
    unit: Unit,
    url_override: Option<String>,
}

impl UsgsMcsAdapter {
    /// Construct a USGS MCS adapter. `commodity_slug` is the URL
    /// slug ("lithium", "copper"). `topic` is the envelope tag
    /// attached to emitted records. `unit` is the production unit
    /// (most commodities: `"t"` for metric tons).
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

    /// Build an Observation record from one parsed table row.
    /// `country_code` is optional: if we recognized the country label,
    /// it's attached as a PlaceRef. Otherwise, only topic tagging is
    /// applied.
    fn observation_from_row(
        &self,
        row: &ParsedRow,
        country_code: Option<CountryCode>,
        source_url: &str,
        pdf_observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Observation {
        let metric = match row.kind {
            RowKind::Production => "production",
            RowKind::Reserves => "reserves",
        };

        let period = match row.kind {
            RowKind::Production => ObservationPeriod::Annual,
            RowKind::Reserves => ObservationPeriod::Instant,
        };

        // valid_at: end of the reporting year when we know it; None
        // otherwise. USGS production numbers are "calendar year"
        // values, so Dec 31 of `row.year` is the natural choice.
        let valid_at = row.year.and_then(|y| {
            use chrono::TimeZone;
            chrono::NaiveDate::from_ymd_opt(y as i32, 12, 31)
                .and_then(|d| d.and_hms_opt(23, 59, 59))
                .and_then(|ndt| chrono::Utc.from_local_datetime(&ndt).single())
        });

        let places = match country_code {
            Some(c) => vec![stockpile_core::schema::envelope::PlaceRef::Country(c)],
            None => vec![],
        };

        let envelope = Envelope {
            provenance: Provenance {
                source_id: self.source_id(),
                source_url: Some(source_url.to_string()),
                source_published_at: None,
                license: "public_domain".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places,
                time: None,
                topics: vec![self.topic.clone()],
            },
            tags: vec![
                "source_tier:authoritative".into(),
                "parser:best_effort".into(),
            ],
            valid_at,
            observed_at: pdf_observed_at,
            confidence: Confidence::new(0.95).unwrap_or(Confidence::ONE),
        };

        let content = ObservationContent {
            metric: metric.into(),
            value: row.value,
            unit: self.unit.clone(),
            value_uncertainty: None,
            currency: None,
            period,
            geometry: None,
        };

        let dedup_key = format!(
            "{}:{}:{}:{}",
            self.source_id(),
            metric,
            row.country_label.replace(' ', "_").to_lowercase(),
            row.year.map(|y| y.to_string()).unwrap_or_else(|| "unknown".into())
        );

        Observation::new(envelope, content).with_dedup_key(dedup_key)
    }
}

/// Map a USGS country label to an ISO 3166-1 alpha-2 code. Returns
/// `None` for labels we don't recognize (including sub-country rows
/// like "United States" subdivisions or "World total").
///
/// The list is intentionally short — only major producer countries
/// that commonly appear in MCS tables. Unknown labels still produce
/// Observations, just without a `PlaceRef::Country` attached.
pub(crate) fn label_to_country_code(label: &str) -> Option<CountryCode> {
    let code = match label.trim() {
        "Argentina" => "AR",
        "Australia" => "AU",
        "Bolivia" => "BO",
        "Brazil" => "BR",
        "Canada" => "CA",
        "Chile" => "CL",
        "China" => "CN",
        "Congo (Kinshasa)" | "Democratic Republic of the Congo" => "CD",
        "Germany" => "DE",
        "India" => "IN",
        "Indonesia" => "ID",
        "Japan" => "JP",
        "Kazakhstan" => "KZ",
        "Mexico" => "MX",
        "Morocco" => "MA",
        "Peru" => "PE",
        "Poland" => "PL",
        "Portugal" => "PT",
        "Russia" => "RU",
        "South Africa" => "ZA",
        "South Korea" | "Korea, Republic of" => "KR",
        "Spain" => "ES",
        "Turkey" => "TR",
        "United States" => "US",
        "Vietnam" => "VN",
        "Zambia" => "ZM",
        "Zimbabwe" => "ZW",
        _ => return None,
    };
    CountryCode::new(code).ok()
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

        // Extract text. On failure, fall back to an empty text body —
        // the Document still has value as a record-of-fetch even if we
        // can't parse it.
        let extracted_text = match parse::extract_text(&bytes) {
            Ok(t) => t,
            Err(e) => {
                notes.push(format!("text extraction failed: {e}"));
                String::new()
            }
        };

        // Build the Document record.
        let doc_envelope = Envelope {
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

        let mut doc = Document::new("report", extracted_text.clone(), doc_envelope);
        doc.dedup_key = Some(format!("{}:mcs:{}", self.source_id(), self.year));
        doc.title = Some(format!(
            "USGS Mineral Commodity Summaries {} — {}",
            self.year, self.commodity_slug
        ));
        doc.mime = "text/plain".into();

        let mut records: Vec<Record> = vec![Record::Document(doc)];

        // Parse production rows; emit one Observation per recognized row.
        let rows = parse::parse_production_rows(&extracted_text);
        let row_count = rows.len();
        for row in rows {
            let country = label_to_country_code(&row.country_label);
            let obs = self.observation_from_row(&row, country, &url, now);
            records.push(Record::Observation(obs));
        }
        notes.push(format!("parsed {} production/reserves rows", row_count));

        Ok(FetchOutcome {
            records,
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
        let adapter = test_adapter();
        assert_eq!(
            adapter.pdf_url(),
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

    #[test]
    fn label_to_country_handles_known_producers() {
        assert!(label_to_country_code("Chile").is_some());
        assert!(label_to_country_code("Australia").is_some());
        assert!(label_to_country_code("China").is_some());
        assert!(label_to_country_code("United States").is_some());
        // Sub-country label or unknown → None
        assert!(label_to_country_code("Nevada").is_none());
        assert!(label_to_country_code("World total").is_none());
    }

    #[test]
    fn observation_from_row_populates_envelope_correctly() {
        let adapter = test_adapter();
        let row = ParsedRow {
            country_label: "Chile".into(),
            year: Some(2024),
            value: 53_000.0,
            kind: RowKind::Production,
        };
        let obs = adapter.observation_from_row(
            &row,
            label_to_country_code("Chile"),
            "https://example/test.pdf",
            Utc::now(),
        );
        assert_eq!(obs.content.metric, "production");
        assert_eq!(obs.content.value, 53_000.0);
        assert_eq!(obs.envelope.subjects.topics.len(), 1);
        assert_eq!(obs.envelope.subjects.places.len(), 1);
        assert!(obs.dedup_key.as_ref().unwrap().contains("chile"));
        assert!(obs.dedup_key.as_ref().unwrap().contains("2024"));
        // valid_at should be end-of-year 2024
        let v = obs.envelope.valid_at.expect("valid_at should be set");
        assert_eq!(v.naive_utc().date().to_string(), "2024-12-31");
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

    // Integration test that actually hits USGS. Ignored by default.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_returns_document_and_observations() {
        let adapter = test_adapter();
        let outcome = adapter
            .fetch(FetchContext {
                since: None,
                focus: vec![],
            })
            .await
            .expect("live fetch should succeed");

        // Expect one Document plus some number of Observations.
        assert!(outcome.records.len() >= 1);
        let n_docs = outcome
            .records
            .iter()
            .filter(|r| matches!(r, Record::Document(_)))
            .count();
        let n_obs = outcome
            .records
            .iter()
            .filter(|r| matches!(r, Record::Observation(_)))
            .count();
        assert_eq!(n_docs, 1, "exactly one Document expected");
        // Best-effort parser: we expect *some* observations but the
        // exact count depends on USGS's layout that year. A single
        // recognized country is enough to prove the pipeline works.
        assert!(
            n_obs >= 1,
            "expected at least one parsed Observation, got {n_obs} (parser may need a tweak \
             for this year's layout). Notes: {:?}",
            outcome.notes
        );

        // Spot-check: the Document should have extracted text, not base64.
        if let Record::Document(doc) = &outcome.records[0] {
            assert_eq!(doc.mime, "text/plain");
            assert!(!doc.body.is_empty(), "extracted text should not be empty");
            assert!(!doc.body.starts_with("JVBERi"), "body should not be base64 PDF");
        }
    }
}
