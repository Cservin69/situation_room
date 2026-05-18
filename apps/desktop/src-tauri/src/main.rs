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
use situation_room_apps_common::sources::{
    load_source_descriptors, LiveSources, DEFAULT_SOURCES_POLL_INTERVAL,
};
use situation_room_pipeline::authoritative::{
    default_seed_entries, AuthorityEntry, AuthorityRegistry,
};
use situation_room_pipeline::authoritative_live::{
    LiveAuthorityRegistry, DEFAULT_POLL_INTERVAL,
};
use situation_room_storage::authority_registry::AuthorityProvenance;
use situation_room_llm::{
    AnthropicProvider, CostLedger, LlmProvider, MeteredProvider, XaiProvider,
};
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

/// Session 77 — per-Document Assertion extractor prompt. Same
/// loading pattern (include_str! at compile time, single source of
/// truth at the workspace `config/prompts/` location). Consumed by
/// `pipeline::extract::extract_and_persist_assertions` once per
/// fetched article-kind Document.
const DOCUMENT_ASSERTIONS_PROMPT: &str =
    include_str!("../../../../config/prompts/document_assertions.md");

/// Session 78 — per-Document Event extractor prompt. Sibling of
/// `DOCUMENT_ASSERTIONS_PROMPT`; same loading pattern. Consumed by
/// `pipeline::extract::extract_and_persist_events` once per
/// fetched article-kind Document, gated upstream on
/// plan-declared `event_kinds` (cost-bounded for plans that don't
/// track events).
const DOCUMENT_EVENTS_PROMPT: &str =
    include_str!("../../../../config/prompts/document_events.md");

/// Session 79 — per-Document Observation extractor prompt. Third
/// sibling of `DOCUMENT_ASSERTIONS_PROMPT` and
/// `DOCUMENT_EVENTS_PROMPT`; same loading pattern. Consumed by
/// `pipeline::extract::extract_and_persist_observations` once per
/// fetched article-kind Document, gated upstream on plan-declared
/// `observation_metrics` (cost-bounded for plans that don't track
/// numeric observations).
const DOCUMENT_OBSERVATIONS_PROMPT: &str =
    include_str!("../../../../config/prompts/document_observations.md");

/// Session 80 — per-Document EntityAttribute extractor prompt. Fourth
/// sibling of the three earlier extractor prompts; same loading
/// pattern. Consumed by
/// `pipeline::extract::extract_and_persist_entity_attributes` once per
/// fetched article-kind Document. v1 is open-vocab on `key` so plans
/// without a declared attribute list still produce extracted
/// attributes (the dashboard surfaces them as Assertion rows with
/// `AssertedContent::EntityAttribute`).
const DOCUMENT_ENTITY_ATTRIBUTES_PROMPT: &str =
    include_str!("../../../../config/prompts/document_entity_attributes.md");

/// Session 97 Lever A — per-Document Entity extractor prompt. Fifth
/// sibling of the four earlier extractor prompts; same loading
/// pattern. Consumed by
/// `pipeline::extract::extract_and_persist_entities` once per
/// fetched article-kind Document, gated upstream on plan-declared
/// `entity_kinds` (cost-bounded for plans that don't track actors).
/// Defense-in-depth alongside Sn-97 Lever B's recipe-driven Entity
/// production: the two paths converge on `Store::upsert_entity`.
const DOCUMENT_ENTITIES_PROMPT: &str =
    include_str!("../../../../config/prompts/document_entities.md");

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

    // Session 66: take a `Weak<Store>` reference for the signal-
    // shutdown task. The signal task only needs to call
    // `Store::checkpoint()` *when a signal fires*; in the more common
    // Cmd-Q exit path no signal ever arrives. A strong `Arc<Store>`
    // captured by the signal task's spawned thread would keep the
    // Store alive past the App's Drop on Cmd-Q exit, preventing the
    // DuckDB `Connection::drop` from running and regressing the
    // pre-Session-65 working path.
    //
    // `Weak<Store>::upgrade()` returns `Some(Arc<Store>)` only while
    // the strong-ref population is non-zero, which is exactly the
    // window where checkpoint is meaningful. On Cmd-Q exit the strong
    // refs drop, the upgrade returns `None`, the signal task's branch
    // becomes a no-op — and the OS killing the detached spawn-thread
    // at process exit is harmless because no Drop was riding on it.
    let store_for_shutdown: std::sync::Weak<Store> = Arc::downgrade(&store);

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

    // Session 50 (Class C): separate HTTP client for the propose-URL
    // retry loop's prefetch step, with a tighter `total_timeout` so a
    // slow host fails fast and leaves room for retries inside the
    // per-source authoring deadline (240s — see
    // `pipeline::fetch_executor::PER_SOURCE_DEADLINE_SECS`).
    //
    // **Why 60s.** The 2026-05-09 lithium MCS run (live-run obs doc,
    // class C) caught the failure mode on `industry.gov.au`: a
    // single 300s prefetch consumed the entire deadline, starving
    // the retry loop. 60s is large enough for a normal-shape PDF or
    // HTML response (most legitimate prefetches land in 5-25s), tight
    // enough that even 4× consecutive timeouts (240s) fit inside the
    // deadline if every attempt's host happens to be slow. The LLM
    // provider client (`http`) keeps the default 300s ceiling for
    // legitimately long completions; only the prefetch path tightens.
    //
    // The two clients share no state (each owns its own
    // `reqwest::Client`); the per-host backoff state lives in
    // `AppState::host_backoff` and decorates both via the
    // `BackoffFetcher` wrapper at the executor's call site (see
    // `crates/api/src/commands.rs::run_fetch_for_plan`). That keeps
    // observed throttling signals symmetric across LLM and prefetch
    // hosts — important because both clients hit the same host
    // population (the LLM call goes through the provider's API host;
    // the prefetch hits arbitrary publisher hosts).
    let prefetch_http_config = SecureHttpConfig {
        total_timeout: std::time::Duration::from_secs(60),
        ..SecureHttpConfig::default()
    };
    let prefetch_http = SecureHttpClient::new(prefetch_http_config)
        .context("building secure prefetch http client")?;
    let prefetch_http_arc = Arc::new(prefetch_http);

    let provider = pick_provider(http)?;

    // --- Cost ledger + metered-provider wrap (Session 75) -----------
    //
    // The ledger is a process-wide tally keyed by (provider_id, tier).
    // We hold the strong `Arc<CostLedger>` in `AppState` (so the
    // `llm_cost_ledger` Tauri command reads from it without going
    // through the provider) AND inside `MeteredProvider` (so every
    // LlmProvider::complete call records here).
    //
    // Wrapping at the trait boundary catches every call site for free —
    // classifier, recipe-author, propose-URL, re-author — without
    // having to thread an accounting hook through each one. See
    // `crates/llm/src/cost_ledger.rs` module docs for the wrap
    // rationale.
    let cost_ledger = Arc::new(CostLedger::new());
    let provider: Arc<dyn LlmProvider + Send + Sync> =
        Arc::new(MeteredProvider::new(provider, Arc::clone(&cost_ledger)));

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
    // Session 88 — wrap the boot-time descriptor list in a hot-reload
    // handle and spawn a polling watcher. Symmetric to the
    // `LiveAuthorityRegistry` watcher below: edits to
    // `config/sources.toml` propagate within ~2 seconds without
    // restarting the desktop binary. AppState stores the snapshot
    // taken at boot today; future call sites that want the live view
    // can plumb the `LiveSources` handle (left as a follow-on so the
    // existing classifier + executor paths stay byte-for-byte
    // unchanged — ADR 0015 already narrowed sources.toml's runtime
    // consumers to the executor's hand-crafted live tests).
    let live_sources = LiveSources::new(sources.clone(), sources_path.clone());
    live_sources.spawn_watcher(DEFAULT_SOURCES_POLL_INTERVAL);

    // --- Authoritative-source registry (Session 82) ----------------
    //
    // ADR 0004 pathway 1. Reads `config/vocab/authoritative_sources.toml`
    // — already in-tree since the commodity-vocab work. Missing-file or
    // parse-error is non-fatal: an empty registry preserves Session 81
    // consensus-only behaviour. The TOML schema is documented in
    // `crates/pipeline/src/authoritative.rs`.
    let authoritative_path = workspace_root
        .join("config")
        .join("vocab")
        .join("authoritative_sources.toml");
    let toml_registry = match AuthorityRegistry::load_from_path(&authoritative_path) {
        Ok(r) => {
            info!(
                count = r.entries().len(),
                path = %authoritative_path.display(),
                "authoritative-source registry loaded from TOML"
            );
            r
        }
        Err(e) => {
            // Non-fatal: log and run with an empty registry. The
            // promote stage skips its authoritative pre-pass on
            // empty; consensus still runs.
            tracing::warn!(
                path = %authoritative_path.display(),
                error = %e,
                "authoritative-source registry TOML load failed — falling back to DB-only seed"
            );
            AuthorityRegistry::empty()
        }
    };

    // Session 90 — ADR 0022 Stage 2 seed-on-empty boot path.
    //
    // Phase-3 extraction (Sessions 77/78/80) stamps every per-Document
    // Assertion with a single synthetic claimant kind (`agency:document`),
    // which made the N=3 consensus quorum mathematically unreachable on
    // every historical run. Seeding `agency:document` with
    // `consensus_quorum = Some(1)` opts that kind into the authoritative
    // fast-track: a single matching Assertion promotes immediately.
    //
    // Seeding is idempotent — `Store::seed_if_empty` short-circuits the
    // moment the table has any rows, so re-boots and operator-curated
    // additions are preserved. The TOML at `authoritative_path` stays
    // as a *bootstrap* artefact: useful for the empty-DB first boot,
    // ignored once the DB has authority over the registry.
    let seed_rows: Vec<_> = default_seed_entries()
        .iter()
        .map(|e| e.to_seed_row(AuthorityProvenance::TomlSeed))
        .collect();
    match store.seed_if_empty(&seed_rows) {
        Ok(0) => info!(
            "authority_registry already populated — default seed skipped"
        ),
        Ok(n) => info!(
            seeded = n,
            "authority_registry seeded from default closed-vocab entries"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "authority_registry seed_if_empty failed — promote auth pass may stay empty"
        ),
    }

    // Resolve the active registry. DB beats TOML when populated; this
    // matches `LiveAuthorityRegistry::reload`'s source-of-truth order
    // so the boot-time snapshot is consistent with subsequent reloads.
    let initial_registry = match store.authority_registry_entries() {
        Ok(rows) if !rows.is_empty() => {
            let count = rows.len();
            let entries: Vec<AuthorityEntry> =
                rows.into_iter().map(AuthorityEntry::from).collect();
            info!(
                count,
                "authoritative-source registry sourced from DB"
            );
            AuthorityRegistry::from_entries(entries)
        }
        Ok(_) => {
            // DB unexpectedly empty after seed_if_empty (would only
            // happen if seed_if_empty above failed). Use the TOML
            // registry we already loaded so the auth pass at least
            // sees whatever the operator has in `config/vocab/…`.
            toml_registry
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "authority_registry DB read failed — falling back to TOML-loaded registry"
            );
            toml_registry
        }
    };

    // Session 84 — wrap the boot-time registry in a hot-reload handle
    // and spawn a polling watcher. Edits to
    // `config/vocab/authoritative_sources.toml` propagate to the next
    // promote run within ~2 seconds without restarting the desktop
    // binary. ADR 0021 amendment: operators can tune
    // `consensus_quorum` interactively.
    //
    // Session 90 binds the `Store` handle so `reload()` (whether
    // triggered by the watcher or by a future operator command)
    // resolves DB-over-TOML the same way the boot path just did.
    let authoritative = LiveAuthorityRegistry::new(initial_registry, authoritative_path.clone())
        .with_store(Arc::clone(&store));
    authoritative.spawn_watcher(DEFAULT_POLL_INTERVAL);

    // --- AppState ----------------------------------------------------
    let state = AppState::new(
        store,
        provider,
        http_arc,
        prefetch_http_arc,
        cost_ledger,
        CLASSIFIER_PROMPT,
        RECIPE_AUTHOR_PROMPT,
        PROPOSE_URL_PROMPT,
        DOCUMENT_ASSERTIONS_PROMPT,
        DOCUMENT_EVENTS_PROMPT,
        DOCUMENT_OBSERVATIONS_PROMPT,
        DOCUMENT_ENTITY_ATTRIBUTES_PROMPT,
        DOCUMENT_ENTITIES_PROMPT,
        sources,
        authoritative,
    );

    // Session 86 — replay persisted promote-history rows into the
    // in-memory ring. Empty on first boot after migration 0017;
    // populated on subsequent boots from the previous session's
    // activity. Non-fatal on any storage failure (see
    // `AppState::hydrate_promote_history` for the per-row fallback
    // posture).
    state.hydrate_promote_history();

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
    let app = tauri::Builder::default()
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
            // Session 48 — operator-introspection surfaces over
            // network-layer + classifier-grounding state. Both are
            // pure reads; no LLM call, no fetch. `host_backoff_state`
            // surfaces what the per-host adaptation layer has observed
            // this binary session (Session 45's HostBackoff). The
            // accessor lives on AppState; the command lifts each
            // typed `HostBackoffSnapshot` into the wire DTO.
            // `sources_memory` surfaces the same rows the classifier
            // consumes under `{{SOURCES_MEMORY}}` — closes the
            // grounding-visibility gap noted across the 46/47/48
            // handoffs.
            situation_room_api::commands::host_backoff_state,
            situation_room_api::commands::sources_memory,
            // Session 22 added the records-rendering join (storage
            // query + DTO + #[tauri::command] function), but the
            // command wasn't added to this macro list — so Tauri
            // rejected every frontend call with "Command
            // records_for_plan not found". Session 23.1 amendment:
            // register it. The function lives in `commands_records`
            // (a sibling module to `commands`), so the path differs
            // from the rest of this list — `commands_records::`,
            // not `commands::`.
            situation_room_api::commands_records::records_for_plan,
            // Session 63 — cross-plan dashboard. Reads the same six
            // per-type tables `records_for_plan` reads, but without a
            // plan-id filter: returns the newest records of each
            // type across every plan. Drives the global
            // `RecordsDashboard` so the operator's view of "what has
            // been collected" doesn't reset every time a fresh
            // classification lands.
            situation_room_api::commands_records::records_recent_global,
            // Session 75 — LLM cost-by-tier ledger. Pure read over
            // `AppState::cost_ledger.snapshot()`; the metered-provider
            // wrap installed above accumulates every LLM completion's
            // (input, output, cached) token totals keyed by
            // (provider_id, tier). Drives the CostByTierPanel on the
            // dashboard so the operator can see the Session-74 v1.22
            // prompt-cache lever working without grepping INFO logs.
            situation_room_api::commands::llm_cost_ledger,
            // Session 81 — per-call cost timeline ring buffer. Sibling
            // to llm_cost_ledger: that command answers "how much have
            // we spent in this bucket", this one answers "what did the
            // last 50 LLM calls look like, in order". The frontend
            // renders a CostTimelinePanel so cost spikes are visible
            // without grepping the INFO log.
            situation_room_api::commands::llm_cost_timeline,
            // Session 77 — surfaces the classifier prompt version
            // currently loaded in the binary. Drives the per-plan
            // "re-classify under newer prompt" banner: the frontend
            // compares this against the @version suffix parsed off
            // `ResearchPlanDto.classified_by` and shows the banner
            // when they differ (or when the plan predates Session 77
            // and has no @version suffix at all).
            situation_room_api::commands::classifier_prompt_version,
            // Session 81 — consensus-promotion stage (ADR 0004 /
            // ADR 0021). Pure read+write: walks the plan's Assertion
            // rows, promotes any compatible-claim group with ≥ N
            // independent claimants. Idempotent on re-run via
            // content-derived dedup_keys. Operator-triggered today;
            // a future session may schedule this off the fetch-run
            // completion hook.
            situation_room_api::commands_records::promote_consensus_for_plan,
            // Session 84 — dashboard tile surfaces over the live
            // authoritative registry + the most recent PromoteReport.
            // Both are pure reads off AppState; no DB, no LLM. The
            // first reads the hot-reload snapshot so an operator's
            // edit to the TOML appears in the dashboard within ~2s.
            // The second reads the in-memory `last_promote_summary`
            // Mutex written by the auto-trigger and the manual
            // promote command.
            situation_room_api::commands::authoritative_registry_summary,
            situation_room_api::commands::last_promote_summary,
            // Session 85 — promote-pass history ring buffer + rolling
            // auto-trigger counter. Sibling to `last_promote_summary`;
            // surfaces the last N (~20) PromoteReports plus the
            // cross-plan trigger counter so the dashboard tile can show
            // "X runs in the last minute" when an operator fires several
            // fetches in rapid succession.
            situation_room_api::commands::promote_history,
            // Session 88 — cross-table id → type batch lookup + single-id
            // record fetch. Powers the PromoteDetailDrawer's per-pass
            // promoted-record-ids strip (chip colour-coded by record type)
            // and any future click-through that opens a record from an
            // opaque id alone (record_types_for_ids resolves the kind,
            // get_record_by_id fetches the full envelope).
            situation_room_api::commands_records::record_types_for_ids,
            situation_room_api::commands_records::get_record_by_id,
            // Session 92 — operator-triggered re-extraction of relation
            // Assertions under prompt v1.2 (ADR 0023). Iterates the
            // plan's article-kind Documents already on disk; one
            // workhorse-tier LLM call per Document. Cost-bounded by
            // article-kind Document count per plan. The auto-trigger
            // pattern was deliberately avoided — operator clicks
            // per-plan, sees per-plan summary, decides whether to
            // re-extract another plan.
            situation_room_api::commands_records::reextract_relations_for_plan,
            // Session 93 — narrower of the above, scoped to one
            // Document by id. Wired so the DocumentDrawer header's
            // re-extract button can target the open Document
            // without paying for the full plan pass.
            situation_room_api::commands_records::reextract_relations_for_document,
            // Session 93 — operator-triggered cull of boilerplate-
            // shaped Assertions whose source Document scores Index
            // under the apply-time detector. Read-only preview +
            // destructive cull pair.
            situation_room_api::commands_records::sample_index_assertions_for_plan,
            situation_room_api::commands_records::cull_index_assertions_for_plan,
        ])
        .build(tauri::generate_context!())
        .context("building tauri")?;

    // --- Signal-driven shutdown (Session 66) -----------------------
    //
    // Session 65 traced a "writes-vanish-between-desktop-sessions" bug
    // to the fact that `run_desktop.sh`'s SIGTERM trap, when the
    // operator Ctrl-C's the terminal hosting `tauri dev`, instant-kills
    // the Rust binary. Drop never runs. DuckDB's in-memory buffer pool
    // never checkpoints. Today's plans, recipes, and fetch-attempts
    // vanish; only writes that exited via the Cmd-Q Tauri path (which
    // does run `Drop`) survive.
    //
    // The fix has two halves. Half one: install signal handlers that
    // turn SIGTERM/SIGINT into a clean Tauri exit via
    // `AppHandle::exit(0)`, which runs Tauri's own cleanup, returns
    // from `app.run`, drops `App`, drops the managed `AppState`, drops
    // `Store`, and DuckDB's `Connection::drop` checkpoints the buffer
    // pool to disk along the way. Half two: belt-and-braces — call
    // `Store::checkpoint()` explicitly inside the signal handler
    // *before* `AppHandle::exit`, so the data is durable even if the
    // Drop chain is short-circuited downstream for any reason.
    //
    // The Cmd-Q path (which works today) never reaches this signal
    // task; AppKit close → Tauri exit event → `app.run` returns,
    // bypassing the signal handler entirely. So this code only changes
    // behaviour on the SIGTERM/SIGINT path. Existing operator habits
    // continue to work.
    //
    // `#[cfg(unix)]` because `tokio::signal::unix::signal` is Unix-only.
    // On Windows we'd register a Ctrl-C handler via
    // `tokio::signal::ctrl_c()`; not relevant for the current Mac-only
    // desktop deploy target but easy to add later.
    #[cfg(unix)]
    {
        let app_handle = app.handle().clone();
        let weak_store = store_for_shutdown.clone();
        // Dedicated current-thread tokio runtime on a std thread —
        // decoupled from whatever async runtime Tauri uses internally,
        // so we don't have to reason about feature unification or
        // Tauri 2's choice of executor. The thread parks blocked on
        // signal recv; the runtime exits the moment `shutdown_on_signal`
        // returns (which happens on the first signal or on
        // `app_handle.exit(0)`'s effects rippling through). Total cost:
        // one parked OS thread + one tokio runtime, both negligible.
        //
        // Captures a `Weak<Store>` (not a strong `Arc`) so that on the
        // Cmd-Q exit path — where no signal fires — the Store can drop
        // normally when AppState drops, allowing DuckDB's
        // `Connection::drop` to checkpoint. The detached thread is
        // killed at process exit; the weak ref leaking is a no-op.
        std::thread::Builder::new()
            .name("sr-signal-shutdown".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "failed to build signal-handler tokio runtime — persistence at risk on Ctrl-C"
                        );
                        return;
                    }
                };
                rt.block_on(shutdown_on_signal(weak_store, app_handle));
            })
            .ok();
    }

    // The Tauri event-loop callback. No-op — Tauri's own cleanup runs
    // on App drop and is sufficient for the Cmd-Q exit path. Logging
    // here would fire on every exit including the working Cmd-Q path
    // and just add noise.
    app.run(|_app_handle, _event| {});

    // Explicitly drop the weak ref so its lifetime is clear in this
    // function body. The signal-shutdown thread (if it exists) holds
    // its own `Weak<Store>` clone; this one drops at the end of main
    // either way.
    drop(store_for_shutdown);

    Ok(())
}

// ---------------------------------------------------------------------------
// Signal-driven shutdown (Session 66)
// ---------------------------------------------------------------------------

/// Wait for SIGTERM or SIGINT, then issue a DuckDB CHECKPOINT (if the
/// Store is still alive) and ask Tauri to exit cleanly. Runs on a
/// dedicated std thread with its own current-thread tokio runtime;
/// spawned once at boot and lives until the first signal or until
/// process exit.
///
/// **Why `Store::checkpoint()` here and not just trust Drop:** the
/// Session 65 diagnosis was that SIGTERM with no handler instant-kills
/// the binary, so `Drop` never runs and DuckDB's buffer pool never
/// hits disk. Calling `checkpoint()` explicitly in the handler — *not*
/// relying on `AppHandle::exit(0)` to fire Drop in time — is the
/// belt-and-braces. If `exit(0)`'s tear-down ever races with the
/// async signal-task drop ordering, we've already flushed.
///
/// **Why `Weak<Store>`:** if this task held a strong `Arc<Store>`, the
/// Store could never drop until the task did, and the task only
/// returns on signal. On the Cmd-Q exit path no signal fires; a
/// strong ref would prevent the Store from dropping when AppState
/// drops, regressing the pre-Session-65 working path. With `Weak`,
/// upgrade fails cleanly when the strong refs have already gone away
/// and the signal-handler branch becomes a no-op — exactly the
/// semantics the Cmd-Q path needs.
///
/// **Why both signals:** `run_desktop.sh`'s `trap` block fires SIGTERM
/// on Ctrl-C in the terminal; the user's shell can also raise SIGINT
/// directly if they bypass the script. Either should be durable.
#[cfg(unix)]
async fn shutdown_on_signal(
    store: std::sync::Weak<Store>,
    app_handle: tauri::AppHandle,
) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to install SIGTERM handler — persistence at risk on Ctrl-C");
            return;
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to install SIGINT handler — persistence at risk on Ctrl-C");
            return;
        }
    };

    let signal_name = tokio::select! {
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv()  => "SIGINT",
    };

    tracing::info!(
        signal = signal_name,
        "received shutdown signal — checkpointing DuckDB then asking Tauri to exit"
    );

    match store.upgrade() {
        Some(store) => {
            if let Err(e) = store.checkpoint() {
                // Don't return — still try the Tauri exit so the App's
                // own Drop chain gets a shot. The checkpoint error is
                // the more informative log line; this is the
                // failure-mode the operator needs to see in the
                // terminal stderr if it ever fires.
                tracing::error!(
                    error = %e,
                    "DuckDB checkpoint failed during shutdown — relying on Drop chain only"
                );
            }
        }
        None => {
            // The Store has already been dropped (Cmd-Q path beat the
            // signal here, vanishingly unlikely on the SIGTERM path but
            // logged for completeness). Nothing to checkpoint; Drop
            // already ran.
            tracing::info!(
                "Store already dropped at signal arrival — checkpoint already \
                 happened via Drop; calling AppHandle::exit anyway"
            );
        }
    }

    app_handle.exit(0);
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
