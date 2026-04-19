//! Logging setup with secret scrubbing.
//!
//! Every log line passes through a formatter that redacts patterns
//! matching common secret shapes (API keys, Bearer tokens, long hex/base64
//! strings). This is belt-and-suspenders: secrets are already wrapped in
//! [`crate::secrets::SecretString`] which doesn't Display/Debug their
//! contents, but if a contributor forgets and writes
//! `tracing::info!("key = {}", raw_key)` this catches it.
//!
//! ## Rules
//!
//! - Use `init()` once at startup. Don't build your own subscriber.
//! - Prefer structured logging: `tracing::info!(source = %id, ...)` over
//!   format strings. Structured fields are easier to scrub and to query.
//! - Never log the contents of `SecretString` or `ApiKey`. The Debug impls
//!   redact them, but don't tempt fate.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber. Call once at startup.
pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("stockpile=info,warn"));

    let writer = ScrubbingStdout::new();

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_ansi(false) // keep ANSI out of scrubbed output to simplify regex
        .init();
}

/// Cloneable writer factory that emits `ScrubbingWriter` instances pointing
/// at stdout. Implements `MakeWriter` so tracing-subscriber can produce a
/// fresh writer per event.
#[derive(Clone)]
struct ScrubbingStdout {
    // Arc<Mutex<>> so concurrent log events don't interleave mid-line.
    // Stdout has its own lock but we want to scrub then write atomically.
    buf: Arc<Mutex<Vec<u8>>>,
}

impl ScrubbingStdout {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::with_capacity(4096))),
        }
    }
}

impl<'a> MakeWriter<'a> for ScrubbingStdout {
    type Writer = ScrubbingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubbingWriter {
            buf: Arc::clone(&self.buf),
        }
    }
}

/// Per-event writer. Buffers the full record, scrubs it, writes to stdout.
pub struct ScrubbingWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for ScrubbingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut buf = self.buf.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "log buffer mutex poisoned")
        })?;
        buf.extend_from_slice(data);
        // Flush on newline to keep per-line scrubbing behavior.
        if data.contains(&b'\n') {
            let text = String::from_utf8_lossy(&buf).into_owned();
            let scrubbed = scrub(&text);
            let mut stdout = io::stdout().lock();
            stdout.write_all(scrubbed.as_bytes())?;
            stdout.flush()?;
            buf.clear();
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut buf = self.buf.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "log buffer mutex poisoned")
        })?;
        if !buf.is_empty() {
            let text = String::from_utf8_lossy(&buf).into_owned();
            let scrubbed = scrub(&text);
            let mut stdout = io::stdout().lock();
            stdout.write_all(scrubbed.as_bytes())?;
            stdout.flush()?;
            buf.clear();
        }
        Ok(())
    }
}

/// Scrub common secret patterns from a log string.
///
/// Patterns covered:
/// - `Authorization: Bearer <token>` → `Authorization: Bearer ***`
/// - Anthropic keys: `sk-ant-*`
/// - OpenAI keys: `sk-proj-*`, `sk-*`
/// - xAI keys: `xai-*`
/// - Google API keys: `AIza...` (39 chars typical)
/// - Long hex strings (≥32 chars)
/// - Long base64-ish strings (≥40 chars)
pub fn scrub(input: &str) -> String {
    let mut out = input.to_string();

    // Bearer tokens — preserve the prefix so log context survives
    for marker in ["Bearer ", "bearer "] {
        out = redact_after_marker(&out, marker);
    }

    // Known key prefixes — replace the full key with prefix + *** + last 4
    for prefix in ["sk-ant-", "sk-proj-", "xai-", "AIza"] {
        out = redact_prefixed(&out, prefix);
    }

    // Long base64-like runs (40+ chars) — catches JWTs, opaque tokens
    out = redact_long_runs(&out, 40, |c| {
        c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '-' || c == '_'
    });

    // Long hex runs (32+ chars) — likely a key or hash
    out = redact_long_runs(&out, 32, |c| c.is_ascii_hexdigit());

    out
}

fn redact_after_marker(input: &str, marker: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(marker) {
        out.push_str(&rest[..pos]);
        out.push_str(marker);
        let after = &rest[pos + marker.len()..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\n')
            .unwrap_or(after.len());
        if end > 0 {
            out.push_str("***");
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn redact_prefixed(input: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(prefix) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\n')
            .unwrap_or(after.len());
        let key = &after[..end];
        if key.len() >= prefix.len() + 8 {
            let last4 = &key[key.len() - 4..];
            out.push_str(prefix);
            out.push_str("***");
            out.push_str(last4);
        } else {
            out.push_str(key);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn redact_long_runs(input: &str, min_len: usize, is_body_char: fn(char) -> bool) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if is_body_char(chars[i]) {
            let start = i;
            while i < chars.len() && is_body_char(chars[i]) {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            if run.chars().count() >= min_len {
                let run_chars: Vec<char> = run.chars().collect();
                out.extend(&run_chars[..4]);
                out.push_str("***");
                out.extend(&run_chars[run_chars.len() - 4..]);
            } else {
                out.push_str(&run);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_bearer_token() {
        let input = "Authorization: Bearer abc123xyz456_definitely_a_token rest of line";
        let out = scrub(input);
        assert!(out.contains("Bearer ***"));
        assert!(!out.contains("abc123xyz456_definitely_a_token"));
    }

    #[test]
    fn scrubs_anthropic_key() {
        let input = "trying sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz1234567890 now";
        let out = scrub(input);
        assert!(!out.contains("AbCdEfGhIjKlMnOpQrStUv"));
        assert!(out.contains("sk-ant-***"));
    }

    #[test]
    fn scrubs_long_hex() {
        let input = "hash = 5f4dcc3b5aa765d61d8327deb882cf99aabbccddeeff end";
        let out = scrub(input);
        assert!(out.contains("***"));
        assert!(!out.contains("5f4dcc3b5aa765d61d8327deb882cf99"));
    }

    #[test]
    fn preserves_short_strings() {
        let input = "source=usgs_mcs count=42";
        // Note: the scrubber may touch "usgs_mcs" or "42" if they match
        // patterns — let's verify they don't
        let out = scrub(input);
        assert_eq!(out, input);
    }

    #[test]
    fn scrubs_aiza_google_key() {
        let input = "Google key AIzaSyDxx_fake_google_key_for_testing_12345 end";
        let out = scrub(input);
        assert!(out.contains("AIza***"));
        assert!(!out.contains("fake_google_key"));
    }
}
