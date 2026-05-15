//! Fetch-outcome classification — Piece A of ADR 0017.
//!
//! ## What this module does
//!
//! Maps a (host, [`FetchError`]) pair to a closed
//! [`FetchOutcomeClass`] — a class-level vocabulary the URL proposer
//! and the fetch executor can reason about. The classification
//! distinguishes the four causes of fetch failure that ADR 0017
//! identifies as needing different responses:
//!
//!   - `HostUnreachable` — DNS, TLS, connect-refuse, timeout, 5xx.
//!     The host itself is not answering. Retrying the same host is
//!     unlikely to help inside one fetch run; the proposer should
//!     pivot to a different host.
//!   - `HostBlockedByWaf` — 403 from a host known (per the host map
//!     below) to fingerprint requests beyond UA. UA tweaks will not
//!     fix this; the proposer should pivot off the host class.
//!   - `HostRequiresAuth` — 401, full stop. Anonymous fetcher will
//!     not get past it; the proposer should pivot off the host (no
//!     credentials path is wired in this layer; that is an adapter
//!     question per ADR 0017 §"Alt 3").
//!   - `HostRequiresUaPolicy` — 403 from a host that is known to
//!     enforce a UA shape (e.g. SEC EDGAR's email-bearing UA). The
//!     production fetcher's UA does not satisfy the policy; either
//!     the UA needs the host's prescribed shape, or the proposer
//!     should pivot to the host's machine-API endpoint.
//!   - `UrlShapeMismatch` — 404 / 400. The endpoint at this URL
//!     does not exist; the proposer's path was wrong (often a
//!     hallucinated subpath). Retrying the same host with a
//!     different shape is reasonable; pivoting off the host is
//!     not.
//!   - `RateLimited` — 429 with or without `Retry-After`. The
//!     existing host-backoff layer handles this; the class is here
//!     for completeness so the proposer can reason about it
//!     uniformly.
//!   - `Ok` — the fetch succeeded. Threaded through so call sites
//!     that record outcomes do not need a separate "success has
//!     no class" branch.
//!
//! ## What this module does NOT do
//!
//! - **It does not bake host strings into the proposer prompt.**
//!   ADR 0007's closed-vocabulary discipline forbids that. Hosts
//!   appear in [`HOST_CLASS_OVERRIDES`] (this file only) and
//!   nowhere else; the proposer sees [`FetchOutcomeClass`] values,
//!   not host names.
//! - **It does not retry, back off, or change request shape.**
//!   Classification is read-only. The host-backoff layer
//!   ([`crate::fetch_backoff`]) and the propose-URL retry loop
//!   ([`crate::fetch_executor`]) react to the class; this module
//!   only emits the class.
//! - **It does not duplicate the wire-level error vocabulary** of
//!   [`FetchError`]. That vocabulary describes what the network
//!   layer saw; this vocabulary describes what the proposer
//!   should *do about it*. Different layers, different shapes.
//!
//! ## Coverage of the host map
//!
//! Today [`HOST_CLASS_OVERRIDES`] is empty. The intended
//! population mechanism is the diagnostic probe shipped alongside
//! this ADR (`apps/eval_harness/src/bin/host_probe.rs`): run it
//! against the suspect URL list, observe which UA strings change
//! which status codes, and add an override per host whose default
//! 403 reading is wrong.
//!
//! In the absence of an override, the default policy is conservative:
//!
//!   - 401 → `HostRequiresAuth` (always — 401 is unambiguous)
//!   - 403 → `HostBlockedByWaf` (assume the worst; the override
//!     map is the only way to upgrade this to `HostRequiresUaPolicy`)
//!   - 404 / 400 → `UrlShapeMismatch`
//!   - 5xx, timeout, TLS, DNS, generic Http(_) → `HostUnreachable`
//!   - 429 → `RateLimited`
//!
//! The "assume WAF on 403" default means we will sometimes
//! mis-classify a UA-policy host as a WAF host and pivot off it
//! prematurely; the cost of that mistake is one extra propose-URL
//! attempt against a different host class. The opposite mistake
//! ("assume UA-policy and burn attempts retrying with different
//! UAs") would cost more attempts and would not learn from the
//! evidence — every retry against a true WAF host returns 403
//! again, so the proposer would loop. We pick the cheaper failure
//! mode.

use serde::{Deserialize, Serialize};

use crate::http_fetcher::FetchError;

// ---------------------------------------------------------------------------
// The class vocabulary
// ---------------------------------------------------------------------------

/// Closed enum of fetch-outcome classes. Read by the propose-URL
/// retry loop to decide whether the next attempt should retry the
/// same host with a different URL shape, pivot to a different host
/// class, or stop early.
///
/// `serde_json` representation is `snake_case` so the class names
/// can appear in `prior_attempts` history surfaced to the LLM
/// without further transformation. ADR 0007's closed-vocabulary
/// discipline applies: this enum is the *whole* vocabulary the
/// proposer sees for fetch outcomes; no host strings, no domain
/// strings, no error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchOutcomeClass {
    Ok,
    HostUnreachable,
    HostBlockedByWaf,
    HostRequiresAuth,
    HostRequiresUaPolicy,
    UrlShapeMismatch,
    RateLimited,
}

impl FetchOutcomeClass {
    /// Short snake_case label suitable for log fields and TSV
    /// columns. Equivalent to the serde representation; provided as
    /// a const-friendly accessor so call sites that don't want to
    /// pull serde into their dependency graph can still spell the
    /// class.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::HostUnreachable => "host_unreachable",
            Self::HostBlockedByWaf => "host_blocked_by_waf",
            Self::HostRequiresAuth => "host_requires_auth",
            Self::HostRequiresUaPolicy => "host_requires_ua_policy",
            Self::UrlShapeMismatch => "url_shape_mismatch",
            Self::RateLimited => "rate_limited",
        }
    }
}

// ---------------------------------------------------------------------------
// Host-class override map
// ---------------------------------------------------------------------------

/// Hosts whose default 403 reading is wrong. Each entry is a
/// `(host_suffix, FetchOutcomeClass)` pair. The host match is
/// suffix-based (case-insensitive) so `www.sec.gov` and `sec.gov`
/// resolve to the same entry.
///
/// **Empty until probe data lands.** Populating this list is the
/// post-probe step of ADR 0017's implementation order; entries
/// added here must be justified by a `host-probe` row showing the
/// status code under each UA. Adding an entry without that
/// evidence is the very "easy win" ADR 0017 rejects — the
/// override is effectively a private prompt-style nudge, and
/// without measurement we cannot know it is correct.
///
/// The closed-vocabulary discipline (ADR 0007) is preserved
/// because hosts appear *only here*, never in the propose-URL
/// prompt or the recipe-author prompt. The proposer reasons about
/// classes; the classifier reasons about hosts; the boundary is
/// this constant.
const HOST_CLASS_OVERRIDES: &[(&str, FetchOutcomeClass)] = &[
    // Examples (commented out, for shape only):
    //
    // ("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy),
    // ("efts.sec.gov", FetchOutcomeClass::Ok),  // would never hit
    //                                              // because the
    //                                              // override only
    //                                              // fires on 403
    // ("bloomberg.com", FetchOutcomeClass::HostBlockedByWaf),
    //
    // Activate these (or others) only after host-probe shows the
    // status-vs-UA cross-tabulation that supports the class.
];

/// Look up the override class for `host`. Match is case-
/// insensitive and suffix-anchored: `www.sec.gov` matches an entry
/// for `sec.gov`. Returns `None` when no override is set; callers
/// fall back to the default policy in [`classify_error`].
///
/// **Visibility:** `pub(crate)` since Session 74 / ADR 0009
/// amendment 2 wire-up. `ua_policies::ua_policy_for_host` reads
/// the same override table so the host → UA-policy decision sits
/// behind the same single source of host-class knowledge as the
/// host → classifier-class decision. The override map itself
/// (`HOST_CLASS_OVERRIDES`) remains private and empty until probe
/// evidence justifies entries.
pub(crate) fn host_class_override(host: &str) -> Option<FetchOutcomeClass> {
    let host_lc = host.to_ascii_lowercase();
    for (suffix, class) in HOST_CLASS_OVERRIDES {
        let suffix_lc = suffix.to_ascii_lowercase();
        if host_lc == suffix_lc || host_lc.ends_with(&format!(".{suffix_lc}")) {
            return Some(*class);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify a [`FetchError`] for the host that produced it.
///
/// `host` is the URL's host component (`www.example.com`), as
/// extracted by the caller. An empty `host` is permitted and
/// disables the override path; the default policy applies.
///
/// The 403 default is `HostBlockedByWaf` (the conservative reading
/// — see the module docs for why). To upgrade a 403 to
/// `HostRequiresUaPolicy`, add the host to
/// [`HOST_CLASS_OVERRIDES`] with that class. The override is
/// consulted only on 403; a 404 from `sec.gov` is still
/// `UrlShapeMismatch`, not `HostRequiresUaPolicy`, because the
/// path is the problem regardless of UA.
pub fn classify_error(host: &str, err: &FetchError) -> FetchOutcomeClass {
    match err {
        FetchError::RateLimited { .. } => FetchOutcomeClass::RateLimited,
        FetchError::Timeout(_) => FetchOutcomeClass::HostUnreachable,
        FetchError::TooLarge { .. } => FetchOutcomeClass::UrlShapeMismatch,
        FetchError::NoFixture(_) => FetchOutcomeClass::HostUnreachable,
        FetchError::Http(_) => FetchOutcomeClass::HostUnreachable,
        FetchError::Status(code) => classify_status(host, *code),
    }
}

/// Classify a non-429 HTTP status. 429 is handled by the
/// `RateLimited` arm of [`classify_error`]; this function should
/// not be called with `code == 429`.
fn classify_status(host: &str, code: u16) -> FetchOutcomeClass {
    match code {
        401 => FetchOutcomeClass::HostRequiresAuth,
        403 => host_class_override(host).unwrap_or(FetchOutcomeClass::HostBlockedByWaf),
        // 400 is included with 404 because in practice the
        // proposer triggers it the same way: a URL whose shape
        // (path, query) the host's routing rejects. Yonhap
        // `/politics` returning 400 in Session 57 is the
        // canonical example.
        400 | 404 | 410 => FetchOutcomeClass::UrlShapeMismatch,
        // Other 4xx (405 Method Not Allowed, 415 Unsupported
        // Media Type, 451 Unavailable For Legal Reasons, …) are
        // rare and host-specific; treat as URL-shape so the
        // proposer pivots URL rather than host.
        c if (400..500).contains(&c) => FetchOutcomeClass::UrlShapeMismatch,
        // 5xx is the host's problem, not ours; treat as
        // unreachable so the proposer pivots host.
        c if (500..600).contains(&c) => FetchOutcomeClass::HostUnreachable,
        // Defensive default: anything else (1xx, 3xx that escaped
        // the redirect policy, anomalous codes) is opaque.
        // Unreachable is the conservative class that pivots host.
        _ => FetchOutcomeClass::HostUnreachable,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // -- classify_error: each FetchError variant routes to the
    //    expected class with no host override.

    #[test]
    fn rate_limited_routes_to_rate_limited() {
        let err = FetchError::RateLimited {
            retry_after_seconds: Some(30),
        };
        assert_eq!(
            classify_error("any.example", &err),
            FetchOutcomeClass::RateLimited
        );
    }

    #[test]
    fn timeout_routes_to_host_unreachable() {
        let err = FetchError::Timeout(Duration::from_secs(60));
        assert_eq!(
            classify_error("slow.example", &err),
            FetchOutcomeClass::HostUnreachable
        );
    }

    #[test]
    fn too_large_routes_to_url_shape_mismatch() {
        // The path returned a body that exceeded the cap; this
        // means the URL pointed at the wrong shape (a full archive
        // when we wanted a daily slice, e.g.) and the proposer
        // should propose a different URL on the same host —
        // exactly what UrlShapeMismatch communicates.
        let err = FetchError::TooLarge { max: 1000, got: 99999 };
        assert_eq!(
            classify_error("data.example", &err),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn opaque_http_error_routes_to_host_unreachable() {
        // DNS / TLS / generic reqwest failures all collapse to
        // FetchError::Http(String) at the pipeline boundary and
        // get classified as host-unreachable for proposer
        // routing purposes.
        let err = FetchError::Http("dns lookup failed".to_string());
        assert_eq!(
            classify_error("nx.example", &err),
            FetchOutcomeClass::HostUnreachable
        );
    }

    // -- classify_status: each status code routes to the expected
    //    class with no host override.

    #[test]
    fn status_401_is_host_requires_auth() {
        assert_eq!(
            classify_error("paywall.example", &FetchError::Status(401)),
            FetchOutcomeClass::HostRequiresAuth
        );
    }

    #[test]
    fn status_403_default_is_host_blocked_by_waf() {
        // Default policy: 403 → WAF until the host appears in the
        // override map with a different class. See module docs
        // for why the conservative default is WAF, not UA-policy.
        assert_eq!(
            classify_error("waf.example", &FetchError::Status(403)),
            FetchOutcomeClass::HostBlockedByWaf
        );
    }

    #[test]
    fn status_404_is_url_shape_mismatch() {
        assert_eq!(
            classify_error("data.example", &FetchError::Status(404)),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn status_400_is_url_shape_mismatch() {
        // Yonhap `/politics` 400 in Session 57: same proposer
        // remediation as 404, so same class.
        assert_eq!(
            classify_error("news.example", &FetchError::Status(400)),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn status_410_is_url_shape_mismatch() {
        assert_eq!(
            classify_error("news.example", &FetchError::Status(410)),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn status_405_routes_to_url_shape_mismatch() {
        // Other 4xx (Method Not Allowed, etc.) collapse to
        // url-shape so the proposer pivots URL rather than host.
        assert_eq!(
            classify_error("api.example", &FetchError::Status(405)),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn status_500_is_host_unreachable() {
        assert_eq!(
            classify_error("broken.example", &FetchError::Status(500)),
            FetchOutcomeClass::HostUnreachable
        );
    }

    #[test]
    fn status_503_is_host_unreachable() {
        assert_eq!(
            classify_error("broken.example", &FetchError::Status(503)),
            FetchOutcomeClass::HostUnreachable
        );
    }

    // -- host override path: with a synthetic override applied,
    //    confirm 403 is upgraded to HostRequiresUaPolicy and
    //    nothing else is affected.

    /// Replicates `classify_status` with a one-off override for
    /// the test; the production `HOST_CLASS_OVERRIDES` constant
    /// stays empty by design (entries land only with probe
    /// evidence), so we cannot cover the override path with
    /// production state alone.
    fn classify_status_with_override(
        host: &str,
        code: u16,
        overrides: &[(&str, FetchOutcomeClass)],
    ) -> FetchOutcomeClass {
        let host_lc = host.to_ascii_lowercase();
        let lookup = |target: u16| -> Option<FetchOutcomeClass> {
            for (suffix, class) in overrides {
                let suffix_lc = suffix.to_ascii_lowercase();
                if host_lc == suffix_lc || host_lc.ends_with(&format!(".{suffix_lc}")) {
                    if target == 403 {
                        return Some(*class);
                    }
                }
            }
            None
        };
        match code {
            401 => FetchOutcomeClass::HostRequiresAuth,
            403 => lookup(403).unwrap_or(FetchOutcomeClass::HostBlockedByWaf),
            400 | 404 | 410 => FetchOutcomeClass::UrlShapeMismatch,
            c if (400..500).contains(&c) => FetchOutcomeClass::UrlShapeMismatch,
            c if (500..600).contains(&c) => FetchOutcomeClass::HostUnreachable,
            _ => FetchOutcomeClass::HostUnreachable,
        }
    }

    #[test]
    fn override_upgrades_403_to_ua_policy_for_matching_host() {
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        assert_eq!(
            classify_status_with_override("www.sec.gov", 403, overrides),
            FetchOutcomeClass::HostRequiresUaPolicy
        );
    }

    #[test]
    fn override_does_not_affect_404_on_matching_host() {
        // A 404 from a UA-policy host is still a path problem,
        // not a UA problem. The override should fire only on 403.
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        assert_eq!(
            classify_status_with_override("www.sec.gov", 404, overrides),
            FetchOutcomeClass::UrlShapeMismatch
        );
    }

    #[test]
    fn override_does_not_affect_non_matching_host() {
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        assert_eq!(
            classify_status_with_override("www.example.com", 403, overrides),
            FetchOutcomeClass::HostBlockedByWaf
        );
    }

    #[test]
    fn override_suffix_match_handles_subdomain() {
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        // `data.sec.gov` ends with `.sec.gov` so the override fires.
        assert_eq!(
            classify_status_with_override("data.sec.gov", 403, overrides),
            FetchOutcomeClass::HostRequiresUaPolicy
        );
    }

    #[test]
    fn override_suffix_match_is_case_insensitive() {
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("SEC.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        assert_eq!(
            classify_status_with_override("WWW.SEC.GOV", 403, overrides),
            FetchOutcomeClass::HostRequiresUaPolicy
        );
    }

    #[test]
    fn override_suffix_match_does_not_partial_match() {
        // "fakesec.gov" is NOT under the "sec.gov" suffix —
        // suffix-anchoring on a dot boundary prevents
        // off-target matches.
        let overrides: &[(&str, FetchOutcomeClass)] =
            &[("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy)];
        assert_eq!(
            classify_status_with_override("fakesec.gov", 403, overrides),
            FetchOutcomeClass::HostBlockedByWaf
        );
    }

    // -- label: every variant has a stable snake_case label that
    //    matches the serde representation. Locked in by test so
    //    accidental renames break compilation, not silent
    //    behaviour.

    #[test]
    fn label_strings_are_stable_snake_case() {
        assert_eq!(FetchOutcomeClass::Ok.label(), "ok");
        assert_eq!(FetchOutcomeClass::HostUnreachable.label(), "host_unreachable");
        assert_eq!(
            FetchOutcomeClass::HostBlockedByWaf.label(),
            "host_blocked_by_waf"
        );
        assert_eq!(
            FetchOutcomeClass::HostRequiresAuth.label(),
            "host_requires_auth"
        );
        assert_eq!(
            FetchOutcomeClass::HostRequiresUaPolicy.label(),
            "host_requires_ua_policy"
        );
        assert_eq!(
            FetchOutcomeClass::UrlShapeMismatch.label(),
            "url_shape_mismatch"
        );
        assert_eq!(FetchOutcomeClass::RateLimited.label(), "rate_limited");
    }

    #[test]
    fn label_matches_serde_representation() {
        // serde and label() are independent paths to the same
        // string; this guards against the two drifting.
        let cases = [
            (FetchOutcomeClass::Ok, "\"ok\""),
            (
                FetchOutcomeClass::HostUnreachable,
                "\"host_unreachable\"",
            ),
            (
                FetchOutcomeClass::HostBlockedByWaf,
                "\"host_blocked_by_waf\"",
            ),
            (
                FetchOutcomeClass::HostRequiresAuth,
                "\"host_requires_auth\"",
            ),
            (
                FetchOutcomeClass::HostRequiresUaPolicy,
                "\"host_requires_ua_policy\"",
            ),
            (
                FetchOutcomeClass::UrlShapeMismatch,
                "\"url_shape_mismatch\"",
            ),
            (FetchOutcomeClass::RateLimited, "\"rate_limited\""),
        ];
        for (class, expected_json) in cases {
            let json = serde_json::to_string(&class).unwrap();
            assert_eq!(json, expected_json);
            // And the embedded string between the quotes equals
            // label().
            let unquoted = &json[1..json.len() - 1];
            assert_eq!(unquoted, class.label());
        }
    }

    // -- production HOST_CLASS_OVERRIDES is empty: locked in so
    //    that adding entries is a deliberate code change reviewed
    //    against probe evidence, never a drive-by tweak.

    #[test]
    fn host_class_overrides_is_empty_until_probe_data_lands() {
        assert!(
            HOST_CLASS_OVERRIDES.is_empty(),
            "HOST_CLASS_OVERRIDES must stay empty until a host-probe TSV \
             justifies each entry; see the module docs and ADR 0017."
        );
    }
}
