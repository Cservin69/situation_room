# ADR 0009 — Security posture

**Status**: Accepted
**Date**: 2026-04-20
**Related**: ADR 0001 (monorepo layout), ADR 0007 (research function)
**Threat model**: `docs/security/threat_model.md`

## Context

Situation_room fetches from dozens of external sources, sends user-derived
strings to LLM providers, stores API keys, and writes files based on
configuration that may ultimately come from an ingested document.
Each of those is a failure surface. The question is whether security
is a cross-cutting concern addressed from Phase 1, or a hardening
pass applied later once the product works.

Hardening passes are a common failure mode in OSS tooling. By the
time a project reaches "now we'll add the security layer," its
fetch code, secret handling, and filesystem access are scattered
across every crate, each crate's maintainer has their own idea of
what "good enough" looks like, and the retrofit is expensive.
Typical symptoms: a `reqwest::Client::new()` in every crate with
slightly different timeouts, API keys `Debug`-printed in error
messages, SSRF via user-supplied URLs because someone didn't know
what SSRF was.

We decided to pay the cost upfront: security is load-bearing from
Phase 1, the primitives live in one place, and every other crate
depends on them rather than rolling its own.

## Decision

**Cybersecurity is a Phase-1 concern, not a polish step.** The
`situation_room_secure` crate centralizes the primitives that every other
crate must use. The detailed threat model lives in
`docs/security/threat_model.md`. Every new crate, every new source
adapter, every new outbound fetch must route through these
primitives or explain (in code review) why it doesn't.

### The primitives

All live in `crates/secure/src/`. Exports are re-exported from the
crate root for convenient use.

1. **`ApiKey` / `SecretString`** (`secrets.rs`). API keys and other
   secrets are wrapped in types that: never `Serialize`, never
   `Display`, print only a fingerprint from `Debug`, and zeroize
   on drop via the `secrecy` and `zeroize` crates. Keys are loaded
   only from environment variables — never from config files,
   never from command-line arguments, never from disk.
2. **`SecureHttpClient` / `SecureHttpConfig`** (`http.rs`). The one
   HTTP client Situation_room uses. Rustls-only TLS (no OpenSSL anywhere
   in the dependency tree; enforced by `cargo deny`), TLS 1.2+
   required at the client level, URL validation on every request
   *and* every redirect, bounded response sizes, timeouts, no
   ambient cookies.
3. **`UrlGuard` / `UrlViolation`** (`url_guard.rs`). Rejects URLs
   that: use schemes other than HTTP(S), resolve to private IP
   ranges, point at localhost, point at cloud metadata endpoints
   (169.254.169.254 and friends), use non-allowlisted ports, or
   carry embedded credentials (`user:pass@host`). Every URL that
   Situation_room fetches passes through this guard.
4. **`FsGuard` / `FsViolation`** (`fs_guard.rs`). Path-traversal-safe
   filesystem access. Resolves paths against a designated workspace
   root; rejects any path that escapes the root via `..` or
   symlinks. Used anywhere user input (config, ingested documents,
   extracted fields) influences a filesystem path.
5. **`Bounds` / `BoundsViolation`** (`bounds.rs`). Named size
   limits: config size, source response size, LLM prompt size, LLM
   response size, URL length, topic string length, collection
   entry count. Used during deserialization to reject
   oversized inputs before they reach any processing code.
6. **Scrubbed logging** (`logging.rs`). A tracing subscriber wrapper
   that scrubs every log line for known secret patterns (Bearer
   tokens, known API key prefixes) before it reaches stdout or
   disk. `situation_room_secure::logging::init()` is the only way the
   binary configures logging.

### The rule

Every other crate depends on `situation_room_secure` and routes through
these primitives. If a crate needs HTTP, it uses `SecureHttpClient`.
If a crate needs to read a URL from user input, it passes through
`UrlGuard`. If a crate writes a file derived from user input, it
passes through `FsGuard`. If a crate handles a secret, it uses
`ApiKey` or `SecretString`. No hand-rolling.

**Enforcement is partially automated and partially by review.**
Automated:

- `cargo deny` blocks `openssl-*`, `native-tls`, and other crates
  that would reintroduce non-rustls TLS. Adding one of these is a
  build failure.
- CI runs `cargo audit` on every change. New advisories fail the
  build.
- `reqwest` is locked to `default-features = false, features = [...,
  "rustls-tls"]`. A PR that adds `native-tls` to the feature list
  fails CI.

By review: a new `reqwest::Client::new()` anywhere in the codebase
is a review-blocking bug. Same for `std::fs::write` on a user-
derived path, or `println!` of a struct that might contain a secret.
These aren't mechanically enforceable today; they're design-review
checks that contributors internalize.

### Build-level hardening

- Rust toolchain pinned via `rust-toolchain.toml` (currently 1.86).
- `.cargo/config.toml` enables overflow checks in release builds,
  PIE + relro + noexecstack on Linux, and enforces crates.io as the
  only registry (no git-dep backdoors).
- `panic = "abort"` in release — no unwind-based exploit surfaces.
- CI runs `cargo fmt --check`, `cargo clippy -D warnings`, full
  tests, `cargo deny`, and `cargo audit` on every PR.

### Tauri posture

The desktop app uses Tauri for its webview host. Tauri's default
posture is permissive; Situation_room's is not:

- Strict CSP: `default-src 'self'`, `connect-src` restricted to
  `ipc:`, `object-src 'none'`, `frame-src 'none'`,
  `freezePrototype: true`.
- The capabilities file enumerates every allowed IPC command
  explicitly. `core:shell`, `core:fs`, `core:http`, `core:process`
  are **disabled**. The webview cannot shell out, read/write the
  filesystem directly, make HTTP calls, or spawn processes.
  Everything goes through the enumerated Rust commands.
- macOS hardened runtime enabled with the minimum set of
  entitlements.

### Explicit non-commitments

Security decisions worth naming because they're the ones a reader
will ask about:

- **No certificate pinning.** Pinning is too fragile for a long-
  lived OSS tool connecting to dozens of third-party services that
  rotate certs on their own schedule. A mispinned cert is
  indistinguishable from a genuine rotation, and the recovery path
  is "ship a new build to users" — worse than trusting the system
  CA store.
- **No DNS-over-HTTPS at the application layer.** The OS resolver
  is the right layer for this. A desktop app trying to implement
  its own DNS would be reinventing the OS poorly.
- **No master password for stored secrets.** API keys live in the
  user's shell environment. The OS (and the user's dotfiles hygiene)
  handles access control. A future release may add OS keychain
  integration; until then, `env` is the deliberate boundary.

## Rationale

**Why a dedicated crate, not per-crate primitives.** Three
properties fall out of centralization that scatter-the-concerns
can't match:

1. *One place to audit.* When a CVE drops in reqwest, rustls, or
   secrecy, we upgrade one crate and ship. Every consumer inherits
   the fix. With scattered primitives, each crate has its own
   version pins and the fix has to be propagated by hand.
2. *One place to test.* `SecureHttpClient` has adversarial unit
   tests for SSRF, TLS downgrade, redirect abuse, and response-size
   exhaustion. Every consumer benefits from those tests without
   rewriting them.
3. *One place to enforce.* `cargo deny` and `cargo audit` look at a
   single dependency graph. Forbidden dependencies show up as build
   failures, not as ignored warnings buried in a less-maintained
   crate.

**Why rustls-only, no OpenSSL.** OpenSSL's memory-unsafety
footprint, release cadence, and historical CVE track record are all
strictly worse than rustls. Pure-Rust TLS means no C compilation,
no linker games, no per-platform TLS library hunts. The tradeoff —
rustls doesn't support some enterprise cert stores — doesn't bite
us; Situation_room talks to public internet endpoints, not enterprise
HTTPS-inspecting proxies.

**Why load API keys only from env.** Config-file loading makes it
too easy to commit secrets by accident. Command-line loading puts
secrets in `ps` output. Disk loading puts secrets in backups that
the user may not realize contain them. Environment variables are
the narrowest sensible interface that the user still has full
control over, and they die with the process.

**Why URL validation happens on every request *and* every redirect.**
A server can return a redirect pointing to
`http://169.254.169.254/latest/meta-data/` (the AWS metadata
endpoint). A naive HTTP client follows it. `SecureHttpClient` runs
`UrlGuard` on the redirect target before following. This is the
SSRF defense that matters in practice — attackers don't supply
malicious URLs directly if they can just set up a redirect.

**Why bounds are named, not ad-hoc.** A single named `Bounds`
registry means "how big is a valid LLM response" is answered in one
place. Raising the limit because a source exceeded it happens once,
with an audit-trail in git. Without a registry, each consumer picks
its own limit and some of them are zero or `usize::MAX`.

**Why logging is scrubbed even though we shouldn't be logging
secrets.** "Shouldn't" is aspirational; "scrubbed" is structural.
A new contributor adding a `tracing::debug!` with a struct that
happens to contain a Bearer header gets the header redacted before
it reaches disk. Belt and suspenders.

## Alternatives considered

**Security as a hardening pass in Phase 4.** Rejected: the
retrofit is expensive, and Phases 2 and 3 would accumulate
violations that have to be hunted down. Paying upfront is cheaper.

**Per-crate security primitives.** Rejected: see "Why a dedicated
crate" above.

**Use `rustls-native-certs` to pick up system cert stores.** Kept
as an option for later. For now, the rustls defaults suffice, and
introducing native-certs adds a dependency that has OS-specific
build gotchas. Revisit if users report connection failures to
sources we should be reaching.

**Keychain integration for secret storage.** Deferred. The env-var
boundary is simpler, portable, and auditable. Keychain integration
is a worthwhile Phase 4+ feature; it's not worth the complexity
now.

**Custom DNS stack (DoH/DoT).** Rejected: wrong layer. If a user
has DNS concerns, they configure those at the OS level, where they
affect every app consistently.

## Consequences

**Positive**

- Security posture is consistent across the codebase. A new
  contributor learns the `situation_room_secure` crate once.
- CVE response is fast: upgrade one crate, release.
- SSRF, secret leakage, path traversal, and TLS downgrade all have
  defined defenses with tests. Regressions fail CI.
- `cargo deny` + `cargo audit` + pinned toolchain give a
  reproducible, auditable build.

**Negative**

- Developer friction: "why can't I just call `reqwest`?" is a
  recurring contributor question. Mitigated by documentation and by
  making `SecureHttpClient` ergonomic enough that it's not actively
  painful.
- Dependency on rustls means we can't connect through
  enterprise-style HTTPS-inspecting proxies without additional work.
  Not a concern for the target use case (a developer's desktop
  fetching public sources).
- Scrubbed logging means some debug output looks weirdly redacted.
  Preferable to the alternative.

**Neutral**

- The Tauri capabilities file is a living document: every new IPC
  command must be enumerated. Discipline, not decision.
- The authoritative list of "what security-relevant things exist"
  is the module list in `crates/secure/src/lib.rs`. Anything not
  re-exported there isn't a sanctioned primitive.

## Code references

- `crates/secure/src/lib.rs` — module list and re-exports.
- `crates/secure/src/secrets.rs` — `ApiKey`, `SecretString`.
- `crates/secure/src/http.rs` — `SecureHttpClient`,
  `SecureHttpConfig`.
- `crates/secure/src/url_guard.rs` — `UrlGuard`, `UrlViolation`.
- `crates/secure/src/fs_guard.rs` — `FsGuard`, `FsViolation`.
- `crates/secure/src/bounds.rs` — `Bounds`, `BoundsViolation`.
- `crates/secure/src/logging.rs` — scrubbed logging init.
- `docs/security/threat_model.md` — the detailed threat model.
- `deny.toml` — `cargo deny` configuration.
- `.cargo/config.toml` — compiler hardening flags.
- `rust-toolchain.toml` — pinned toolchain.

## Review notes

Reviewed 2026-04-20. This ADR largely codifies work already shipped
in Phase 1. The stub had strong content; this revision reorganizes
it into the standard ADR structure (Context, Decision, Rationale,
Alternatives, Consequences, References) and makes the enforcement
boundary explicit (what's automated via `cargo deny`/`cargo audit`
versus what's caught at review time).

The commitments to rustls-only, env-var secret loading, and URL
validation on redirects are all carried over verbatim from the
original Phase 1 implementation — none were revisited during this
review. The non-commitments (no cert pinning, no DoH, no keychain)
were reaffirmed as appropriate for the current threat model and
deployment context; all three are explicitly revisitable if the
product context shifts.
