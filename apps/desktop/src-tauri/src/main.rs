//! Stockpile desktop binary — composition root.
//!
//! Phase 1: boots the scrubbed tracing logger, prints the boot banner,
//! verifies API key loading (without logging keys), exits.
//!
//! Phase 4 wires up Tauri proper, registers commands from `stockpile-api`,
//! starts the source scheduler, and opens the webview.

use anyhow::Result;
use stockpile_secure::{logging, secrets::ApiKey};
use tracing::{info, warn};

fn main() -> Result<()> {
    // Load .env if present — never required, but convenient for dev.
    // Uses a minimal inline loader so we don't depend on `dotenv` (one less
    // transitive dep, one less thing to review for supply chain).
    load_dotenv();

    // Initialize the scrubbed tracing subscriber. All subsequent logs pass
    // through the secret-scrubbing writer.
    logging::init();

    info!("Stockpile boots (Phase 1).");
    info!("security posture: scrubbed logging active, api keys loaded via env only");

    // Verify API key ingress — but never log the key, only the fingerprint.
    verify_llm_keys();

    info!("");
    info!("Phase 1 complete: workspace structure verified.");
    info!("Next steps:");
    info!("  - Phase 2: schema + storage layer");
    info!("  - Phase 3: source adapters + llm extraction");
    info!("  - Phase 4: Tauri webview + Svelte frontend");

    Ok(())
}

/// Minimal `.env` loader. Reads KEY=VALUE lines from `./.env` if present.
/// Does not overwrite existing env vars (the real environment wins).
fn load_dotenv() {
    let path = std::path::Path::new(".env");
    if !path.exists() {
        return;
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if std::env::var_os(k).is_none() {
                // NOTE: env::set_var will become unsafe in edition 2024.
                // We're on edition 2021 where it's safe. When we migrate to
                // edition 2024, wrap this in an `unsafe` block with a SAFETY
                // comment explaining that this runs in main before threads spawn.
                std::env::set_var(k, v);
            }
        }
    }
}

fn verify_llm_keys() {
    let providers = [
        ("ANTHROPIC_API_KEY", "Anthropic (Claude)"),
        ("XAI_API_KEY", "xAI (Grok)"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("GOOGLE_API_KEY", "Google (Gemini)"),
    ];

    let mut found_any = false;
    for (env_var, label) in providers {
        match ApiKey::from_env(env_var) {
            Ok(key) => {
                info!(provider = label, fingerprint = %key.fingerprint(), "llm provider key loaded");
                found_any = true;
            }
            Err(e) => {
                // Downgrade the known-bad-placeholder case to info; real missing keys
                // stay as warn so users see them during setup.
                match e {
                    stockpile_secure::secrets::ApiKeyError::NotSet(_) => {
                        tracing::debug!(provider = label, "no key configured (optional)");
                    }
                    _ => warn!(provider = label, error = %e, "llm provider key rejected"),
                }
            }
        }
    }

    if !found_any {
        warn!("no LLM provider keys configured. copy .env.example to .env and add at least one key.");
    }
}
