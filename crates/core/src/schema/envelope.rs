//! The envelope.
//!
//! Every record in situation_room carries this metadata. It answers five
//! questions a user will want to ask of any claim the system surfaces:
//!
//! 1. **Where did this come from?** → [`Provenance`]
//! 2. **What is this about?** → [`Subjects`]
//! 3. **What categorical attributes does it have?** → [`tags`](Envelope::tags)
//! 4. **When was it true in the world vs when did we learn it?**
//!    → [`observed_at`](Envelope::observed_at) / [`valid_at`](Envelope::valid_at)
//! 5. **How much do we trust it?** → [`confidence`](Envelope::confidence)
//!
//! The envelope is the same shape for every record type. That uniformity
//! is what makes the query layer simple: every panel filters by subjects,
//! orders by timestamps, and surfaces confidence the same way regardless
//! of whether it's rendering an Observation or an Assertion.

use crate::schema::geometry::{Geometry, Position};
use crate::vocab::{Confidence, CountryCode, EntityId, Topic};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Metadata carried by every record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    /// Where did this record come from?
    pub provenance: Provenance,

    /// What is this record about? Used for subject-filtered queries in the UI.
    pub subjects: Subjects,

    /// Free-form categorical attributes. Lowercase `key:value` pairs.
    /// Examples: `direction:supply_negative`, `severity:high`,
    /// `sentiment:bearish`. Distinct from [`Subjects::topics`]:
    /// topics answer *what is this about*, tags describe *attributes of
    /// the record itself* (its stance, impact, provenance quality).
    #[serde(default)]
    pub tags: Vec<String>,

    /// When was this claim/measurement true in the world?
    ///
    /// For a price observation, this is the exchange timestamp.
    /// For a production number from an annual report, this is the end
    /// of the reporting period. For a news event, this is when the
    /// event occurred (not when it was reported).
    ///
    /// Optional because some records (Entities, some Assertions) have
    /// no meaningful world-time.
    pub valid_at: Option<DateTime<Utc>>,

    /// When did situation_room learn about this?
    ///
    /// Always set. Usually the fetch time. For records imported from
    /// historical dumps, it's the import time.
    pub observed_at: DateTime<Utc>,

    /// How much do we trust this record? See [`Confidence`] for the rubric.
    pub confidence: Confidence,
}

impl Envelope {
    /// Minimal constructor for records with no `valid_at` distinction.
    /// Sets `observed_at = now`, `confidence = 1.0`, empty tags.
    pub fn minimal(provenance: Provenance, subjects: Subjects) -> Self {
        Self {
            provenance,
            subjects,
            tags: Vec::new(),
            valid_at: None,
            observed_at: Utc::now(),
            confidence: Confidence::ONE,
        }
    }
}

// ---------------------------------------------------------------------------
// Subjects — what is this record about?
// ---------------------------------------------------------------------------

/// The subjects a record concerns.
///
/// The design is **four universal dimensions** (entities, places, time,
/// topics), applied uniformly across every domain situation_room is used for.
/// The first three are typed — they answer structural queries well
/// (entity-joins, geographic intersection, time windowing). The fourth,
/// [`topics`](Self::topics), is an open bag of free-form [`Topic`] tags
/// that carries all domain-specific categorization.
///
/// **There is no hardcoded "commodity" dimension** — commodities are
/// topics, just like sectors, technologies, and policy areas. See the
/// module-level documentation of [`crate::vocab`] for the full
/// rationale.
///
/// All fields are vectors because a single record legitimately
/// concerns multiple of each (a trade-flow relation mentions two
/// countries; a joint-venture event names multiple entities; an
/// article discussing chip exports tags both `semiconductors` and
/// `export_controls`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Subjects {
    /// Actors the record concerns: companies, mines, vessels, agencies,
    /// people. The heaviest-queried subject dimension in practice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityId>,

    /// Geographic subjects. See [`PlaceRef`] for the variants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<PlaceRef>,

    /// Temporal scope the record addresses. Optional because many records
    /// are point-in-time (captured by [`Envelope::valid_at`]) and don't
    /// need a scope. Use this when a record is *about* a period or event
    /// rather than *occurring at* one — e.g. "Q3 2025 earnings" or
    /// "the 2028 election cycle".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeScope>,

    /// Open-namespace topical tags. Populated by the research planner
    /// (or by source-specific ingest logic) to categorize what the
    /// record is about. Examples: `"Li"`, `"semiconductors"`,
    /// `"euv_lithography"`, `"tw_2028_presidential"`,
    /// `"ai_export_controls"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<Topic>,
}

impl Subjects {
    /// Build Subjects with a single topic. Convenience for the common
    /// "this record is about one thing" case.
    pub fn topic(t: Topic) -> Self {
        Self {
            topics: vec![t],
            ..Default::default()
        }
    }

    /// Build Subjects with a single entity.
    pub fn entity(e: EntityId) -> Self {
        Self {
            entities: vec![e],
            ..Default::default()
        }
    }

    /// Build Subjects with a single country place.
    pub fn country(c: CountryCode) -> Self {
        Self {
            places: vec![PlaceRef::Country(c)],
            ..Default::default()
        }
    }

    /// True if the subjects overlap on at least one dimension.
    pub fn intersects(&self, other: &Self) -> bool {
        self.entities.iter().any(|e| other.entities.contains(e))
            || self.places.iter().any(|p| other.places.contains(p))
            || self.topics.iter().any(|t| other.topics.contains(t))
    }

    /// True if the subjects are completely empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
            && self.places.is_empty()
            && self.time.is_none()
            && self.topics.is_empty()
    }
}

// ---------------------------------------------------------------------------
// PlaceRef — geographic subjects
// ---------------------------------------------------------------------------

/// A reference to a place. Ranges from coarse (country) to fine (polygon).
///
/// The enum variants are ordered roughly by granularity. A country record
/// can match a polygon-query if the polygon intersects the country, but
/// that's a query-layer concern; the schema just stores what it was told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PlaceRef {
    /// ISO 3166-1 country.
    Country(CountryCode),
    /// Region identifier. Free-form — examples: `"eu"`, `"cis"`, `"latam"`,
    /// `"asean"`, `"pilbara"`, `"lithium_triangle"`. The registry is open,
    /// managed the same way as [`Topic`] — the research planner introduces
    /// region names as needed. No governance.
    Region(String),
    /// Named sub-national place: city, province, basin. Free-form.
    Named(String),
    /// Precise point. Use for asset-level records (a specific mine, a vessel
    /// at a specific moment).
    Point(Position),
    /// Polygon boundary. Use for records scoped to a specific geographic
    /// area with a known shape (a sanction zone, a protected basin).
    Polygon(Geometry),
}

impl Eq for PlaceRef {}

// ---------------------------------------------------------------------------
// TimeScope — temporal subjects
// ---------------------------------------------------------------------------

/// The temporal scope a record addresses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TimeScope {
    /// A specific point in time the record is about. Use when `valid_at`
    /// already captures the when of the record but you want to mark that
    /// the record is *about* a different specific moment (e.g. an Assertion
    /// made today about what happened on a specific historical date).
    Instant(DateTime<Utc>),
    /// A closed time range `[start, end]`.
    Range {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// A named period — free-form identifier. Examples: `"q3_2025"`,
    /// `"2028_election_cycle"`, `"post_covid_recovery"`, `"2020s"`. Use
    /// when the time scope is conceptual (a named era) rather than a
    /// precise range.
    Named(String),
}

// ---------------------------------------------------------------------------
// Provenance — where did this record come from?
// ---------------------------------------------------------------------------

/// Provenance chain. Links a record back to its source in enough detail
/// that the user can verify the claim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// Stable identifier of the source. Matches a `Source::metadata().id`
    /// or the special value `"derived"` for records produced by the
    /// pipeline itself (e.g. promoted Observations, computed aggregates).
    pub source_id: String,

    /// Optional URL pointing to the specific resource that produced this
    /// record. The URL guard will be applied when following it.
    pub source_url: Option<String>,

    /// The source's own timestamp for the item, if known. Often differs
    /// from `observed_at`: a news article published at 10:00 but fetched
    /// at 11:00 has `source_published_at = 10:00`, `observed_at = 11:00`.
    pub source_published_at: Option<DateTime<Utc>>,

    /// License of the data. Determines caching and redistribution behavior.
    /// Stored as a free-form string to stay in sync with `DataLicense` in
    /// `situation_room-sources` without creating a core↔sources circular dep.
    /// Convention: values match one of `DataLicense` variant names in
    /// snake_case ("public_domain", "open_with_attribution", etc.).
    pub license: String,

    /// For derived records: what inputs produced this record?
    /// Empty for directly-fetched records.
    #[serde(default)]
    pub derived_from: Vec<DerivedFrom>,

    /// Session 87: which selector / path / cell matched the leaf
    /// inside the fetched bytes. Populated by `recipe_apply::build_record`
    /// at apply-time; `None` for promoted / derived / LLM-synthesized
    /// records (no recipe selector applies).
    ///
    /// **Format is a closed-vocabulary tag plus the operator-facing
    /// rendering of the selector**:
    ///
    ///   - `"css:#price"`        — CssSelect with selector `#price`
    ///   - `"css:#price[data-v]"` — CssSelect with attribute `data-v`
    ///   - `"json:$.close"`       — JsonPath at path `$.close`
    ///   - `"csv:close@row=3"`     — CsvCell column `close`, row 3
    ///   - `"pdf:p1/t0/r2/c3"`     — PdfTable page/table/row/col
    ///   - `"regex:group=1"`       — RegexCapture group index
    ///
    /// Iterator-mode recipes stamp `"<iter> >> <inner>"` so the
    /// operator can see both selectors at once.
    ///
    /// Closed-vocabulary discipline (no source/host strings, no
    /// human-author free text): the strings above are mode-derived
    /// and stable across recipes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_path: Option<String>,

    /// Session 87: short UTF-8 excerpt of the raw bytes that the
    /// recipe extracted, *as the leaf seen by the binding*. Populated
    /// by `recipe_apply::build_record`; truncated to
    /// [`RAW_BYTES_EXCERPT_CAP`] codepoints. `None` for promoted /
    /// derived / LLM-synthesized records.
    ///
    /// This is the operator-visible answer to "what did the recipe
    /// actually pull from the page?" — useful when the rendered value
    /// (e.g. `613.99`) looks wrong and the operator needs to know
    /// whether the recipe matched the right DOM scalar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_bytes_excerpt: Option<String>,
}

/// Codepoint cap on `Provenance::raw_bytes_excerpt`. Tuned for the
/// MetricDetailDrawer's per-row excerpt cell: 256 codepoints is enough
/// to fit a short scalar (a price, a date, a headline) without
/// inflating the per-record wire payload. Long excerpts get truncated
/// with a trailing `"…"` marker inside the stamper.
pub const RAW_BYTES_EXCERPT_CAP: usize = 256;

/// A link from a derived record back to an input record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DerivedFrom {
    /// The input record's UUID.
    pub record_id: uuid::Uuid,

    /// How did this input contribute?
    pub role: DerivationRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationRole {
    /// This input was an Assertion whose content was promoted into a full record.
    Promotion,
    /// This input was one of multiple supporting sources for a consensus promotion.
    ConsensusSupport,
    /// This input was a Document from which the record was extracted.
    Extraction,
    /// This input contributed to an aggregate or computed record.
    Aggregation,
    /// This input triggered an anomaly record.
    AnomalyTrigger,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::Topic;

    fn test_provenance() -> Provenance {
        Provenance {
            source_id: "usgs_mcs".into(),
            source_url: Some("https://pubs.usgs.gov/publication/mcs2025".into()),
            source_published_at: None,
            license: "public_domain".into(),
            derived_from: Vec::new(),
            selector_path: None,
            raw_bytes_excerpt: None,
        }
    }

    /// Session 87: legacy JSON (pre-selector_path / raw_bytes_excerpt)
    /// must still deserialize. `#[serde(default)]` carries this.
    #[test]
    fn provenance_legacy_json_deserializes_without_new_fields() {
        let legacy = serde_json::json!({
            "source_id": "usgs_mcs",
            "source_url": "https://pubs.usgs.gov/x.pdf",
            "license": "public_domain",
            "derived_from": []
        });
        let p: Provenance = serde_json::from_value(legacy).expect("legacy JSON");
        assert!(p.selector_path.is_none());
        assert!(p.raw_bytes_excerpt.is_none());
    }

    /// Session 87: new fields round-trip through serde when populated.
    #[test]
    fn provenance_with_selector_trace_roundtrips() {
        let p = Provenance {
            source_id: "cnbc".into(),
            source_url: None,
            source_published_at: None,
            license: "fair_use".into(),
            derived_from: Vec::new(),
            selector_path: Some("css:#last-price".into()),
            raw_bytes_excerpt: Some("$613.99".into()),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: Provenance = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    /// Session 87: `skip_serializing_if = "Option::is_none"` keeps the
    /// new fields out of the wire shape when unpopulated.
    #[test]
    fn provenance_omits_none_selector_trace_from_json() {
        let p = test_provenance();
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("selector_path"));
        assert!(!s.contains("raw_bytes_excerpt"));
    }

    #[test]
    fn envelope_minimal_roundtrip() {
        let li = Topic::new("Li").unwrap();
        let env = Envelope::minimal(test_provenance(), Subjects::topic(li.clone()));
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn subjects_intersect_on_topic() {
        let li = Topic::new("Li").unwrap();
        let cu = Topic::new("Cu").unwrap();
        let a = Subjects::topic(li.clone());
        let b = Subjects::topic(li);
        let c = Subjects::topic(cu);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn subjects_intersect_on_entity() {
        let tsmc = EntityId::new("tsmc").unwrap();
        let samsung = EntityId::new("samsung").unwrap();
        let a = Subjects::entity(tsmc.clone());
        let b = Subjects::entity(tsmc);
        let c = Subjects::entity(samsung);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn subjects_intersect_on_place() {
        let tw = CountryCode::new("TW").unwrap();
        let kr = CountryCode::new("KR").unwrap();
        let a = Subjects::country(tw.clone());
        let b = Subjects::country(tw);
        let c = Subjects::country(kr);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn multi_dimensional_record() {
        // A realistic record about TSMC's fab capacity, tagging both
        // semiconductor and export-controls topics, geolocated to Taiwan
        // and Arizona, mentioning entities.
        let subjects = Subjects {
            entities: vec![
                EntityId::new("tsmc").unwrap(),
                EntityId::new("fab:TSMC-Arizona-F21").unwrap(),
            ],
            places: vec![
                PlaceRef::Country(CountryCode::new("TW").unwrap()),
                PlaceRef::Named("Arizona".into()),
            ],
            time: Some(TimeScope::Named("q3_2025".into())),
            topics: vec![
                Topic::new("semiconductors").unwrap(),
                Topic::new("ai_export_controls").unwrap(),
            ],
        };
        let json = serde_json::to_string(&subjects).unwrap();
        let back: Subjects = serde_json::from_str(&json).unwrap();
        assert_eq!(subjects, back);
    }

    #[test]
    fn empty_subjects_omitted_from_json() {
        let subjects = Subjects::default();
        let json = serde_json::to_string(&subjects).unwrap();
        // All fields are empty/none; skip_serializing_if should elide them.
        assert_eq!(json, "{}");
        assert!(subjects.is_empty());
    }

    #[test]
    fn time_scope_variants() {
        let s = TimeScope::Named("q3_2025".into());
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"named\""));
        let back: TimeScope = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);

        let r = TimeScope::Range {
            start: Utc::now(),
            end: Utc::now(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"kind\":\"range\""));
    }

    #[test]
    fn place_ref_variants() {
        let p = PlaceRef::Country(CountryCode::new("TW").unwrap());
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"kind\":\"country\""));
        let back: PlaceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);

        let n = PlaceRef::Named("Pilbara".into());
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"kind\":\"named\""));
    }

    #[test]
    fn provenance_with_derivation() {
        let mut p = test_provenance();
        p.source_id = "derived".into();
        p.derived_from.push(DerivedFrom {
            record_id: uuid::Uuid::nil(),
            role: DerivationRole::Promotion,
        });
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("promotion"));
    }
}
