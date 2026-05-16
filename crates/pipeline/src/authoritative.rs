//! Authoritative-source registry (ADR 0004 pathway 1, Session 82).
//!
//! # What this module ships
//!
//! Loads `config/vocab/authoritative_sources.toml` into a typed
//! [`AuthorityRegistry`] and exposes a `matches(&Assertion)` predicate
//! the promotion stage uses to decide whether a single-source claim
//! gets fast-tracked into a promoted record (N=1) instead of waiting
//! for consensus (N=3).
//!
//! ADR 0004 names the registry as configuration, not code:
//!
//! > Authoritative status is *per-content-type × per-subject*, not
//! > global. USGS is authoritative for US mineral production, not
//! > authoritative for stock prices. The LME is authoritative for
//! > copper warehouse stocks, not authoritative for policy events.
//! > The authoritative registry is configuration, not code, and lives
//! > in `config/authoritative.toml` (to be created in Phase 3).
//!
//! Session 82 reuses the pre-existing
//! `config/vocab/authoritative_sources.toml` location (the file has
//! been in-tree since the early commodities vocabularies) so we don't
//! introduce a parallel path. The schema below matches what's already
//! on disk.
//!
//! # TOML schema
//!
//! ```toml
//! [[authority]]
//! source_id = "usgs_mcs"
//! metric    = "production"   # optional
//! topic     = "Cu"           # optional
//! ```
//!
//! `source_id` is the *trailing* portion of a claimant `EntityId`:
//! `agency:usgs_mcs` claimants match a `source_id = "usgs_mcs"`
//! entry. The matcher also accepts the bare form (`"usgs_mcs"` →
//! `"usgs_mcs"`) so a config author can write either shape.
//!
//! `metric` matches:
//! - `ObservationContent.metric` (e.g. `"production"`),
//! - `EventContent.event_type` (e.g. `"export_restriction"`),
//! - `RelationContent.kind` (e.g. `"supplies_to"`),
//! - `EntityAttributeContent.key` (e.g. `"legal_name"`).
//!
//! `topic` matches any string in `Envelope::subjects.topics`.
//!
//! Both `metric` and `topic` are optional. An entry with neither is a
//! "this source is authoritative for everything it claims" override;
//! use sparingly because it bypasses the per-(content-type × subject)
//! discipline ADR 0004 names.
//!
//! # Closed-vocabulary posture
//!
//! No host strings. No URL routing. The match is on the closed
//! `claimant` namespace already enforced by the storage layer. This
//! module is read-only configuration — adding a new authoritative
//! source means editing the TOML, not the Rust source.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use tracing::{info, warn};

use situation_room_core::schema::content::AssertedContent;
use situation_room_core::schema::records::Assertion;

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

/// One row in `authoritative_sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityEntry {
    pub source_id: String,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
}

/// Wrapper for the TOML file: `[[authority]]` table arrays.
#[derive(Debug, Deserialize)]
struct AuthorityFile {
    #[serde(default, rename = "authority")]
    entries: Vec<AuthorityEntry>,
}

#[derive(Debug, Error)]
pub enum AuthorityLoadError {
    #[error("authoritative-sources file read failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("authoritative-sources file parse failed: {0}")]
    Parse(#[from] toml::de::Error),
}

// ---------------------------------------------------------------------------
// AuthorityRegistry — in-memory representation
// ---------------------------------------------------------------------------

/// In-memory authority registry. Construct via [`AuthorityRegistry::load_from_path`]
/// at the composition root; pass it to [`crate::promote`] via
/// `PromoteConfig::authoritative`.
///
/// `AuthorityRegistry::empty()` matches no claimants — pass this in
/// tests and from contexts that don't have a config file. Session 82
/// preserves the Session 81 behaviour when the registry is empty:
/// `promote_for_plan` runs the consensus pass only.
#[derive(Debug, Clone, Default)]
pub struct AuthorityRegistry {
    entries: Vec<AuthorityEntry>,
}

impl AuthorityRegistry {
    /// Empty registry — the default. Matches no Assertions; the
    /// promote stage runs consensus-only (Session 81 behaviour).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Read-only view of the configured entries. Used by the
    /// `authoritative_registry_summary` IPC surface so the dashboard
    /// can show "5 authoritative entries loaded."
    pub fn entries(&self) -> &[AuthorityEntry] {
        &self.entries
    }

    /// True iff the registry has no entries — consumers (the promote
    /// stage) skip the authoritative pass entirely in this case.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build a registry directly from typed entries (test sites + the
    /// rare in-code injection case).
    pub fn from_entries(entries: Vec<AuthorityEntry>) -> Self {
        Self { entries }
    }

    /// Read `path` and parse it as the authoritative-sources file.
    /// Returns `Ok(empty)` if the file is missing — production binaries
    /// should warn-log a missing file but continue (the registry is
    /// optional configuration).
    pub fn load_from_path(path: &Path) -> Result<Self, AuthorityLoadError> {
        let bytes = std::fs::read_to_string(path)?;
        Self::parse(&bytes)
    }

    /// Parse TOML text. Exposed for unit tests that don't want to
    /// touch the filesystem.
    pub fn parse(s: &str) -> Result<Self, AuthorityLoadError> {
        let f: AuthorityFile = toml::from_str(s)?;
        // Sanitise: drop entries with empty source_id. They'd match no
        // claimant anyway; the warn-log surfaces the typo to the
        // operator without aborting the whole load.
        let mut kept: Vec<AuthorityEntry> = Vec::with_capacity(f.entries.len());
        for entry in f.entries {
            if entry.source_id.trim().is_empty() {
                warn!(
                    metric = ?entry.metric,
                    topic = ?entry.topic,
                    "authoritative entry with empty source_id ignored"
                );
                continue;
            }
            kept.push(entry);
        }
        info!(
            count = kept.len(),
            "authoritative-source registry loaded"
        );
        Ok(Self { entries: kept })
    }

    /// True iff this Assertion's claimant matches any entry's
    /// `source_id`, AND any declared `metric` matches the content's
    /// natural metric/key, AND any declared `topic` is in the
    /// envelope's `subjects.topics`. An entry with no metric or topic
    /// gate accepts any content/subject for the matching claimant.
    pub fn matches(&self, a: &Assertion) -> bool {
        let claimant = a.claimant.as_str();
        self.entries.iter().any(|entry| {
            claimant_matches(claimant, &entry.source_id)
                && entry
                    .metric
                    .as_deref()
                    .map_or(true, |m| content_metric_matches(&a.content, m))
                && entry.topic.as_deref().map_or(true, |t| {
                    a.envelope.subjects.topics.iter().any(|topic| topic.as_str() == t)
                })
        })
    }
}

// ---------------------------------------------------------------------------
// Matchers — kept private; AuthorityRegistry exposes only `matches`
// ---------------------------------------------------------------------------

/// Closed-vocabulary claimant match. Equality on the trailing portion
/// of the claimant `EntityId`. The well-known namespace prefixes
/// (`agency:`, `publisher:`, `source:`) are stripped from the
/// claimant side before comparing, so a TOML author can write
/// `"usgs_mcs"` and the matcher accepts both `agency:usgs_mcs` and
/// the bare form.
///
/// Match is case-insensitive on ASCII alphanumerics — the storage
/// layer's `EntityId` parser normalises the case at insert time, so
/// this is a defence in depth rather than load-bearing.
fn claimant_matches(claimant: &str, source_id: &str) -> bool {
    if claimant.eq_ignore_ascii_case(source_id) {
        return true;
    }
    for prefix in ["agency:", "publisher:", "source:"] {
        if let Some(rest) = claimant.strip_prefix(prefix) {
            if rest.eq_ignore_ascii_case(source_id) {
                return true;
            }
        }
    }
    false
}

/// Per-content-type "natural metric" extraction. Mirrors the four
/// `AssertedContent` arms. ObservationContent.metric is the obvious
/// one; for the other three we use the closed-vocabulary field that
/// distinguishes one row's claim from another within the same shape.
fn content_metric_matches(content: &AssertedContent, metric: &str) -> bool {
    match content {
        AssertedContent::Observation(c) => c.metric == metric,
        AssertedContent::Event(c) => c.event_type.as_str() == metric,
        AssertedContent::Relation(c) => c.kind == metric,
        AssertedContent::EntityAttribute(c) => c.key == metric,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use situation_room_core::schema::content::{
        AssertedContent, AttributeValue, EntityAttributeContent, EventContent, ObservationContent,
        ObservationPeriod, RelationContent,
    };
    use situation_room_core::schema::envelope::{Envelope, Provenance, Subjects};
    use situation_room_core::vocab::{Confidence, EntityId, EventType, Stance, Topic, Unit};

    fn env(claimant: &str, topic: &str) -> Envelope {
        let _ = claimant;
        Envelope {
            provenance: Provenance {
                source_id: "test".into(),
                source_url: None,
                source_published_at: None,
                license: "extracted".into(),
                derived_from: vec![],
            },
            subjects: Subjects {
                entities: vec![],
                places: vec![],
                time: None,
                topics: if topic.is_empty() {
                    vec![]
                } else {
                    vec![Topic::new(topic).unwrap()]
                },
            },
            tags: vec![],
            valid_at: None,
            observed_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
            confidence: Confidence::clamp(0.9),
        }
    }

    fn obs_assertion(claimant: &str, metric: &str, topic: &str) -> Assertion {
        Assertion::new(
            EntityId::new(claimant).unwrap(),
            Stance::Reported,
            AssertedContent::Observation(ObservationContent {
                metric: metric.into(),
                value: 142_000.0,
                unit: Unit::new("t").unwrap(),
                value_uncertainty: None,
                currency: None,
                period: ObservationPeriod::Annual,
                geometry: None,
            }),
            env(claimant, topic),
        )
    }

    #[test]
    fn empty_registry_matches_nothing() {
        let r = AuthorityRegistry::empty();
        assert!(r.is_empty());
        let a = obs_assertion("agency:usgs_mcs", "production", "lithium");
        assert!(!r.matches(&a));
    }

    #[test]
    fn parse_handles_existing_authoritative_sources_toml_shape() {
        // The exact byte sequence currently sitting in
        // config/vocab/authoritative_sources.toml. Parsing this without
        // a config tweak is the load-bearing compatibility guarantee.
        let s = r#"
[[authority]]
source_id = "usgs_mcs"
metric = "production"

[[authority]]
source_id = "usgs_mcs"
metric = "reserves"

[[authority]]
source_id = "lme_warehouse"
metric = "warehouse_stock"
topic = "Cu"
"#;
        let r = AuthorityRegistry::parse(s).expect("toml parses");
        assert_eq!(r.entries().len(), 3);
        assert_eq!(r.entries()[2].topic.as_deref(), Some("Cu"));
    }

    #[test]
    fn claimant_with_prefix_matches_bare_source_id() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
        }]);
        let a = obs_assertion("agency:usgs_mcs", "production", "lithium");
        assert!(r.matches(&a));
    }

    #[test]
    fn bare_claimant_matches_bare_source_id() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: None,
            topic: None,
        }]);
        let a = obs_assertion("usgs_mcs", "production", "");
        assert!(r.matches(&a));
    }

    #[test]
    fn metric_mismatch_rejects() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
        }]);
        let a = obs_assertion("agency:usgs_mcs", "reserves", "lithium");
        assert!(!r.matches(&a));
    }

    #[test]
    fn topic_gate_requires_match() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "lme_warehouse".into(),
            metric: Some("warehouse_stock".into()),
            topic: Some("Cu".into()),
        }]);
        // Copper assertion matches.
        let cu = obs_assertion("agency:lme_warehouse", "warehouse_stock", "Cu");
        assert!(r.matches(&cu));
        // Aluminium assertion does not — different topic.
        let al = obs_assertion("agency:lme_warehouse", "warehouse_stock", "Al");
        assert!(!r.matches(&al));
        // Missing topic on the assertion side: gate fails closed.
        let bare = obs_assertion("agency:lme_warehouse", "warehouse_stock", "");
        assert!(!r.matches(&bare));
    }

    #[test]
    fn event_type_acts_as_metric_for_event_content() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:sec".into(),
            metric: Some("export_restriction".into()),
            topic: None,
        }]);
        let event = Assertion::new(
            EntityId::new("agency:sec").unwrap(),
            Stance::Asserted,
            AssertedContent::Event(EventContent {
                event_type: EventType::new("export_restriction").unwrap(),
                headline: "Country X tightens lithium export quotas".into(),
                actors: vec![],
                direction: None,
                magnitude: None,
                geometry: None,
            }),
            env("agency:sec", "lithium"),
        );
        assert!(r.matches(&event));
    }

    #[test]
    fn relation_kind_acts_as_metric_for_relation_content() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:edgar".into(),
            metric: Some("ownership".into()),
            topic: None,
        }]);
        let rel = Assertion::new(
            EntityId::new("agency:edgar").unwrap(),
            Stance::Asserted,
            AssertedContent::Relation(RelationContent {
                kind: "ownership".into(),
                from: EntityId::new("company:a").unwrap(),
                to: EntityId::new("company:b").unwrap(),
                magnitude: None,
                valid_until: None,
            }),
            env("agency:edgar", ""),
        );
        assert!(r.matches(&rel));
    }

    #[test]
    fn entity_attribute_key_acts_as_metric() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:edgar".into(),
            metric: Some("legal_name".into()),
            topic: None,
        }]);
        let attr = Assertion::new(
            EntityId::new("agency:edgar").unwrap(),
            Stance::Asserted,
            AssertedContent::EntityAttribute(EntityAttributeContent {
                entity_id: EntityId::new("company:tsla").unwrap(),
                key: "legal_name".into(),
                value: AttributeValue::Text("Tesla, Inc.".into()),
            }),
            env("agency:edgar", ""),
        );
        assert!(r.matches(&attr));
    }

    #[test]
    fn empty_source_id_entry_is_dropped_with_warn() {
        let s = r#"
[[authority]]
source_id = ""
metric = "production"

[[authority]]
source_id = "usgs_mcs"
metric = "reserves"
"#;
        let r = AuthorityRegistry::parse(s).unwrap();
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].source_id, "usgs_mcs");
    }
}
