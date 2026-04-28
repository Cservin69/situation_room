# tauri.conf.json — notes

JSON has no comments and Tauri 2's config schema rejects `"//"` filler
keys outside `security.csp`. The explanations that previously rode
along as `"//"` entries live here instead. Authoritative policy is in
ADR 0009 (`docs/adr/0009-security-posture.md`); this file is just the
inline-comment substitute.

## CSP

- `default-src 'self'` — only bundled assets load by default.
- `connect-src 'self' ipc: http://ipc.localhost` — only Tauri IPC for
  backend calls. External data fetch happens in Rust, not JS.
- `img-src 'self' data:` — `data:` is for the embedded SVG/PNG used
  in the design system. The `asset:` protocol is intentionally not
  enabled (`assetProtocol.enable` is omitted from `security`); if
  Phase-6 ever needs to surface archived snapshots into the webview,
  re-enable it AND add the `protocol-asset` feature to the `tauri`
  cargo dep, otherwise `cargo build` fails with a feature-mismatch
  error.
- `style-src 'self' 'unsafe-inline'` — required by Svelte's scoped
  styles; the compiled bundle contains no remote styles.
- `script-src 'self'` — no inline JS, no remote scripts.
- `object-src 'none'`, `frame-src 'none'`, `form-action 'none'` —
  defense in depth against legacy embed vectors.

## Other knobs

- `freezePrototype: true` — blocks prototype-pollution attacks.

## Capabilities

Capabilities live in `capabilities/default.json`. Custom
`#[tauri::command]` handlers are gated by the `invoke_handler!()`
registration in `src/main.rs`; entries in `capabilities/` gate
built-in plugin permissions only. Adding a fourth IPC command means
editing both `invoke_handler!()` and the JS-side wrapper in
`apps/desktop/src/lib/api/client.ts`.
