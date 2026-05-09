//! situation_room desktop binary — Tauri 2 composition root.
//!
//! Boots the scrubbed tracing logger, opens the DuckDB store, builds
//! the LLM provider on top of `SecureHttpClient`, loads the source
//! descriptors from `config/sources.toml`, registers the three
//! commands defined in `situation_room-api`, and starts the webview.
//!
//! Per ADR 0001 this is the only binary `main.rs` for the desktop app:
//! all wiring happens here, in one file. The library crates own the
//! types, the pipeline, the security primitives; this file owns the
//! act of plugging them together.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use situation_room_api::commands::AppState;
use situation_room_apps_common::sources::load_source_descriptors;
use situation_room_llm::{AnthropicProvider, LlmProvider, XaiProvider};
use situation_room_secure::{
    http::{SecureHttpClient, SecureHttpConfig},
    logging,
};
use situation_room_storage::Store;
use tracing::info;

/// The production classifier prompt, embedded at compile time. The CLI
/// embeds the same file via `include_str!` from the same path; keeping
/// both binaries on a single source-of-truth means a prompt revision
/// affects both surfaces in lockstep.
const CLASSIFIER_PROMPT: &str =
    include_str!("../../../../config/prompts/research_classifier.md");

/// The production recipe-author prompt. Embedded the same way as the
/// classifier prompt — single source of truth at the workspace
/// `config/prompts/` location, included at compile time so the
/// binary doesn't need filesystem discovery.
///
/// Used by the fetch executor's Level-2 authoring step (Session 8).
/// ADR 0007: this is the *only* LLM call in the runtime path, and
/// it runs once per (plan, source) pair — never on refresh.
const RECIPE_AUTHOR_PROMPT: &str =
    include_str!("../../../../config/prompts/recipe_author.md");

/// The production propose-URL prompt (Session 39). Cheap-tier LLM
/// call inside the fetch executor's per-attempt retry loop. Picks
/// the URL each attempt fetches given the description and the
/// prior-attempts history.
const PROPOSE_URL_PROMPT: &str =
    include_str!("../../../../config/prompts/propose_source_url.md");

fn main() -> Result<()> {
    // .env is a dev convenience; the real environment always wins.
    // Walks up from CWD to find .env at the workspace root and
    // returns that directory so we can anchor other relative paths
    // (db, sources) to the same place. Falls back to CWD if no .env
    // is found.
    let workspace_root = load_dotenv().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });

    // Scrubbed tracing — every log line passes through the secret-
    // scrubbing writer before it reaches stdout or disk.
    logging::init();

    info!(
        workspace_root = %workspace_root.display(),
        "situation_room desktop boots."
    );

    // --- Storage -----------------------------------------------------
    //
    // `situation_room.duckdb` lives at the workspace root — the same
    // location the CLI uses by default. Anchoring on workspace_root
    // (not CWD) means `tauri dev` finds the same database whether
    // the binary is launched from the repo root or from inside
    // `apps/desktop/src-tauri/` (Tauri sets CWD there).
    let db_path = workspace_root.join("situation_room.duckdb");
    let store = Store::open(&db_path)
        .with_context(|| format!("opening store at {}", db_path.display()))?;
    store.migrate().context("running migrations")?;
    let store = Arc::new(store);

    // --- LLM provider ------------------------------------------------
    //
    // Single SecureHttpClient instance shared by the provider AND by
    // the fetch executor's source-fetch path — no fresh
    // `reqwest::Client::new()` anywhere. ADR 0009 §"The rule".
    //
    // We hold the client in an Arc and hand the provider a Clone
    // (SecureHttpClient is internally Arc-wrapped, so Clone shares
    // the underlying connection pool — what we want).
    //
    // `pick_provider` reads `LLM_PROVIDER` (default `"xai"`) and
    // builds the matching concrete provider, type-erased to
    // `Arc<dyn LlmProvider + Send + Sync>` so AppState can hold one
    // shape regardless of which provider is configured. Session 23
    // promoted Anthropic from stub to real; both providers are now
    // viable picks.
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("building secure http client")?;
    let http_arc = Arc::new(http.clone());
    let provider = pick_provider(http)?;

    // --- Source descriptors -----------------------------------------
    //
    // Doc-narrowed under ADR 0015 (Session 37). The classifier no
    // longer reads this file — it consults the in-DB sources memory
    // (see `Store::sources_memory`). The load survives because the
    // executor's `#[ignore]` live tests still author hand-crafted
    // recipes against the surviving `csv_demo` and `json_demo`
    // entries. A missing file is non-fatal; production classification
    // proceeds either way.
    let sources_path = workspace_root.join("config").join("sources.toml");
    let sources = load_source_descriptors(&sources_path, 30)
        .with_context(|| format!("loading sources from {}", sources_path.display()))?;
    info!(
        count = sources.len(),
        "source descriptors loaded (post-ADR-0015 demo fixtures only)"
    );

    // --- AppState ----------------------------------------------------
    let state = AppState::new(
        store,
        provider,
        http_arc,
        CLASSIFIER_PROMPT,
        RECIPE_AUTHOR_PROMPT,
        PROPOSE_URL_PROMPT,
        sources,
    );

    // --- Tauri -------------------------------------------------------
    //
    // The capabilities file (`capabilities/default.json`) explicitly
    // enumerates which IPC commands the webview is allowed to call.
    // Adding a fourth or fifth command means editing this
    // `invoke_handler` call; capabilities here gate built-in plugin
    // permissions only, not custom #[tauri::command] handlers.
    // ADR 0009 §"Tauri posture".
    //
    // The full paths are required, not just the imported function
    // names: `#[tauri::command]` generates a sibling `__cmd__<name>`
    // macro in the same module as the function, and
    // `tauri::generate_handler!` re-prefixes the path you give it. So
    // `generate_handler![classify]` looks for `__cmd__classify` in
    // *this* file's scope, where it does not exist; the macro lives
    // in `situation_room_api::commands`. Bare imports work for the
    // function, not for the macro.
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            situation_room_api::commands::classify,
            situation_room_api::commands::list_recent_plans,
            situation_room_api::commands::get_plan,
            situation_room_api::commands::accept_plan,
            situation_room_api::commands::reject_plan,
            situation_room_api::commands::reclassify_plan,
            situation_room_api::commands::run_fetch_for_plan,
            situation_room_api::commands::list_fetch_runs,
            situation_room_api::commands::list_recipes_for_plan,
            situation_room_api::commands::set_recipe_feedback,
            situation_room_api::commands::list_recipe_feedback_for_plan,
            // Track A (Session 25/26 — manual re-author UI). Loads the
            // prior recipe + its captured failed-apply bytes, calls
            // the LLM to author a corrected recipe, persists the new
            // recipe with `prior_recipe_id` and `reauthor_reason`
            // populated. The Session 23.1 IPC-registration guard
            // would catch the omission, but the registration belongs
            // in the same commit as the command per the hard rule.
            situation_room_api::commands::latest_attempt_for_recipe,
            situation_room_api::commands::reauthor_recipe,
            // Session 46 — operator-introspection surfaces over data
            // the runtime already produces. The heatmap reads
            // `fetch_run_outcomes` (migration 0016, populated by the
            // executor at run completion); coverage reads
            // `recipes.produces_json` joined against the plan's
            // expectations. Both are pure reads; no LLM call.
            situation_room_api::commands::recipe_outcomes_history,
            situation_room_api::commands::expectation_coverage,
            // Session 22 added the records-rendering join (storage
            // query + DTO + #[tauri::command] function), but the
            // command wasn't added to this macro list — so Tauri
            // rejected every frontend call with "Command
            // records_for_plan not found". Session 23.1 amendment:
            // register it. The function lives in `commands_records`
            // (a sibling module to `commands`), so the path differs
            // from the rest of this list — `commands_records::`,
            // not `commands::`.
            situation_room_api::commands_records::records_for_plan
        ])
        .run(tauri::generate_context!())
        .context("running tauri")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// LLM provider selection
// ---------------------------------------------------------------------------

/// Environment variable that picks which LLM provider the binary uses
/// at boot. Default is `"xai"`; set to `"anthropic"` to switch to
/// Claude. Documented in `.env.example`.
const LLM_PROVIDER_ENV: &str = "LLM_PROVIDER";

/// Build the LLM provider chosen at boot. Reads `LLM_PROVIDER` (default
/// `"xai"`), constructs the matching concrete provider, and type-erases
/// it to `Arc<dyn LlmProvider + Send + Sync>` so AppState carries one
/// shape regardless of which provider is configured.
///
/// Returns a clear error if the chosen provider's API key isn't set —
/// rather than silently falling back to the other provider, which
/// would surprise an operator who explicitly asked for one.
fn pick_provider(
    http: SecureHttpClient,
) -> Result<Arc<dyn LlmProvider + Send + Sync>> {
    let choice = std::env::var(LLM_PROVIDER_ENV)
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xai".to_string());

    info!(provider = %choice, "selecting LLM provider");

    match choice.as_str() {
        "xai" | "grok" => {
            let p = XaiProvider::from_env(http).context(
                "XAI_API_KEY not found — set it in the environment or in a .env file at the workspace root",
            )?;
            Ok(Arc::new(p))
        }
        "anthropic" | "claude" => {
            let p = AnthropicProvider::from_env(http).context(
                "ANTHROPIC_API_KEY not found — set it in the environment or in a .env file at the workspace root",
            )?;
            Ok(Arc::new(p))
        }
        other => {
            // The operator typed something unrecognised. Don't fall
            // through to a default — surface the typo so the next
            // boot uses the provider they actually meant.
            anyhow::bail!(
                "unknown LLM_PROVIDER {other:?}; valid values are 'xai' or 'anthropic'"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// .env loader (minimal, no transitive dep)
// ---------------------------------------------------------------------------

/// Minimal `.env` loader. Walks up from the current working
/// directory looking for a `.env` file and reads `KEY=VALUE` lines
/// from the first one found. Does not overwrite existing env vars
/// (the real environment wins).
///
/// Why the walk: `tauri dev` sets CWD to `apps/desktop/src-tauri/`
/// when it runs the Rust binary, but the user's `.env` lives at the
/// workspace root. A naive `Path::new(".env")` looks in the wrong
/// place and the boot fails with "XAI_API_KEY not found" even when
/// the file is right there.
///
/// We stop walking at the filesystem root or after 8 hops, whichever
/// comes first. Eight is comfortably more than any sane monorepo
/// depth and bounds the work.
/// Minimal `.env` loader. Walks up from the current working
/// directory looking for a `.env` file, reads `KEY=VALUE` lines from
/// the first one found, and returns the directory it was found in.
/// That directory is treated as the workspace root by the caller, so
/// other relative paths (the DuckDB file, `config/sources.toml`) can
/// be anchored consistently regardless of CWD.
///
/// Returns `None` if no `.env` is found within the search budget — in
/// which case `main` falls back to CWD as the workspace root and
/// expects `XAI_API_KEY` to come from the real environment (or the
/// Tauri build to ship a `.app` bundle whose user supplies the key
/// via launchd plist or shell rc).
///
/// Why the walk: `tauri dev` sets CWD to `apps/desktop/src-tauri/`
/// when it runs the Rust binary, but the user's `.env` lives at the
/// workspace root. A naive `Path::new(".env")` looks in the wrong
/// place and the boot fails with "XAI_API_KEY not found" even when
/// the file is right there.
///
/// We stop walking at the filesystem root or after 8 hops, whichever
/// comes first. Eight is comfortably more than any sane monorepo
/// depth and bounds the work.
///
/// Existing env vars take precedence over `.env` contents.
fn load_dotenv() -> Option<PathBuf> {
    let start = std::env::current_dir().ok()?;

    let mut current: Option<&Path> = Some(start.as_path());
    let mut hops = 0u8;
    while let Some(dir) = current {
        if hops > 8 {
            break;
        }
        let candidate = dir.join(".env");
        if candidate.is_file() {
            apply_dotenv(&candidate);
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
        hops += 1;
    }
    None
}

/// Read `KEY=VALUE` pairs from `path` and set them in the
/// environment, but only for keys not already present. Silently
/// ignores read errors — `.env` is a convenience, not a contract.
fn apply_dotenv(path: &Path) {
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
                // SAFETY note: env::set_var becomes unsafe in edition
                // 2024. We're on 2021 where it's safe. When migrating
                // to 2024, wrap this in `unsafe { … }` with a SAFETY
                // comment noting it runs in main before threads spawn.
                std::env::set_var(k, v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Source-descriptor loader
// ---------------------------------------------------------------------------
//
// Session 24: this used to be a local copy of the loader, word-for-
// word identical to the CLI's. Both copies now call into
// `situation_room_apps_common::sources::load_source_descriptors`. See
// `crates/apps_common/src/lib.rs` for the contract on what does and
// does not belong in that crate.
