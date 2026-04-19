//! Controlled vocabularies.
//!
//! These are the typed identifiers Stockpile uses for commodities, countries,
//! event types, units, currencies, and stance. They reuse existing standards
//! wherever possible (ISO 3166 for countries, ISO 4217 for currencies, UCUM
//! for units) to keep the system interoperable.
//!
//! Phase 2 will define the actual enums/newtypes. This stub establishes the
//! module so other crates can reference the path.

use serde::{Deserialize, Serialize};

/// Commodity identifier — element symbol or controlled code.
/// e.g. "Li", "Cu", "Nd". Phase 2 will make this a proper newtype with validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommodityId(pub String);

/// ISO 3166-1 alpha-2 country code. e.g. "CL", "AU", "ID".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CountryCode(pub String);

/// Stable identifier for an Entity (mine, company, vessel, port, person).
/// Use LEI for companies where available; project-specific slug otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityId(pub String);

/// Controlled event type vocabulary. Phase 2 will make this an enum with
/// the full taxonomy (export_restriction, production_cut, force_majeure, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventType(pub String);

/// UCUM unit string. e.g. "t" (metric ton), "USD/t", "kt/yr".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Unit(pub String);

/// ISO 4217 currency code. e.g. "USD", "CNY", "EUR".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Currency(pub String);

/// Claimant stance toward an Assertion's content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Stance {
    Asserted,
    Hedged,
    Denied,
    Reported,
    Predicted,
    Speculated,
}
