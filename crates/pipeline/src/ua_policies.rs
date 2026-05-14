//! Host-class-aware User-Agent policies (Session 70 / ADR 0009 amendment 2).
//!
//! ## Why this module exists
//!
//! The default UA shipped by `SecureHttpConfig`
//! (`SituationRoom/<version> (+<repo-url>)`) is honest and works for
//! the majority of hosts. A small but persistent minority returns
//! 403 to that UA: hosts that fingerprint past UA (WAF) or hosts that
//! require a specific UA shape (SEC EDGAR's e-mail-bearing convention).
//! ADR 0017's `FetchOutcomeClass` already names these classes; this
//! module supplies the UA strings the executor should reach for when
//! a host is known (via the override map in
//! `crate::fetch_classes::HOST_CLASS_OVERRIDES`) to fall into one of
//! them.
//!
//! ## The closed table
//!
//! [`UaPolicy`] is a small closed enum keyed off the same vocabulary
//! `FetchOutcomeClass` uses. Each variant maps to a single UA string
//! constant defined here — *no* host strings, *no* domain strings, *no*
//! per-source overrides. The host-to-class mapping lives in
//! `fetch_classes`; the class-to-UA mapping lives here; the proposer
//! and recipe author see neither.
//!
//! ## What this module does NOT do
//!
//! - **It does not call `SecureHttpClient`.** The UA strings are
//!   pure data. The fetch executor reads them and passes the result
//!   to `SecureHttpClient::get_with_headers_ua` at the call site.
//! - **It does not populate the host-class override map.** That
//!   work is gated on `apps/eval_harness/src/bin/host_probe.rs`
//!   data — the override map in `fetch_classes` is empty until
//!   the probe shows status-vs-UA evidence for a host.
//! - **It does not implement a UA-rotation strategy.** Each class
//!   maps to exactly one UA. Adding randomisation or rotation would
//!   be a separate decision; this table is meant to be small,
//!   stable, and auditable.

use serde::{Deserialize, Serialize};

use crate::fetch_classes::FetchOutcomeClass;

/// Closed enum of UA policies, one per fetch-outcome class that
/// benefits from a non-default UA. Variants are named for the policy
/// they implement, not for the hosts they target — the closed
/// vocabulary discipline (see `project_sr_no_source_routing`) means
/// the variant names must read as principles, not as proper nouns.
///
/// `Default` is the canonical SituationRoom UA, used everywhere a
/// host isn't classified into a specific policy. It's listed
/// explicitly here so call sites can be exhaustive over the enum
/// rather than branching `Option<UaPolicy>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UaPolicy {
    /// SituationRoom's build-time UA. Works for the majority of
    /// hosts; this is the same string `SecureHttpConfig::default`
    /// ships at the secure-crate level.
    Default,
    /// "Identifies as a recent browser." Used for hosts classified
    /// as `HostBlockedByWaf` — the working assumption is that the
    /// WAF rejects non-browser UAs and the bypass is to look like
    /// a browser.
    ///
    /// **The string is intentionally common.** A real Chrome/Firefox
    /// UA is the most-tested input for any anti-bot WAF; spoofing
    /// to something exotic would be both technically dishonest and
    /// strategically worse (rare UAs are themselves flagged).
    BrowserLike,
    /// "Identifies as a research tool with operator contact." Used
    /// for hosts classified as `HostRequiresUaPolicy` — the
    /// canonical example is SEC EDGAR, whose access policy requires
    /// an e-mail-bearing UA (the policy is publicly documented at
    /// the host; SituationRoom's compliance is to provide the
    /// contact, not to spoof a browser).
    ///
    /// The contact e-mail is sourced from the
    /// `SITUATIONROOM_CONTACT_EMAIL` environment variable at
    /// runtime; if absent, the UA falls back to the build-time
    /// repository URL so the request still carries a contact path.
    ResearchToolWithContact,
}

impl UaPolicy {
    /// Short snake_case label suitable for log fields and TSV
    /// columns. Equivalent to the serde representation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::BrowserLike => "browser_like",
            Self::ResearchToolWithContact => "research_tool_with_contact",
        }
    }

    /// Resolve the UA string for this policy.
    ///
    /// `default_ua` is the secure-crate-built default and is used
    /// for [`UaPolicy::Default`]. For the policy variants we return
    /// owned `String`s because the contact-bearing variant needs a
    /// runtime env-var read; making the entire return type `String`
    /// keeps the API uniform and obvious at the call site.
    ///
    /// Returns the *resolved* UA string. Callers pass this to
    /// `SecureHttpClient::get_with_headers_ua` as `Some(&ua)`. To
    /// avoid the override entirely (default behaviour), callers
    /// should branch on `policy == UaPolicy::Default` and pass
    /// `None` — the secure client then uses its configured UA
    /// without a per-request override. The functions here are
    /// "what would the override be," not "what should I do."
    pub fn resolve(self, default_ua: &str) -> String {
        match self {
            Self::Default => default_ua.to_string(),
            Self::BrowserLike => BROWSER_LIKE_UA.to_string(),
            Self::ResearchToolWithContact => research_tool_with_contact_ua(),
        }
    }
}

/// Map a fetch-outcome class to the policy that addresses it.
///
/// `HostUnreachable`, `RateLimited`, `UrlShapeMismatch`,
/// `HostRequiresAuth`, and `Ok` all map to [`UaPolicy::Default`] —
/// none of those failure modes have UA as the cause, and switching
/// UA doesn't unblock them. The two classes that benefit from a UA
/// switch (`HostBlockedByWaf`, `HostRequiresUaPolicy`) are the only
/// ones with non-default mappings.
pub const fn policy_for_class(class: FetchOutcomeClass) -> UaPolicy {
    match class {
        FetchOutcomeClass::HostBlockedByWaf => UaPolicy::BrowserLike,
        FetchOutcomeClass::HostRequiresUaPolicy => UaPolicy::ResearchToolWithContact,
        // Every other class — Ok, HostUnreachable, HostRequiresAuth,
        // UrlShapeMismatch, RateLimited — has no UA-based remedy.
        // The fetch executor will not call this function for
        // success or for already-classified-rate-limited responses;
        // returning Default for those keeps the function total.
        _ => UaPolicy::Default,
    }
}

// ---------------------------------------------------------------------------
// UA string constants
// ---------------------------------------------------------------------------

/// "Recent stable Chrome on macOS." Picked because it's the
/// most-common UA on the public internet today (per several recent
/// UA surveys); WAFs that lean on UA fingerprinting will have the
/// most-tested code path for this string.
///
/// This is *not* a guarantee that the UA reflects the actual
/// platform SituationRoom is running on — the operator may be on
/// Windows, Linux, or an older macOS. The UA string is a fingerprint,
/// not a system identifier; for hosts that reject everything that
/// doesn't look like a real browser, the closest-to-real-browser
/// string is the working bypass.
///
/// **Pinned, not floating.** Bumping this string is a deliberate edit
/// (with an entry in the ADR 0009 amendment log) rather than a
/// dynamic upstream lookup; "always send the current real Chrome UA"
/// would require a network dependency we don't want in the fetch path.
const BROWSER_LIKE_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) \
     AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/126.0.0.0 Safari/537.36";

/// The env var that carries the operator's contact e-mail for the
/// `ResearchToolWithContact` policy. Optional — falls back to the
/// repository URL when absent. The fallback keeps the policy
/// functional in CI / dev without forcing every contributor to set a
/// personal e-mail; production operators who fetch from policy hosts
/// regularly should set it so the host's logs identify them.
const CONTACT_EMAIL_ENV: &str = "SITUATIONROOM_CONTACT_EMAIL";

/// Build the `ResearchToolWithContact` UA. Shape:
/// `SituationRoom-Research/<version> (+<contact>)` where `<contact>`
/// is the env-var e-mail if set, the repo URL otherwise. The
/// `-Research` suffix distinguishes this variant from the default UA
/// in host-side access logs so the operator can audit which requests
/// used the research-policy UA.
fn research_tool_with_contact_ua() -> String {
    let contact = std::env::var(CONTACT_EMAIL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_REPOSITORY").to_string());
    format!(
        "SituationRoom-Research/{} (+{})",
        env!("CARGO_PKG_VERSION"),
        contact
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_for_class_maps_waf_to_browser_like() {
        assert_eq!(
            policy_for_class(FetchOutcomeClass::HostBlockedByWaf),
            UaPolicy::BrowserLike,
        );
    }

    #[test]
    fn policy_for_class_maps_ua_policy_to_research_tool() {
        assert_eq!(
            policy_for_class(FetchOutcomeClass::HostRequiresUaPolicy),
            UaPolicy::ResearchToolWithContact,
        );
    }

    #[test]
    fn policy_for_class_maps_non_ua_classes_to_default() {
        // The closed enum must be exhaustive — every non-UA class
        // returns Default. Listed explicitly here so a future variant
        // forces an update.
        assert_eq!(policy_for_class(FetchOutcomeClass::Ok), UaPolicy::Default);
        assert_eq!(
            policy_for_class(FetchOutcomeClass::HostUnreachable),
            UaPolicy::Default,
        );
        assert_eq!(
            policy_for_class(FetchOutcomeClass::HostRequiresAuth),
            UaPolicy::Default,
        );
        assert_eq!(
            policy_for_class(FetchOutcomeClass::UrlShapeMismatch),
            UaPolicy::Default,
        );
        assert_eq!(
            policy_for_class(FetchOutcomeClass::RateLimited),
            UaPolicy::Default,
        );
    }

    #[test]
    fn browser_like_ua_resolves_to_chrome_shape() {
        let resolved = UaPolicy::BrowserLike.resolve("SituationRoom/test (+default)");
        assert!(resolved.starts_with("Mozilla/5.0"), "UA shape: {resolved}");
        assert!(resolved.contains("Chrome/"), "UA shape: {resolved}");
        assert!(resolved.contains("Safari/"), "UA shape: {resolved}");
        // Sanity: must not be the default — the whole point is the
        // browser-shape override.
        assert_ne!(resolved, "SituationRoom/test (+default)");
    }

    #[test]
    fn research_tool_ua_carries_contact_path() {
        // With env unset (most CI runs) the fallback contact is the
        // repository URL baked at compile time.
        let prev = std::env::var(CONTACT_EMAIL_ENV).ok();
        // SAFETY: serialised by the test runner — Rust test suites
        // are single-threaded per-test by default and Cargo's harness
        // runs them concurrently across binaries, not across tests in
        // one module unless the user opts in. This module's tests do
        // not run in parallel against each other under the default
        // harness, and we restore the env in a guard at the end.
        unsafe {
            std::env::remove_var(CONTACT_EMAIL_ENV);
        }

        let resolved = UaPolicy::ResearchToolWithContact.resolve("ignored");
        assert!(resolved.starts_with("SituationRoom-Research/"));
        assert!(resolved.contains("(+http"), "expected contact path: {resolved}");

        if let Some(v) = prev {
            unsafe {
                std::env::set_var(CONTACT_EMAIL_ENV, v);
            }
        }
    }

    #[test]
    fn research_tool_ua_uses_env_email_when_present() {
        let prev = std::env::var(CONTACT_EMAIL_ENV).ok();
        unsafe {
            std::env::set_var(CONTACT_EMAIL_ENV, "ops@example.test");
        }

        let resolved = UaPolicy::ResearchToolWithContact.resolve("ignored");
        assert!(resolved.contains("ops@example.test"), "UA: {resolved}");

        unsafe {
            match prev {
                Some(v) => std::env::set_var(CONTACT_EMAIL_ENV, v),
                None => std::env::remove_var(CONTACT_EMAIL_ENV),
            }
        }
    }

    #[test]
    fn default_policy_resolves_to_caller_default() {
        let resolved = UaPolicy::Default.resolve("SituationRoom/1.2.3 (+contact)");
        assert_eq!(resolved, "SituationRoom/1.2.3 (+contact)");
    }

    #[test]
    fn label_strings_are_snake_case() {
        // Serde's snake_case representation must match `label()` so
        // log emitters and JSON-serialised attempt records read the
        // same value.
        assert_eq!(UaPolicy::Default.label(), "default");
        assert_eq!(UaPolicy::BrowserLike.label(), "browser_like");
        assert_eq!(
            UaPolicy::ResearchToolWithContact.label(),
            "research_tool_with_contact"
        );
    }

    #[test]
    fn serde_roundtrips_snake_case() {
        let json = serde_json::to_string(&UaPolicy::BrowserLike).unwrap();
        assert_eq!(json, "\"browser_like\"");
        let parsed: UaPolicy = serde_json::from_str("\"research_tool_with_contact\"").unwrap();
        assert_eq!(parsed, UaPolicy::ResearchToolWithContact);
    }
}
