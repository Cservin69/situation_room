//! Controlled vocabularies.
//!
//! These are the typed identifiers Stockpile uses for countries, entities,
//! event types, units, currencies, topics, stance, and confidence. They
//! reuse existing standards wherever possible:
//!
//! - **Countries**: ISO 3166-1 alpha-2 (e.g. "CL", "AU", "CN")
//! - **Currencies**: ISO 4217 (e.g. "USD", "EUR", "CNY")
//! - **Units**: UCUM (Unified Code for Units of Measure). Common ones:
//!   "t" (metric ton), "kt" (kiloton), "USD/t", "%", "1" (dimensionless)
//!
//! ## On topics (and why there is no `CommodityId`)
//!
//! Stockpile does not hardcode a schema for "commodities", "sectors",
//! "technologies", "elections", etc. The six record types
//! ([Observation](crate::schema::records::Observation),
//! [Event](crate::schema::records::Event), [Entity](crate::schema::records::Entity),
//! [Relation](crate::schema::records::Relation), [Document](crate::schema::records::Document),
//! [Assertion](crate::schema::records::Assertion)) are the universal
//! schema. Topical categorization — "this record is about lithium /
//! semiconductors / Taiwan's 2028 election" — happens through free-form
//! [`Topic`] tags that live in the record's
//! [`Subjects`](crate::schema::envelope::Subjects).
//!
//! The LLM research-planner populates these tags at classification time
//! (see `pipeline::research`). It is instructed to reuse existing topic
//! strings across sessions, but the topic namespace is open — no
//! registry, no governance, no schema-per-domain. The six record types
//! are the governance.
//!
//! This keeps the schema light and the project fully general: you can
//! research any subject that decomposes into facts.
//!
//! Vocabularies that *do* have registries (countries, currencies) keep
//! their validation here. Vocabularies that are user-curated lists
//! (commodity seed list, event type taxonomy) live in `config/vocab/`
//! as plain data, not as code.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Validation errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum VocabError {
    #[error("country code must be ISO 3166-1 alpha-2 (two uppercase letters): got {0:?}")]
    InvalidCountry(String),
    #[error("entity id must be non-empty (1-128 chars): got {0:?}")]
    InvalidEntity(String),
    #[error("event type must be lowercase_snake_case (1-64 chars): got {0:?}")]
    InvalidEventType(String),
    #[error("unit must be non-empty UCUM-compatible string (1-32 chars): got {0:?}")]
    InvalidUnit(String),
    #[error("currency must be ISO 4217 alpha code (three uppercase letters): got {0:?}")]
    InvalidCurrency(String),
    #[error("topic must be non-empty ASCII (1-64 chars, alphanumeric + _ / -): got {0:?}")]
    InvalidTopic(String),
}

// ---------------------------------------------------------------------------
// Topic — the general-purpose subject tag.
// ---------------------------------------------------------------------------

/// A free-form topic tag for categorizing records.
///
/// Examples: `"Li"` (lithium, the commodity), `"semiconductors"` (a sector),
/// `"euv_lithography"` (a technology), `"tw_2028_presidential"` (an event),
/// `"ai_export_controls"` (a policy area), `"co2_removal"` (a scientific
/// domain).
///
/// Validation is intentionally permissive (ASCII alphanumeric plus `_` and
/// `-`, 1–64 chars) because the namespace is user/LLM-curated per research
/// session, not schema-enforced. The research planner is instructed via
/// prompt to reuse existing strings when possible; synonym drift is managed
/// there, not in the type system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Topic(String);

impl Topic {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.is_empty()
            || s.len() > 64
            || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(VocabError::InvalidTopic(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Country (ISO 3166-1 alpha-2)
// ---------------------------------------------------------------------------

/// ISO 3166-1 alpha-2 country code. Always two uppercase ASCII letters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CountryCode(String);

impl CountryCode {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.len() != 2 || !s.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(VocabError::InvalidCountry(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CountryCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Entity ID
// ---------------------------------------------------------------------------

/// Stable identifier for an Entity (company, mine, vessel, port, person,
/// agency, fab, facility, etc.). Use LEI when available for companies,
/// project slugs ("sqm", "tsmc", "mine_kathleen_valley") otherwise.
///
/// Validation: non-empty, ≤128 chars, ASCII-printable minus whitespace.
/// Deliberately more permissive than other vocabs because entity naming
/// is messy in practice.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(String);

impl EntityId {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.is_empty()
            || s.len() > 128
            || s.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(VocabError::InvalidEntity(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Event type
// ---------------------------------------------------------------------------

/// Event type. Lowercase snake_case, drawn from the controlled vocabulary
/// in `config/vocab/event_types.toml`. Examples: "export_restriction",
/// "production_cut", "force_majeure", "guidance_change", "fab_announcement",
/// "election_result".
///
/// Like [`Topic`], the event type namespace is open — the research planner
/// introduces new types as needed. The config file is a seed and a naming
/// suggestion, not a hard allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventType(String);

impl EventType {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.is_empty()
            || s.len() > 64
            || !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(VocabError::InvalidEventType(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Unit (UCUM)
// ---------------------------------------------------------------------------

/// UCUM-compatible unit string. We don't validate against the full UCUM
/// grammar (that would require a dependency) — we accept any non-empty
/// string with printable ASCII. Consumers can validate more strictly.
///
/// Common units in Stockpile: "t" (metric ton), "kt" (kiloton),
/// "USD/t" (price), "%" (percent), "1" (dimensionless),
/// "USD" (raw currency amount), "units/month", "MWh".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Unit(String);

impl Unit {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.is_empty()
            || s.len() > 32
            || s.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(VocabError::InvalidUnit(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Currency (ISO 4217)
// ---------------------------------------------------------------------------

/// ISO 4217 alpha currency code. Always three uppercase ASCII letters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Currency(String);

impl Currency {
    pub fn new(s: impl Into<String>) -> Result<Self, VocabError> {
        let s = s.into();
        if s.len() != 3 || !s.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(VocabError::InvalidCurrency(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Stance
// ---------------------------------------------------------------------------

/// The claimant's stance toward an Assertion's content.
///
/// Captures linguistic markers in the source: "will ban" → `Predicted`,
/// "may ban" → `Hedged`, "banned" → `Asserted`, "rumors suggest" → `Reported`,
/// "speculation that" → `Speculated`, "denied plans to ban" → `Denied`.
///
/// Matters for anomaly detection: a flurry of `Predicted`/`Speculated`
/// claims contradicted by the absence of `Asserted` ones is a signature
/// of rumor-driven market moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    Asserted,
    Hedged,
    Denied,
    Reported,
    Predicted,
    Speculated,
}

impl std::fmt::Display for Stance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Asserted => "asserted",
            Self::Hedged => "hedged",
            Self::Denied => "denied",
            Self::Reported => "reported",
            Self::Predicted => "predicted",
            Self::Speculated => "speculated",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Confidence
// ---------------------------------------------------------------------------

/// Confidence in a claim or observation, in the closed range [0.0, 1.0].
///
/// Conventions (see docs/schema/envelope.md for the full rubric):
/// - 1.0: direct measurement from authoritative source (LME warehouse stocks).
/// - 0.9: named official on-the-record statement.
/// - 0.7: structured data from reputable secondary reporting.
/// - 0.5: analyst speculation, hedged reporting.
/// - 0.3: anonymous sources, rumor.
/// - 0.0: explicitly unverified (e.g. extracted from low-credibility feed).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    /// Construct from f32, validating the range and rejecting NaN/Inf.
    pub fn new(v: f32) -> Result<Self, ConfidenceError> {
        if !v.is_finite() {
            return Err(ConfidenceError::NotFinite);
        }
        if !(0.0..=1.0).contains(&v) {
            return Err(ConfidenceError::OutOfRange(v));
        }
        Ok(Self(v))
    }

    /// Saturating constructor — clamps to [0.0, 1.0] instead of erroring.
    /// Useful at ingest boundaries where we'd rather accept noisy input.
    pub fn clamp(v: f32) -> Self {
        if !v.is_finite() {
            return Self(0.0);
        }
        Self(v.clamp(0.0, 1.0))
    }

    pub fn value(&self) -> f32 {
        self.0
    }

    pub const ZERO: Self = Self(0.0);
    pub const ONE: Self = Self(1.0);
}

impl Eq for Confidence {}
impl std::hash::Hash for Confidence {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

#[derive(Debug, Error)]
pub enum ConfidenceError {
    #[error("confidence must be in [0.0, 1.0], got {0}")]
    OutOfRange(f32),
    #[error("confidence must be finite, got NaN or Inf")]
    NotFinite,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_accepts_common() {
        for s in ["Li", "semiconductors", "euv_lithography", "tw_2028", "co2-removal", "Cu"] {
            assert!(Topic::new(s).is_ok(), "should accept {s}");
        }
    }

    #[test]
    fn topic_rejects_bad() {
        for s in ["", "has spaces", "ünicode", &"x".repeat(65), "with/slash"] {
            assert!(Topic::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn country_code_strict() {
        assert!(CountryCode::new("CL").is_ok());
        assert!(CountryCode::new("US").is_ok());
        assert!(CountryCode::new("cl").is_err()); // lowercase
        assert!(CountryCode::new("USA").is_err()); // alpha-3
        assert!(CountryCode::new("C").is_err()); // too short
        assert!(CountryCode::new("").is_err());
    }

    #[test]
    fn currency_strict() {
        assert!(Currency::new("USD").is_ok());
        assert!(Currency::new("EUR").is_ok());
        assert!(Currency::new("usd").is_err());
        assert!(Currency::new("DOLLAR").is_err());
    }

    #[test]
    fn event_type_snake_case() {
        assert!(EventType::new("export_restriction").is_ok());
        assert!(EventType::new("production_cut_q3").is_ok());
        assert!(EventType::new("Export_Restriction").is_err());
        assert!(EventType::new("export restriction").is_err());
    }

    #[test]
    fn unit_permissive() {
        for s in ["t", "kt", "USD/t", "%", "1", "mol/L", "m^3", "MWh"] {
            assert!(Unit::new(s).is_ok(), "should accept {s}");
        }
        assert!(Unit::new("").is_err());
        assert!(Unit::new("kt\n").is_err());
    }

    #[test]
    fn entity_id_allows_real_world_messiness() {
        for s in [
            "sqm",
            "albemarle",
            "tsmc",
            "LEI:549300PPXHEU2JF0AM85",
            "mine_kathleen_valley",
            "vessel:IMO-9876543",
            "fab:TSMC-Arizona-F21",
        ] {
            assert!(EntityId::new(s).is_ok(), "should accept {s:?}");
        }
        assert!(EntityId::new("").is_err());
        assert!(EntityId::new("has spaces").is_err());
        assert!(EntityId::new("x".repeat(200)).is_err());
    }

    #[test]
    fn confidence_valid_range() {
        assert!(Confidence::new(0.0).is_ok());
        assert!(Confidence::new(1.0).is_ok());
        assert!(Confidence::new(0.5).is_ok());
        assert!(Confidence::new(1.01).is_err());
        assert!(Confidence::new(-0.01).is_err());
        assert!(Confidence::new(f32::NAN).is_err());
        assert!(Confidence::new(f32::INFINITY).is_err());
    }

    #[test]
    fn confidence_clamp_forgiving() {
        assert_eq!(Confidence::clamp(1.5).value(), 1.0);
        assert_eq!(Confidence::clamp(-0.1).value(), 0.0);
        assert_eq!(Confidence::clamp(f32::NAN).value(), 0.0);
        assert_eq!(Confidence::clamp(0.75).value(), 0.75);
    }

    #[test]
    fn stance_serialization_is_lowercase() {
        let s = serde_json::to_string(&Stance::Asserted).unwrap();
        assert_eq!(s, "\"asserted\"");
        let s: Stance = serde_json::from_str("\"hedged\"").unwrap();
        assert_eq!(s, Stance::Hedged);
    }

    #[test]
    fn newtype_serialization_is_transparent() {
        let t = Topic::new("Li").unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"Li\"");
        let t: Topic = serde_json::from_str("\"semiconductors\"").unwrap();
        assert_eq!(t.as_str(), "semiconductors");
    }
}
