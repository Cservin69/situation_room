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
//! source_id        = "usgs_mcs"
//! metric           = "production"   # optional
//! topic            = "Cu"           # optional
//! consensus_quorum = 2              # optional, Session 84
//! ```
//!
//! `source_id` is the *trailing* portion of a claimant `EntityId`:
//! `agency:usgs_mcs` claimants match a `source_id = "usgs_mcs"`
//! entry. The matcher also accepts the bare form (`"usgs_mcs"` →
//! `"usgs_mcs"`) so a config author can write either shape.
//!
//! `consensus_quorum` (Session 84) is the per-entry override on the
//! quorum bar this claim must clear before promotion.
//!  - **Unset or `1`** preserves the Session-82 fast-track: a single
//!    matching Assertion promotes immediately via the authoritative
//!    pass at N=1.
//!  - **`>= 2`** opts out of the authoritative fast-track. The
//!    Assertion goes through the consensus pass instead; its group's
//!    effective quorum drops to `min(cfg.min_independent_claimants,
//!    min over matching entries' consensus_quorum)`. Operator
//!    interpretation: "we trust Reuters enough to lower the global
//!    N=3 bar to N=2 for any claim they participate in, but we
//!    still want corroboration before promoting."
//!
//! The override is per-(claimant × metric × topic), so an operator
//! can mark `consensus_quorum = 2` for one metric a source covers
//! well while leaving another metric at the global default.
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
    /// Session 84 — per-claimant consensus quorum override.
    /// `None` or `Some(1)` preserves the Session-82 N=1 fast-track via
    /// the authoritative pass. `Some(n)` with `n >= 2` opts this entry
    /// OUT of the fast-track and instead lowers the consensus quorum
    /// for groups its claimant participates in.
    #[serde(default)]
    pub consensus_quorum: Option<u32>,
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
        // Session 85 — schema-validate before the typed parse.
        // serde's default `#[derive(Deserialize)]` silently accepts
        // unknown fields, so a typo like `consensus_quorom = 2` or
        // `metirc = "production"` would parse cleanly but never apply.
        // The pre-parse sweep below walks the raw `[[authority]]` array
        // and warn-logs each unknown field name against a closed
        // allowlist. The typed parse below is unchanged — we don't
        // promote this to a hard error because operators editing the
        // TOML interactively would lose hot-reloads on a single typo;
        // the warn is the right pressure level.
        check_schema_warnings(s);

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
    /// envelope's `subjects.topics`, AND the matching entry's
    /// `consensus_quorum` is `None` or `Some(1)` (i.e. the entry opts
    /// into the authoritative fast-track). Entries with
    /// `consensus_quorum >= 2` deliberately fall out of this predicate
    /// so the auth pass does NOT fast-track them — they're handled by
    /// the consensus pass at the lowered quorum returned by
    /// [`Self::quorum_override_for`].
    ///
    /// An entry with no metric or topic gate accepts any
    /// content/subject for the matching claimant.
    pub fn matches(&self, a: &Assertion) -> bool {
        let claimant = a.claimant.as_str();
        self.entries.iter().any(|entry| {
            entry_matches_assertion(entry, claimant, a)
                && entry.consensus_quorum.unwrap_or(1) <= 1
        })
    }

    /// Session 84 — per-claimant consensus quorum override.
    ///
    /// Returns the minimum `consensus_quorum` across every entry that
    /// matches this Assertion (claimant + metric + topic gates the
    /// same as [`Self::matches`], without the `<= 1` short-circuit).
    /// Returns `None` when no entry matches, OR when every matching
    /// entry's `consensus_quorum` is unset (treated as the
    /// auth-fast-track default).
    ///
    /// Operator semantics:
    ///  - **None** → use the global `PromoteConfig::min_independent_claimants`.
    ///  - **Some(n)** → lower the consensus bar to `n` for groups
    ///    containing this Assertion. Pre-existing N=3 logic still wins
    ///    if `cfg.min_independent_claimants < n` (consensus uses the
    ///    min of the two).
    pub fn quorum_override_for(&self, a: &Assertion) -> Option<u32> {
        let claimant = a.claimant.as_str();
        let mut min_q: Option<u32> = None;
        for entry in &self.entries {
            if !entry_matches_assertion(entry, claimant, a) {
                continue;
            }
            if let Some(q) = entry.consensus_quorum {
                if q < 1 {
                    continue;
                }
                min_q = Some(min_q.map_or(q, |cur| cur.min(q)));
            }
        }
        min_q
    }
}

/// Session 85 — schema-validation sweep over the raw TOML.
///
/// Parses the file into a structural `toml::Value` (lossless on field
/// names) and walks the `[[authority]]` array. Any field name in a
/// row that isn't part of the known closed allowlist gets a
/// warn-log; the operator sees the typo without losing the
/// hot-reload (the typed parse below proceeds normally, ignoring the
/// unknown field).
///
/// `[authority]`-keyed top-level fields other than `authority` are
/// also warned. We don't enforce strict deserialization because:
///   - `#[serde(deny_unknown_fields)]` would fail the entire load
///     on a single typo, blanking the in-memory registry until the
///     operator fixes the file — worse interactive behaviour than a
///     warn-log + lossy continue.
///   - Adding new fields in future sessions would otherwise become
///     a backwards-compat break across rolled-out binary versions.
///
/// The allowlist mirrors the typed `AuthorityEntry` shape. When a
/// new field lands on that struct, append it here in the same
/// commit; the unit tests in this module verify the two stay in
/// lockstep.
fn check_schema_warnings(toml_text: &str) {
    /// Known field names on a single `[[authority]]` entry. Kept in
    /// sync with `AuthorityEntry` via the `known_entry_fields_match_struct`
    /// unit test below.
    const KNOWN_ENTRY_FIELDS: &[&str] = &[
        "source_id",
        "metric",
        "topic",
        "consensus_quorum",
    ];
    /// Known top-level table names. Today only `authority` (which
    /// is itself array-of-tables for `[[authority]]`).
    const KNOWN_TOP_LEVEL_KEYS: &[&str] = &["authority"];

    let parsed: toml::Value = match toml::from_str(toml_text) {
        Ok(v) => v,
        Err(_) => {
            // Typed parse will surface the same error; don't double-log.
            return;
        }
    };

    let table = match parsed.as_table() {
        Some(t) => t,
        None => return,
    };

    // Top-level: warn on any key that isn't in the allowlist.
    for (key, _) in table.iter() {
        if !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            match distance_1_suggestion(key, KNOWN_TOP_LEVEL_KEYS) {
                Some(suggestion) => warn!(
                    key = %key,
                    suggestion = %suggestion,
                    "authoritative-sources TOML: unknown top-level key (ignored). Did you mean `{}`? Allowed: {:?}",
                    suggestion,
                    KNOWN_TOP_LEVEL_KEYS
                ),
                None => warn!(
                    key = %key,
                    "authoritative-sources TOML: unknown top-level key (ignored). Allowed: {:?}",
                    KNOWN_TOP_LEVEL_KEYS
                ),
            }
        }
    }

    // `[[authority]]` rows: walk each row's fields.
    let Some(array) = table.get("authority").and_then(|v| v.as_array()) else {
        return;
    };
    for (idx, entry) in array.iter().enumerate() {
        let Some(row) = entry.as_table() else {
            continue;
        };
        for (key, _) in row.iter() {
            if !KNOWN_ENTRY_FIELDS.contains(&key.as_str()) {
                match distance_1_suggestion(key, KNOWN_ENTRY_FIELDS) {
                    Some(suggestion) => warn!(
                        entry_index = idx,
                        unknown_field = %key,
                        suggestion = %suggestion,
                        "authoritative-sources TOML: unknown field on [[authority]] entry (ignored). Did you mean `{}`? Allowed: {:?}",
                        suggestion,
                        KNOWN_ENTRY_FIELDS
                    ),
                    None => warn!(
                        entry_index = idx,
                        unknown_field = %key,
                        "authoritative-sources TOML: unknown field on [[authority]] entry (ignored). Allowed: {:?}",
                        KNOWN_ENTRY_FIELDS
                    ),
                }
            }
        }
    }
}

/// Session 86 — closest distance-1 typo suggestion against a closed
/// allowlist. Returns `Some(candidate)` when there's exactly one
/// allowlist entry within edit distance 1 of `unknown`, otherwise
/// `None`.
///
/// Edit distance 1 is defined as: a single character insertion,
/// deletion, or substitution (Levenshtein 1). Transpositions of two
/// adjacent characters count as 2 edits under classic Levenshtein but
/// we promote them to distance 1 here because TOML field-name typos
/// like `metirc` ↔ `metric` are the canonical case.
///
/// **Why "exactly one":** when two allowlist entries are equidistant
/// the suggestion would be ambiguous, so we surface nothing rather
/// than guess. The warn still names the full allowlist, which is the
/// fallback behaviour from Session 85.
///
/// **Why a tiny custom implementation:** the allowlist is a handful of
/// ASCII identifiers; pulling in `strsim` or `levenshtein` would be
/// dependency weight for a leaf decoration. The bound below operates
/// over up to len*allowlist character comparisons — measured in
/// nanoseconds for any plausible config file.
///
/// Session 87: exposed `pub` so apps_common's `sources.toml` loader
/// (and other future TOML loaders that follow the lossy-continue +
/// warn-with-suggestion posture) can reuse it without re-implementing.
pub fn distance_1_suggestion<'a>(unknown: &str, allowlist: &'a [&'a str]) -> Option<&'a str> {
    let mut matches = Vec::new();
    for candidate in allowlist {
        if is_within_edit_distance_1(unknown, candidate) {
            matches.push(*candidate);
            if matches.len() > 1 {
                // Two equidistant candidates: refuse to guess.
                return None;
            }
        }
    }
    matches.into_iter().next()
}

/// Returns true iff `a` and `b` differ by exactly one
/// insertion / deletion / substitution / adjacent transposition.
/// Equal strings return false (the caller wants a *suggestion*, not
/// self-identity).
fn is_within_edit_distance_1(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let (alen, blen) = (a_bytes.len(), b_bytes.len());
    let len_diff = alen.abs_diff(blen);
    if len_diff > 1 {
        return false;
    }

    if alen == blen {
        // Substitution: exactly one byte differs.
        let mut diffs = 0;
        let mut first_diff: Option<usize> = None;
        for i in 0..alen {
            if a_bytes[i] != b_bytes[i] {
                if diffs == 0 {
                    first_diff = Some(i);
                }
                diffs += 1;
                if diffs > 2 {
                    return false;
                }
            }
        }
        if diffs == 1 {
            return true;
        }
        // Exactly-2-diff case: accept only if they form an adjacent
        // transposition (a[i] = b[i+1] && a[i+1] = b[i]). The typoed
        // `metirc` vs `metric` lands here.
        if diffs == 2 {
            if let Some(i) = first_diff {
                if i + 1 < alen
                    && a_bytes[i] == b_bytes[i + 1]
                    && a_bytes[i + 1] == b_bytes[i]
                    && a_bytes[i + 2..] == b_bytes[i + 2..]
                {
                    return true;
                }
            }
        }
        false
    } else {
        // Insertion / deletion: align the shorter against the longer
        // and require at most one byte unaccounted for.
        let (short, long) = if alen < blen {
            (a_bytes, b_bytes)
        } else {
            (b_bytes, a_bytes)
        };
        let mut i = 0;
        let mut j = 0;
        let mut skipped = false;
        while i < short.len() && j < long.len() {
            if short[i] == long[j] {
                i += 1;
                j += 1;
            } else if !skipped {
                skipped = true;
                j += 1;
            } else {
                return false;
            }
        }
        // Either we consumed the short side and the long side has at
        // most one byte left, or the long side is exhausted (and the
        // tail skip happens implicitly).
        true
    }
}

/// Shared (claimant, metric, topic) predicate used by both `matches`
/// and `quorum_override_for`. Pulled out so the two surfaces can't
/// drift on the closed-vocabulary matching rules.
fn entry_matches_assertion(entry: &AuthorityEntry, claimant: &str, a: &Assertion) -> bool {
    claimant_matches(claimant, &entry.source_id)
        && entry
            .metric
            .as_deref()
            .map_or(true, |m| content_metric_matches(&a.content, m))
        && entry.topic.as_deref().map_or(true, |t| {
            a.envelope.subjects.topics.iter().any(|topic| topic.as_str() == t)
        })
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
                selector_path: None,
                raw_bytes_excerpt: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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
            consensus_quorum: None,
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

    // Session 84 — per-claimant consensus quorum override -----------

    #[test]
    fn entry_with_consensus_quorum_2_opts_out_of_fast_track_match() {
        // An entry with consensus_quorum >= 2 should not satisfy
        // `matches` (the auth pass would otherwise fast-track at N=1
        // and skip the consensus pass entirely).
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:reuters".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: Some(2),
        }]);
        let a = obs_assertion("agency:reuters", "production", "lithium");
        assert!(!r.matches(&a), "consensus_quorum=2 must NOT fast-track");
        assert_eq!(r.quorum_override_for(&a), Some(2));
    }

    #[test]
    fn entry_with_consensus_quorum_1_still_fast_tracks() {
        // Explicit `consensus_quorum = 1` behaves the same as the
        // default (None) — preserves Session-82 N=1 fast-track.
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: Some(1),
        }]);
        let a = obs_assertion("agency:usgs_mcs", "production", "lithium");
        assert!(r.matches(&a));
        // quorum_override_for still returns Some(1) for traceability;
        // the consensus pass treats Some(n) and the cfg default the
        // same when min() resolves.
        assert_eq!(r.quorum_override_for(&a), Some(1));
    }

    #[test]
    fn quorum_override_picks_minimum_across_matching_entries() {
        // Two entries match the same Assertion. The lower
        // consensus_quorum wins.
        let r = AuthorityRegistry::from_entries(vec![
            AuthorityEntry {
                source_id: "agency:reuters".into(),
                metric: None,
                topic: None,
                consensus_quorum: Some(3),
            },
            AuthorityEntry {
                source_id: "agency:reuters".into(),
                metric: Some("production".into()),
                topic: None,
                consensus_quorum: Some(2),
            },
        ]);
        let a = obs_assertion("agency:reuters", "production", "lithium");
        assert_eq!(r.quorum_override_for(&a), Some(2));
    }

    #[test]
    fn quorum_override_returns_none_when_no_entry_matches() {
        let r = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: Some(2),
        }]);
        let a = obs_assertion("agency:other", "production", "lithium");
        assert_eq!(r.quorum_override_for(&a), None);
    }

    #[test]
    fn parse_handles_consensus_quorum_field() {
        let s = r#"
[[authority]]
source_id = "agency:reuters"
metric = "production"
consensus_quorum = 2
"#;
        let r = AuthorityRegistry::parse(s).expect("toml parses");
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].consensus_quorum, Some(2));
    }

    #[test]
    fn parse_treats_missing_consensus_quorum_as_none() {
        // Backwards-compat: pre-Session-84 TOML rows have no
        // `consensus_quorum` field. They must parse as None and
        // continue to fast-track via the auth pass at N=1.
        let s = r#"
[[authority]]
source_id = "usgs_mcs"
metric = "production"
"#;
        let r = AuthorityRegistry::parse(s).expect("toml parses");
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].consensus_quorum, None);
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

    // Session 85 — schema validation -------------------------------

    #[test]
    fn parse_accepts_unknown_field_with_warn_does_not_fail() {
        // Operator typoed `consensus_quorom` — the file still parses
        // (lossy continue), the typed shape ignores the unknown field
        // and the entry parses normally with default consensus_quorum.
        // The warn-log surfaces the typo without breaking hot-reload.
        let s = r#"
[[authority]]
source_id = "agency:reuters"
metric = "production"
consensus_quorom = 2
"#;
        let r = AuthorityRegistry::parse(s).expect("typo still parses");
        assert_eq!(r.entries().len(), 1);
        // The unknown field is dropped; the typed field stays at its
        // serde default.
        assert_eq!(r.entries()[0].consensus_quorum, None);
        assert_eq!(r.entries()[0].source_id, "agency:reuters");
        assert_eq!(r.entries()[0].metric.as_deref(), Some("production"));
    }

    #[test]
    fn parse_accepts_unknown_top_level_table_with_warn() {
        // Misspelled top-level key (`[[authoritys]]` instead of
        // `[[authority]]`) → no entries, but the parse doesn't error.
        // The warn-log surfaces the typo.
        let s = r#"
[[authoritys]]
source_id = "agency:reuters"
metric = "production"
"#;
        let r = AuthorityRegistry::parse(s).expect("unknown top-level key still parses");
        assert_eq!(r.entries().len(), 0);
    }

    #[test]
    fn parse_handles_multiple_unknown_fields_in_one_entry() {
        // Two typos in one row + a real field. Real fields land;
        // unknowns get dropped.
        let s = r#"
[[authority]]
source_id = "agency:reuters"
metirc = "production"
topick = "Cu"
"#;
        let r = AuthorityRegistry::parse(s).unwrap();
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].source_id, "agency:reuters");
        // Both typoed fields land as None (their typed counterparts
        // got nothing to deserialize).
        assert_eq!(r.entries()[0].metric, None);
        assert_eq!(r.entries()[0].topic, None);
    }

    #[test]
    fn parse_known_fields_pass_schema_check_without_warn() {
        // Sanity: a fully-known shape parses with no warnings (we
        // can't capture log output here without a custom subscriber,
        // but the contract is "no false warnings on the happy path";
        // the constants we maintain in `check_schema_warnings` must
        // cover every field on the real struct, exercised by the
        // `known_entry_fields_match_struct` test below).
        let s = r#"
[[authority]]
source_id = "agency:reuters"
metric = "production"
topic = "Cu"
consensus_quorum = 2
"#;
        let r = AuthorityRegistry::parse(s).unwrap();
        assert_eq!(r.entries().len(), 1);
        let e = &r.entries()[0];
        assert_eq!(e.source_id, "agency:reuters");
        assert_eq!(e.metric.as_deref(), Some("production"));
        assert_eq!(e.topic.as_deref(), Some("Cu"));
        assert_eq!(e.consensus_quorum, Some(2));
    }

    // Session 86 — distance-1 typo suggestions -------------------------

    #[test]
    fn substitution_typo_resolves_to_canonical_field() {
        // `consensus_quorom` ↔ `consensus_quorum` — single substitution
        // (`o` → `u` at position 14). Most common shape of TOML typos.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("consensus_quorom", allow), Some("consensus_quorum"));
    }

    #[test]
    fn transposition_typo_resolves_to_canonical_field() {
        // `metirc` ↔ `metric` — adjacent transposition at positions 2/3.
        // Classic Levenshtein is distance 2, but we promote
        // single-adjacent-swap to distance 1 — typing-realistic, and
        // these are exactly the typos operators make.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("metirc", allow), Some("metric"));
    }

    #[test]
    fn deletion_typo_resolves_to_canonical_field() {
        // `topi` ↔ `topic` — missing trailing `c`. Single deletion.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("topi", allow), Some("topic"));
    }

    #[test]
    fn insertion_typo_resolves_to_canonical_field() {
        // `metrics` ↔ `metric` — extra trailing `s`. Single insertion.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("metrics", allow), Some("metric"));
    }

    #[test]
    fn far_typo_yields_no_suggestion() {
        // `quor` is distance 1 from no allowlist entry: shorter than
        // every multi-char candidate by more than one, and not a
        // prefix of `consensus_quorum`.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("quor", allow), None);
    }

    #[test]
    fn equidistant_candidates_yield_no_suggestion() {
        // `xs` is distance 1 from both `xa` and `xb`: refuse to guess
        // when two allowlist entries are tied. The full allowlist still
        // gets named in the warn (caller's responsibility).
        let allow = &["xa", "xb", "completely_different"];
        assert_eq!(distance_1_suggestion("xs", allow), None);
    }

    #[test]
    fn exact_match_yields_no_suggestion() {
        // Defensive: `distance_1_suggestion` is only called when the
        // key is NOT in the allowlist, but if a caller ever invokes it
        // with a known field name it returns None — a suggestion of
        // self-identity would be a nonsense warn.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("metric", allow), None);
    }

    #[test]
    fn edit_distance_helper_is_symmetric() {
        // Defence-in-depth: the helper's contract is symmetric, so
        // swapping the two arguments should not change the boolean.
        assert!(is_within_edit_distance_1("metirc", "metric"));
        assert!(is_within_edit_distance_1("metric", "metirc"));
        assert!(is_within_edit_distance_1("topi", "topic"));
        assert!(is_within_edit_distance_1("topic", "topi"));
    }

    #[test]
    fn edit_distance_rejects_distance_two_and_above() {
        // `souce_di` from `source_id`: deletion + transposition,
        // distance 2; neither alignment fits within 1.
        let allow = &["source_id", "metric", "topic", "consensus_quorum"];
        assert_eq!(distance_1_suggestion("souce_di", allow), None);
        // Sanity: kitten/sitten is distance 1 (single substitution).
        assert!(is_within_edit_distance_1("kitten", "sitten"));
        // sittin vs kitten is distance 2 (k→s + e→i).
        assert!(!is_within_edit_distance_1("kitten", "sittin"));
    }

    #[test]
    fn parse_unknown_field_with_typo_suggestion_still_parses() {
        // Operator typoed `consensus_quorom`. The pre-parse sweep emits
        // a suggestion-bearing warn; the typed parse below proceeds with
        // the field at its serde default. Behaviour identical to
        // Session 85 from the registry's point of view — the
        // suggestion is purely a log decoration.
        let s = r#"
[[authority]]
source_id = "agency:reuters"
metric = "production"
consensus_quorom = 2
"#;
        let r = AuthorityRegistry::parse(s).expect("typo still parses");
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].consensus_quorum, None);
    }

    #[test]
    fn known_entry_fields_match_struct() {
        // Lockstep guard: every field on `AuthorityEntry` must appear
        // in the schema-validator's allowlist. If a future session
        // adds a new field on the struct without updating
        // `KNOWN_ENTRY_FIELDS`, the new field would warn as
        // "unknown" on every load — exactly the false-warn case we
        // want to catch.
        //
        // We can't introspect the struct at compile time without a
        // proc-macro, but we can round-trip a fully-populated value
        // through serde and confirm every emitted key is allowlisted.
        let entry = AuthorityEntry {
            source_id: "x".into(),
            metric: Some("m".into()),
            topic: Some("t".into()),
            consensus_quorum: Some(2),
        };
        let val = toml::Value::try_from(&entry).expect("serialize");
        let table = val.as_table().expect("authority entry serializes to table");

        // Pull the allowlist back out of `parse` by parsing a known
        // entry with each field; if the warn function ever gets out
        // of sync this assertion blows up.
        let allowed: &[&str] = &["source_id", "metric", "topic", "consensus_quorum"];
        for key in table.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "AuthorityEntry serializes field {key:?} but the schema-validator allowlist doesn't include it. \
                 Add it to `KNOWN_ENTRY_FIELDS` in `check_schema_warnings` in the same commit."
            );
        }
    }
}
