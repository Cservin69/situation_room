//! Host-probe TSV → `HOST_CLASS_OVERRIDES` snippet generator
//! (Session 75 groundwork for ADR 0009 amendment 2 activation).
//!
//! ## What this binary does
//!
//! Reads `host-probe` TSV from stdin (or `--input FILE`), groups
//! rows by host, applies a small closed-vocabulary status × UA →
//! `FetchOutcomeClass` rule, and emits a Rust-syntax snippet on
//! stdout that the operator pastes into
//! `crates/pipeline/src/fetch_classes.rs::HOST_CLASS_OVERRIDES`.
//!
//! ## What this binary does NOT do
//!
//! - **No network calls.** It does not run the probe itself; the
//!   operator runs `host-probe` first and pipes/redirects its TSV
//!   into this binary.
//! - **No file writes.** Output goes to stdout only. The
//!   operator reviews the proposed entries and pastes them by hand
//!   into the override map. This is deliberate — the override
//!   table is closed-vocabulary content (ADR 0007) and any new
//!   row deserves a human review.
//! - **No host blacklist / allowlist.** Every distinct host in the
//!   input TSV gets considered. Hosts whose status-vs-UA cross-tab
//!   doesn't justify an override are silently skipped (the
//!   default policy in `classify_status` is conservative and
//!   correct for them).
//! - **No source-specific routing.** The rule below uses only the
//!   status codes and the UA label strings the probe itself
//!   emitted; nothing in this binary names a host, a publisher,
//!   or a domain.
//!
//! ## The mapping rule
//!
//! The host-probe binary tries four UA strings against each URL
//! (see `host_probe.rs`). For each host:
//!
//!   - If `default_situation_room` returned 403 AND
//!     `situation_room_with_email` returned 200 → mapping is
//!     `HostRequiresUaPolicy` (the host enforces a UA shape that
//!     includes a contact email; SEC EDGAR is the canonical
//!     example).
//!   - Else if `default_situation_room` returned 403 AND
//!     `chrome_macos` returned 200 (regardless of googlebot) →
//!     mapping is `HostRequiresUaPolicy` (the host accepts a
//!     browser UA; populating the override flips the proposer's
//!     class from "WAF, pivot host" to "UA policy, try a
//!     browser-class UA").
//!   - Else if `default_situation_room` returned 403 AND every
//!     non-default UA also returned 403 (or worse — 401, 5xx,
//!     timeout) → mapping is `HostBlockedByWaf` (the host
//!     fingerprints beyond UA; the conservative reading is
//!     correct, but we still emit the explicit override so the
//!     classifier's reasoning is documented at the override site
//!     rather than inferred from the default).
//!   - Otherwise → no override emitted. The default policy in
//!     `classify_status` is correct for this host.
//!
//! Hosts whose `default_situation_room` row didn't see a 403 are
//! skipped entirely — there's no override to make.
//!
//! ## Output shape
//!
//! Rust syntax suitable for paste into the `HOST_CLASS_OVERRIDES`
//! constant. Example:
//!
//! ```text
//! // 4 host-probe rows · status (default → email → googlebot → chrome): 403 → 200 → 403 → 200
//! ("sec.gov", FetchOutcomeClass::HostRequiresUaPolicy),
//! ```
//!
//! The leading comment names the probe-observed status sequence so
//! a future reviewer can see at a glance why the row earned its
//! class. The host string is suffix-form (no `www.`); the matcher
//! in `fetch_classes::host_class_override` is suffix-anchored so
//! `www.sec.gov` resolves to a `sec.gov` entry.

use std::collections::BTreeMap;
use std::io::{self, Read};

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "host-probe-to-overrides",
    version,
    about = "Convert host-probe TSV (stdin or --input) into HOST_CLASS_OVERRIDES entries on stdout."
)]
struct Cli {
    /// Optional path to a TSV file; if omitted, reads stdin. The
    /// TSV must carry the same columns `host-probe` emits:
    /// `url`, `ua_label`, `status`, `response_bytes`, `body_preview`.
    #[arg(long)]
    input: Option<String>,
}

/// The closed vocabulary of UA labels the `host-probe` binary
/// emits. Mirrored here as a literal constant so this binary is
/// honest about what TSV shape it expects; a future label addition
/// in `host_probe.rs` needs a matching update here. Stays in
/// `&[&str]` rather than an `enum` because the values are pasted
/// verbatim into log messages and the human-readable output.
const UA_LABELS: &[&str] = &[
    "default_situation_room",
    "situation_room_with_email",
    "googlebot",
    "chrome_macos",
];

/// One UA's observed status from the probe. Stored per-host so we
/// can apply the mapping rule after the input has been fully
/// streamed.
#[derive(Debug, Default, Clone)]
struct PerUaStatus {
    /// The literal status string from the TSV — `"200"`,
    /// `"403"`, `"timeout"`, etc. `None` when this UA wasn't
    /// probed for this host (a malformed or partial input).
    status: Option<String>,
}

/// Per-host accumulator. UA-label → status. Hosts arrive in
/// alphabetical order via `BTreeMap` so the output is reproducible
/// across runs of the same TSV input.
type HostMap = BTreeMap<String, BTreeMap<String, PerUaStatus>>;

/// The proposed class for one host, with the supporting status
/// sequence so the comment line can carry the evidence. `None`
/// means "no override needed; default policy applies."
struct Proposal {
    host: String,
    class: &'static str,
    status_sequence: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut buf = String::new();
    if let Some(path) = &cli.input {
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("opening input file {path}"))?;
        file.read_to_string(&mut buf)
            .with_context(|| format!("reading input file {path}"))?;
    } else {
        io::stdin()
            .lock()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
    }

    let hosts = parse_tsv(&buf)?;
    let proposals = derive_proposals(&hosts);

    if proposals.is_empty() {
        eprintln!(
            "no overrides proposed — every host in the input either succeeded \
             on the default UA, returned a non-403 failure, or didn't see \
             behaviour changes under any non-default UA."
        );
        return Ok(());
    }

    // Banner comment so the operator's paste lands inside the
    // existing leading comment block in HOST_CLASS_OVERRIDES.
    println!(
        "// host-probe-to-overrides — {} proposed entries from {} host{} of probe data.",
        proposals.len(),
        hosts.len(),
        if hosts.len() == 1 { "" } else { "s" },
    );
    println!("// Review each line; paste into HOST_CLASS_OVERRIDES in crates/pipeline/src/fetch_classes.rs.");
    println!();

    for p in &proposals {
        println!("// {} · {}", p.host, p.status_sequence);
        println!("(\"{}\", FetchOutcomeClass::{}),", p.host, p.class);
    }

    Ok(())
}

/// Parse the TSV. Skips the header row if present (heuristic:
/// header starts with `url\t`). Returns one row per UA × host
/// observation. Hosts are derived from the URL's host component
/// — same suffix discipline the override map applies, so an entry
/// for `www.sec.gov` and `sec.gov` collapse into one bucket.
fn parse_tsv(input: &str) -> Result<HostMap> {
    let mut hosts: HostMap = BTreeMap::new();
    for (lineno, line) in input.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Skip the canonical header.
        if lineno == 0 && line.starts_with("url\t") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            // Tolerate stray short rows; they're either a wrapped
            // body_preview or a partial flush.
            continue;
        }
        let url = cols[0];
        let ua_label = cols[1];
        let status = cols[2];

        if !UA_LABELS.iter().any(|l| *l == ua_label) {
            // Unknown UA label — skip without erroring; the probe
            // may have grown a new label that this binary hasn't
            // been taught about.
            continue;
        }

        let host = host_of(url).unwrap_or_default();
        if host.is_empty() {
            continue;
        }

        hosts
            .entry(host)
            .or_default()
            .entry(ua_label.to_string())
            .or_insert_with(|| PerUaStatus { status: None })
            .status = Some(status.to_string());
    }
    Ok(hosts)
}

/// Apply the mapping rule (see the module docs) to each host's
/// status table and emit proposals.
fn derive_proposals(hosts: &HostMap) -> Vec<Proposal> {
    let mut out = Vec::new();
    for (host, ua_map) in hosts {
        let get = |label: &str| -> Option<String> {
            ua_map.get(label).and_then(|s| s.status.clone())
        };
        let default = get("default_situation_room");
        let email = get("situation_room_with_email");
        let googlebot = get("googlebot");
        let chrome = get("chrome_macos");

        // No override needed unless the default UA hit 403. This
        // mirrors the gate in `classify_status`: only 403 routes
        // through the override map at runtime.
        if default.as_deref() != Some("403") {
            continue;
        }

        let class = if email.as_deref() == Some("200") {
            "HostRequiresUaPolicy"
        } else if chrome.as_deref() == Some("200") {
            "HostRequiresUaPolicy"
        } else if all_blocked(&[&email, &googlebot, &chrome]) {
            "HostBlockedByWaf"
        } else {
            // Mixed signal — the default UA failed but the other
            // UAs returned something other than 200 or a clear
            // block. Skip rather than guess; the operator can
            // re-run the probe with a tighter URL list.
            continue;
        };

        let status_sequence = format!(
            "status (default → email → googlebot → chrome): {} → {} → {} → {}",
            default.as_deref().unwrap_or("—"),
            email.as_deref().unwrap_or("—"),
            googlebot.as_deref().unwrap_or("—"),
            chrome.as_deref().unwrap_or("—"),
        );

        out.push(Proposal {
            host: host.clone(),
            class,
            status_sequence,
        });
    }
    out
}

/// Returns true when every UA's observed status is non-200
/// (403, 401, 5xx, timeout, …). Distinguishes the
/// "host fingerprints beyond UA" case from the "mixed signal"
/// case.
fn all_blocked(statuses: &[&Option<String>]) -> bool {
    statuses
        .iter()
        .all(|s| s.as_deref().map(|v| v != "200").unwrap_or(true))
}

/// Strip the URL down to its host. `www.` is preserved here (the
/// runtime matcher is suffix-anchored so a `sec.gov` entry will
/// match `www.sec.gov` rows automatically); the operator can
/// choose to keep or drop the `www.` prefix when pasting.
fn host_of(url: &str) -> Option<String> {
    // Tiny dependency-free parser: find `://`, then the next `/`
    // or end-of-string. Strip the userinfo and port.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host_with_path = after_scheme.split('/').next().unwrap_or("");
    let host = host_with_path
        .split('@')
        .next_back()
        .unwrap_or(host_with_path)
        .split(':')
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_strips_scheme_path_and_port() {
        assert_eq!(host_of("https://www.sec.gov/edgar"), Some("www.sec.gov".to_string()));
        assert_eq!(host_of("http://example.com:8080/x"), Some("example.com".to_string()));
        assert_eq!(host_of("https://user@example.com/x"), Some("example.com".to_string()));
    }

    fn make_input(rows: &[(&str, &str, &str)]) -> String {
        let mut s = String::new();
        s.push_str("url\tua_label\tstatus\tresponse_bytes\tbody_preview\n");
        for (url, ua, status) in rows {
            s.push_str(&format!("{url}\t{ua}\t{status}\t0\t\n"));
        }
        s
    }

    #[test]
    fn email_ua_unblocks_403_proposes_ua_policy() {
        // SEC-pattern: default 403, email 200, others 403.
        let input = make_input(&[
            ("https://www.sec.gov/edgar", "default_situation_room", "403"),
            ("https://www.sec.gov/edgar", "situation_room_with_email", "200"),
            ("https://www.sec.gov/edgar", "googlebot", "403"),
            ("https://www.sec.gov/edgar", "chrome_macos", "403"),
        ]);
        let hosts = parse_tsv(&input).unwrap();
        let proposals = derive_proposals(&hosts);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].host, "www.sec.gov");
        assert_eq!(proposals[0].class, "HostRequiresUaPolicy");
    }

    #[test]
    fn chrome_ua_unblocks_403_proposes_ua_policy() {
        // Browser-UA-friendly host: default 403, chrome 200.
        let input = make_input(&[
            ("https://news.example.com/article/1", "default_situation_room", "403"),
            ("https://news.example.com/article/1", "situation_room_with_email", "403"),
            ("https://news.example.com/article/1", "googlebot", "403"),
            ("https://news.example.com/article/1", "chrome_macos", "200"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].host, "news.example.com");
        assert_eq!(proposals[0].class, "HostRequiresUaPolicy");
    }

    #[test]
    fn everything_blocked_proposes_waf() {
        // WAF that fingerprints beyond UA: every UA blocked or 401.
        let input = make_input(&[
            ("https://wall.example.com/x", "default_situation_room", "403"),
            ("https://wall.example.com/x", "situation_room_with_email", "403"),
            ("https://wall.example.com/x", "googlebot", "403"),
            ("https://wall.example.com/x", "chrome_macos", "403"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].host, "wall.example.com");
        assert_eq!(proposals[0].class, "HostBlockedByWaf");
    }

    #[test]
    fn default_ua_200_skips_host() {
        // Host responds fine to the default UA — no override needed.
        let input = make_input(&[
            ("https://api.example.com/", "default_situation_room", "200"),
            ("https://api.example.com/", "situation_room_with_email", "200"),
            ("https://api.example.com/", "googlebot", "200"),
            ("https://api.example.com/", "chrome_macos", "200"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert!(proposals.is_empty());
    }

    #[test]
    fn default_ua_404_skips_host() {
        // Default UA hit a 404 — that's a URL-shape problem, not a
        // host class problem. The override map is consulted only on
        // 403 (see `classify_status`); skip.
        let input = make_input(&[
            ("https://path.example.com/missing", "default_situation_room", "404"),
            ("https://path.example.com/missing", "situation_room_with_email", "404"),
            ("https://path.example.com/missing", "googlebot", "404"),
            ("https://path.example.com/missing", "chrome_macos", "404"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert!(proposals.is_empty());
    }

    #[test]
    fn mixed_signal_skips_host_rather_than_guess() {
        // Default 403, email 403, googlebot timeout, chrome 401. No
        // clean signal — operator should re-probe rather than have
        // this binary guess a class. Skip.
        let input = make_input(&[
            ("https://weird.example.com/", "default_situation_room", "403"),
            ("https://weird.example.com/", "situation_room_with_email", "403"),
            ("https://weird.example.com/", "googlebot", "timeout"),
            ("https://weird.example.com/", "chrome_macos", "401"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        // Per the rule, "everything non-200" → HostBlockedByWaf.
        // This is intentional: 401/timeout under non-default UAs
        // is still "the host does not let us in," so the
        // conservative reading earns the override.
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].class, "HostBlockedByWaf");
    }

    #[test]
    fn hosts_emit_in_stable_alphabetical_order() {
        // BTreeMap means the output is deterministic across runs of
        // the same input. Important so a future reviewer can diff
        // two TSVs and see only the meaningful changes.
        let input = make_input(&[
            ("https://b.example.com/", "default_situation_room", "403"),
            ("https://b.example.com/", "situation_room_with_email", "200"),
            ("https://b.example.com/", "googlebot", "403"),
            ("https://b.example.com/", "chrome_macos", "403"),
            ("https://a.example.com/", "default_situation_room", "403"),
            ("https://a.example.com/", "situation_room_with_email", "200"),
            ("https://a.example.com/", "googlebot", "403"),
            ("https://a.example.com/", "chrome_macos", "403"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].host, "a.example.com");
        assert_eq!(proposals[1].host, "b.example.com");
    }

    #[test]
    fn ignores_unknown_ua_labels() {
        // A future probe extension may emit a new UA label; this
        // binary should ignore it without crashing.
        let input = make_input(&[
            ("https://x.example.com/", "default_situation_room", "403"),
            ("https://x.example.com/", "situation_room_with_email", "200"),
            ("https://x.example.com/", "googlebot", "403"),
            ("https://x.example.com/", "chrome_macos", "403"),
            ("https://x.example.com/", "future_label_we_dont_know_yet", "200"),
        ]);
        let proposals = derive_proposals(&parse_tsv(&input).unwrap());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].class, "HostRequiresUaPolicy");
    }

    #[test]
    fn parse_tsv_tolerates_blank_lines_and_short_rows() {
        let input =
            "url\tua_label\tstatus\tresponse_bytes\tbody_preview\n\n\
             https://a.example.com/\tdefault_situation_room\t200\t0\t\n\
             short\n\
             https://a.example.com/\tchrome_macos\t200\t0\t\n";
        let hosts = parse_tsv(input).unwrap();
        // One host bucket, two UA rows in it; the "short" row is
        // skipped without error.
        assert_eq!(hosts.len(), 1);
    }
}
