# Session 3 — fixes patch (pre-3c.4)

Applies cleanly on top of Parts 1 + 2. Does **not** include the 3c.4
end-to-end demo work — that patch will follow once these fixes are
verified green on your side.

```bash
cd /Users/aben/RustroverProjects/stockpile
tar -xzvf ~/Downloads/stockpile_session3_fixes_patch.tar.gz
```

## What failed

The Part 2 build/test run surfaced four real problems. All are
addressed in this patch.

### 1. `serde_json_path 0.6.7` fails to compile (11 errors)

Upstream bug: the crate's `0.6.x` series and its macro companion
pull in two different major versions of `serde_json_path_core`
(0.1.6 and 0.2.2) at the same time. `Function`, `ValueType`, and
`NodesType` end up doubly-defined; the macro expansion can't
type-check. Nothing we can do in our code fixes that — it's a
packaging bug in that crate version.

**Fix:** swap to `jsonpath-rust = "1.0"` workspace-wide. It is
RFC 9535 compliant, actively maintained, and has no split-version
issue. API is trait-based on `serde_json::Value`:

```rust
use jsonpath_rust::JsonPath;   // trait
let nodes: Vec<&Value> = value.query(path)?;
```

Files changed: `Cargo.toml`, `crates/pipeline/Cargo.toml`,
`crates/pipeline/src/recipe_apply.rs` (the `extract_json_path`
function and the one import line).

### 2. `rejects_ipv6_loopback` fails — real SSRF bypass

`UrlGuard::check("http://[::1]/")` was returning `Ok` instead of
`PrivateIp`. Root cause: `url::Url::host_str()` returns IPv6
literals **with brackets** (`"[::1]"`), and `IpAddr::from_str`
rejects bracketed strings. The check silently failed open.

Mild SSRF defense gap: a recipe with an IPv6-loopback URL would
pass the guard. The URL guard's only other line of defense
(`http.rs::check_host_ip`) had the exact same bug.

**Fix in both places:** use typed `url::Host::Ipv6(v6)` via
`url.host()` rather than parsing the stringified host. Removed the
now-unused `std::str::FromStr` imports on the way out.

Files changed: `crates/secure/src/url_guard.rs`,
`crates/secure/src/http.rs`.

### 3. `fs_guard::tests::accepts_plain_filename` fails

Not a real code bug — a parallel-test race. All four `fs_guard`
tests shared one temp dir keyed on PID. Rust runs tests in parallel
by default; one test's cleanup would `remove_dir_all` the shared
root while another test was mid-`canonicalize()`, producing the
observed `Io("No such file or directory")`. The other three tests
didn't show the race because they hit early-return paths that
don't canonicalize.

**Fix:** per-test unique temp dir (PID + test name suffix), so
cleanup from one test never affects another.

Files changed: `crates/secure/src/fs_guard.rs`.

### 4. `build_body_has_expected_shape_for_plain_completion` fails

Float roundtrip. `CompletionRequest.temperature` is `f32`;
`assert_eq!(body["temperature"], json!(0.1))` compared a
`Value::Number` derived from the f32 input against one derived from
the f64 `0.1` literal. `0.1_f32 → f64` widens to
`0.10000000149011612`, not `0.1`. The assertion was always
precision-sensitive and happened to stay hidden until now.

**Fix:** the test now uses `temperature: 0.5` — exactly
representable in both f32 and f64, so the roundtrip is byte-clean.
A comment in the test names the reason so nobody "simplifies" it
back to `0.1` later.

Files changed: `crates/llm/src/providers/grok.rs`.

### Also: two warnings cleared

The `#[cfg(any(test, feature = "testing"))]` attribute in `grok.rs`
and `usgs/mod.rs` referred to a `testing` feature that isn't
declared anywhere. Rust 1.80+'s `unexpected_cfgs` lint warned on
both. Dropped the speculative alt — now just `#[cfg(test)]`.

Files changed: `crates/llm/src/providers/grok.rs`,
`crates/sources/src/adapters/usgs/mod.rs`.

## What to run after applying

```bash
cargo check --workspace
cargo test -p stockpile-secure     # must now be 20/20 (was 18/20)
cargo test -p stockpile-sources    # should stay 5/5 + 1 ignored
cargo test -p stockpile-llm        # must now be 10/10 + 2 ignored (was 9/10)
cargo test -p stockpile-pipeline   # must compile + all recipe_apply tests pass
```

If anything here surprises you, share the output and we'll iterate
before moving to the 3c.4 demo patch.

## Files in this archive

```
SESSION3_FIXES_PATCH.md                       (this file)
Cargo.toml                                    (workspace)
crates/pipeline/Cargo.toml
crates/pipeline/src/recipe_apply.rs
crates/llm/src/providers/grok.rs
crates/sources/src/adapters/usgs/mod.rs
crates/secure/src/url_guard.rs
crates/secure/src/http.rs
crates/secure/src/fs_guard.rs
```
