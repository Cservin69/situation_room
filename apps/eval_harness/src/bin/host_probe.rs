//! Host probe — diagnostic for the "coverage-publisher block wall"
//! pattern (Session 56 + 57). For each URL on the CLI, runs GET
//! against the production `SecureHttpClient` with several
//! User-Agent strings, recording the resulting status (or error
//! category) and — on the 2xx path — a 256-byte preview of the
//! response body. Output is TSV to stdout so the operator can paste
//! it back without losing structure.
//!
//! ## Why this exists
//!
//! Sessions 56–57 saw lithium and south-korea runs return a wall of
//! 401/403/404/timeout from major coverage publishers (Reuters,
//! Bloomberg, SEC EDGAR, Fastmarkets, Realmeter, industry.gov.au).
//! The shallow patch — "add a browser UA" — would silently paper
//! over four distinct failure modes that need different responses:
//!
//!   1. Paywall (Reuters): UA does not help.
//!   2. WAF/Cloudflare on non-residential IP (Bloomberg): UA only
//!      sometimes helps; TLS / HTTP-2 fingerprinting and IP class
//!      also matter.
//!   3. UA-policy enforcement (SEC EDGAR — requires email-bearing
//!      UA per the published fair-access doc): UA helps directly.
//!   4. Hallucinated URLs (Fastmarkets/Gallup/IEA-2024): UA
//!      irrelevant; this is a proposer-knowledge gap, not a
//!      fetcher gap.
//!
//! Without probe data we cannot know which host falls in which
//! bucket. This binary's job is to produce that data so the ADR
//! that follows can prescribe per-bucket responses instead of a
//! single one-size-fits-all UA tweak.
//!
//! ## What this binary does NOT do
//!
//! - It does not rewrite any production code. It only observes.
//! - It does not capture the body on non-2xx responses — the
//!   `SecureHttpClient::get_bytes` surface drops the body when the
//!   status is non-success, returning `HttpError::Status(code)`.
//!   The status code is the most-load-bearing signal anyway; if a
//!   future revision needs the 4xx body (e.g. to confirm a paywall
//!   string) we can lift `get_with_headers_internal` to a public
//!   surface, scoped to this binary.
//!
//! ## Output (TSV columns)
//!
//! url, ua_label, status, response_bytes, body_preview
//!
//! `status` is the HTTP status code as a string (e.g. `200`, `401`)
//! or one of: `timeout`, `request_err:<msg>`, `url_rejected:<msg>`,
//! `redirect_rejected:<msg>`, `too_large:max=…,got=…`,
//! `tls_err:<msg>`. `body_preview` is the first 256 bytes of the
//! body on 2xx responses, ASCII-escaped on a single line; empty on
//! any error path.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use situation_room_secure::http::{HttpError, SecureHttpClient, SecureHttpConfig};

#[derive(Parser, Debug)]
#[command(
    name = "host-probe",
    version,
    about = "Probe one or more URLs with several User-Agent strings using the production SecureHttpClient."
)]
struct Cli {
    /// One or more URLs to probe. Pass them as bare arguments; they
    /// are probed in order, one row per (url × UA).
    #[arg(required = true)]
    urls: Vec<String>,

    /// How long to allow each individual request before reporting
    /// `timeout`. Defaults to 30s — comfortably above the slowest
    /// healthy host in our sample, tight enough that a probe sweep
    /// over a long URL list finishes in a reasonable wall-clock
    /// window. Note: the production prefetch client uses 60s; this
    /// probe's tighter ceiling is intentional so timeout-class
    /// hosts surface as `timeout` quickly without distorting the
    /// sweep.
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

/// The UA strings probed for each URL. The four are intentionally
/// chosen to discriminate the four hypotheses in this binary's
/// module docs, with no overlap in what they test:
///
///   - `default_situation_room`: what the production fetcher sends
///     today (Session 45's build-time identifier). Reproduces the
///     production status code as the baseline.
///   - `situation_room_with_email`: SEC EDGAR's fair-access docs
///     require a contact email; this row tests whether adding one
///     unblocks SEC specifically without changing identification
///     in any other way.
///   - `googlebot`: a published, well-known crawler UA. Some
///     Cloudflare setups whitelist known crawlers; others actively
///     block any non-residential IP regardless of UA. This row
///     splits those cases.
///   - `chrome_macos`: a current Chrome UA. Tests whether the WAF
///     decides on UA shape alone, or whether it also fingerprints
///     TLS / HTTP-2 / IP class. (If `chrome_macos` succeeds where
///     `googlebot` fails, the WAF is doing UA shape; if both fail,
///     the WAF is fingerprinting beyond UA.)
const UAS: &[(&str, &str)] = &[
    (
        "default_situation_room",
        // Mirrors `SecureHttpConfig::default().user_agent` —
        // Session 45's build-time identifier. Hard-coded here so a
        // future bump to the workspace version does not silently
        // change which UA string the probe is "reproducing".
        "SituationRoom/0.1.0 (+https://github.com/Cservin69/situation_room.git)",
    ),
    (
        "situation_room_with_email",
        // Drop-in SEC-fair-access compliant variant. Same identity,
        // adds a contact email per the published policy. The
        // example.com host is intentional — we are NOT registering
        // a real contact here; that lives in the per-host adapter
        // discussion in the ADR. This row only tests whether SEC's
        // policy enforcer recognises an email-shaped suffix.
        "SituationRoom/0.1.0 contact@situation-room.example (+https://github.com/Cservin69/situation_room.git)",
    ),
    (
        "googlebot",
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
    ),
    (
        "chrome_macos",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    ),
];

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // TSV header on stdout. Stderr stays for any `tracing` output
    // the underlying client might emit, so the TSV stream is clean.
    println!("url\tua_label\tstatus\tresponse_bytes\tbody_preview");

    for url in &cli.urls {
        for (label, ua) in UAS {
            // Build a fresh client per (URL × UA) so each row is an
            // isolated request — no connection reuse can mask
            // per-request behaviour. This is the same construction
            // shape as the production composition root, only the
            // `user_agent` and `total_timeout` fields are
            // overridden.
            let cfg = SecureHttpConfig {
                total_timeout: Duration::from_secs(cli.timeout_secs),
                user_agent: (*ua).to_string(),
                ..SecureHttpConfig::default()
            };
            let client = SecureHttpClient::new(cfg).with_context(|| {
                format!("building client for {url} with {label}")
            })?;

            let row = probe_one(&client, url).await;
            // body_preview is escaped to a single TSV-safe line:
            // tabs/newlines stripped, non-ASCII rendered as `\xNN`.
            // Truncated to 256 bytes of source so the row stays
            // scannable.
            let preview = escape_tsv_preview(&row.body_preview, 256);
            println!(
                "{url}\t{label}\t{}\t{}\t{}",
                row.status, row.response_bytes, preview,
            );
        }
    }

    Ok(())
}

struct ProbeRow {
    status: String,
    response_bytes: usize,
    body_preview: Vec<u8>,
}

async fn probe_one(client: &SecureHttpClient, url: &str) -> ProbeRow {
    match client.get_bytes(url).await {
        Ok(body) => {
            let preview = body.iter().copied().take(256).collect::<Vec<u8>>();
            ProbeRow {
                status: "200".to_string(),
                response_bytes: body.len(),
                body_preview: preview,
            }
        }
        Err(HttpError::Status(code)) => ProbeRow {
            status: code.to_string(),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::Timeout(_)) => ProbeRow {
            status: "timeout".to_string(),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::Request(s)) => ProbeRow {
            // Inline newlines/tabs in error messages would corrupt
            // the TSV; sanitize before embedding.
            status: format!("request_err:{}", s.replace(['\t', '\n', '\r'], " ")),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::UrlRejected(v)) => ProbeRow {
            status: format!("url_rejected:{v}"),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::ResponseTooLarge { max, got }) => ProbeRow {
            status: format!("too_large:max={max},got={got}"),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::RedirectRejected(s)) => ProbeRow {
            status: format!("redirect_rejected:{s}"),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::Tls(s)) => ProbeRow {
            status: format!("tls_err:{}", s.replace(['\t', '\n', '\r'], " ")),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
        Err(HttpError::StatusWithHeaders { status, .. }) => ProbeRow {
            // Reachable in principle if a future SecureHttpClient
            // revision routes get_bytes through the headers-aware
            // path. Keep the row TSV-safe by emitting the numeric
            // status; the headers themselves are not surfaced here.
            status: status.to_string(),
            response_bytes: 0,
            body_preview: Vec::new(),
        },
    }
}

/// Make a body-preview TSV-safe: take up to `max` bytes, replace
/// tab/newline/CR with spaces, escape non-printable bytes as
/// `\xNN`. The result fits on one TSV line and contains only
/// printable ASCII so terminal display is predictable.
fn escape_tsv_preview(bytes: &[u8], max: usize) -> String {
    let mut out = String::with_capacity(max);
    for b in bytes.iter().take(max) {
        match *b {
            b'\t' | b'\n' | b'\r' => out.push(' '),
            0x20..=0x7E => out.push(*b as char),
            _ => out.push_str(&format!("\\x{:02X}", b)),
        }
    }
    out
}
