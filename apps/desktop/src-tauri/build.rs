//! Build script for the Tauri 2 desktop binary.
//!
//! `tauri-build::build()` reads `tauri.conf.json` and the
//! `capabilities/` directory at compile time, generating the
//! permission boilerplate that `tauri::generate_handler!` and
//! `tauri::generate_context!` consume at runtime.
//!
//! Required by Tauri 2 — running the binary without this build script
//! produces a `generate_context!` panic at startup.

fn main() {
    tauri_build::build()
}
