# ADR 0009 — Security posture

**Status**: Accepted
**Date**: 2026-04-19

## Decision

Stockpile treats cybersecurity as a first-class concern from Phase 1
onward, not as a polish step. Cross-cutting security primitives live in
`crates/secure`; every other crate depends on it rather than hand-rolling
equivalents. The detailed threat model lives in
`docs/security/threat_model.md`.

## Key primitives

1. **`ApiKey` / `SecretString`** (secrets.rs) — secrets never Serialize,
   never Display, Debug prints only fingerprints, zeroized on drop,
   loaded only from env.
2. **`SecureHttpClient`** (http.rs) — rustls-only TLS 1.2+, URL validation
   on every request and redirect, bounded response sizes, no ambient
   cookies. All outbound HTTP uses this.
3. **`UrlGuard`** (url_guard.rs) — rejects non-HTTP(S), private IPs,
   cloud metadata endpoints, localhost, non-allowlisted ports,
   embedded credentials.
4. **`FsGuard`** (fs_guard.rs) — path-traversal-safe resolution against
   a designated root. Used for article archive and anywhere user input
   influences filesystem paths.
5. **`Bounds`** (bounds.rs) — named size limits for config, source
   responses, LLM prompts/responses, URLs, topics, collection entries.
6. **Scrubbed logging** (logging.rs) — every log line passes through a
   writer that redacts Bearer tokens and known key prefixes.

## Build-level hardening

- Toolchain pinned via `rust-toolchain.toml`.
- `.cargo/config.toml` enables overflow checks in release, PIE + relro
  + noexecstack on Linux, enforces crates.io as the only registry.
- `panic = "abort"` in release.
- `cargo deny` configured: license allowlist, source allowlist, deny
  list includes openssl* / native-tls (rustls only).
- CI runs fmt, clippy (`-D warnings`), test, `cargo deny`, `cargo audit`.

## Tauri posture

- Strict CSP: `default-src 'self'`, `connect-src` restricted to `ipc:`,
  `object-src 'none'`, `frame-src 'none'`, `freezePrototype: true`.
- Capabilities file enumerates every allowed IPC command; `core:shell`,
  `core:fs`, `core:http`, `core:process` are DISABLED.
- macOS hardened runtime with minimal entitlements.

## Trade-offs

- We do **not** pin certificates. Pinning is too fragile for a long-lived
  OSS tool connecting to dozens of rotating services.
- We do **not** implement DNS-over-HTTPS. Out of scope for a desktop app.
- We do **not** require a master password to decrypt stored keys — keys
  live in the user's shell environment, where the OS handles access
  control. A future release may add keychain integration.

## Rationale

[To be written in full with the human reviewer. Captures the threat model
discussion from the conversation, why a dedicated `secure` crate beats
scattering security concerns across every other crate, and the explicit
trade-offs on cert pinning, DoH, and keychain integration.]
