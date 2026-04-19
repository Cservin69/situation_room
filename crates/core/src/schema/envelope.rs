//! The envelope.
//!
//! Every record in Stockpile carries this metadata. It answers five
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

use crate::vocab::{CommodityId, Confidence, CountryCode, EntityId};
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
    /// `sentiment:bearish`. The controlled set lives in
    /// `config/vocab/` — but new tags can be added without code change.
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

    /// When did Stockpile learn about this?
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
    /// `stockpile-sources` without creating a core↔sources circular dep.
    /// Convention: values match one of `DataLicense` variant names in
    /// snake_case ("public_domain", "open_with_attribution", etc.).
    pub license: String,

    /// For derived records: what inputs produced this record?
    /// Empty for directly-fetched records.
    #[serde(default)]
    pub derived_from: Vec<DerivedFrom>,
}

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
// Subjects — what is this record about?
// ---------------------------------------------------------------------------

/// The subjects a record concerns. The UI uses these for filtering and
/// cross-referencing: "show me everything about Chile" or "everything
/// about copper mentioning Chinese entities."
///
/// Empty fields are legal and common: a price observation has exactly
/// one commodity and no country; a sanctions event has one country and
/// no commodity.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Subjects {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commodities: Vec<CommodityId>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub countries: Vec<CountryCode>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityId>,
}

impl Subjects {
    /// Build a Subjects containing exactly one commodity.
    pub fn commodity(c: CommodityId) -> Self {
        Self {
            commodities: vec![c],
            ..Default::default()
        }
    }

    /// True if the subjects overlap on at least one commodity/country/entity.
    pub fn intersects(&self, other: &Self) -> bool {
        self.commodities.iter().any(|c| other.commodities.contains(c))
            || self.countries.iter().any(|c| other.countries.contains(c))
            || self.entities.iter().any(|e| other.entities.contains(e))
    }

    /// True if the subjects are completely empty.
    pub fn is_empty(&self) -> bool {
        self.commodities.is_empty() && self.countries.is_empty() && self.entities.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::CommodityId;

    fn test_provenance() -> Provenance {
        Provenance {
            source_id: "usgs_mcs".into(),
            source_url: Some("https://pubs.usgs.gov/publication/mcs2025".into()),
            source_published_at: None,
            license: "public_domain".into(),
            derived_from: Vec::new(),
        }
    }

    #[test]
    fn envelope_minimal_roundtrip() {
        let li = CommodityId::new("Li").unwrap();
        let env = Envelope::minimal(test_provenance(), Subjects::commodity(li.clone()));
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn subjects_intersects() {
        let li = CommodityId::new("Li").unwrap();
        let cu = CommodityId::new("Cu").unwrap();
        let a = Subjects::commodity(li.clone());
        let b = Subjects::commodity(li);
        let c = Subjects::commodity(cu);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
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

    #[test]
    fn empty_subjects_omitted_from_json() {
        let subjects = Subjects::default();
        let json = serde_json::to_string(&subjects).unwrap();
        // All three fields are empty; skip_serializing_if should elide them.
        assert_eq!(json, "{}");
    }
}
