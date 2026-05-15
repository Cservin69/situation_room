//! situation_room eval harness — runs N independent trials of
//! `classify → accept → run_fetch_for_plan` for one topic and emits
//! JSONL metrics so prompt and runtime experiments can be evaluated
//! against baseline variance instead of single-trial anecdote.
//!
//! ## Why this exists
//!
//! Session 56 ran 5 lithium trials and observed records 0/1/1/2/3.
//! Per-trial variance was larger than the prompt-version effect we
//! were trying to measure. Every subsequent prompt or runtime patch
//! lands in the same fog unless the operator has a way to compare a
//! distribution-of-N against a baseline distribution-of-N rather than
//! a single observation against another single observation.
//!
//! This binary is that way. It is a non-Tauri composition root that
//! mirrors the desktop wiring (`apps/desktop/src-tauri/src/main.rs`)
//! and invokes the same `pipeline::fetch_executor::run_fetch_for_plan`
//! the desktop UI's `run_fetch_for_plan` Tauri command invokes — by
//! the same path, with the same `ExecutorContext`, the same provider,
//! the same prefetch HTTP client.
//!
//! ## What it doesn't do
//!
//! - It does not bake any topic-specific knowledge in. Anything topic-
//!   specific (which sources to expect, what records "should" land)
//!   lives in the JSONL the harness writes; the analysis is the
//!   operator's job.
//! - It does not tune the runtime. It only measures.
//!
//! ## Output shape
//!
//! One JSONL line per trial, written to `<out>` (default
//! `eval-runs/<topic-slug>-<timestamp>.jsonl`). Each line is a
//! `TrialReport` (see below). A summary line is also written to
//! stderr at the end of the run; the JSONL itself stays one-line-
//! per-trial so downstream tools (jq, pandas, etc.) can consume it
//! as a stream.
//!
//! ## Isolation
//!
//! By default each trial uses its own DuckDB file under
//! `/tmp/situation_room-eval-<run-id>/trial-N.duckdb` so plan-cache
//! and sources-memory state from earlier trials cannot contaminate
//! later trials' classification or authoring. The DBs are deleted at
//! the end of the harness run unless `--keep-dbs` is passed.
//!
//! Each trial also gets its own fresh `HostBackoff` (a per-host
//! adaptation layer that throttles after observed 429s / timeouts) —
//! sharing it across trials would mean an early 429 contaminates
//! later trials' wall-clock measurements.
//!
//! What is shared across trials: the LLM provider client, the two
//! HTTP clients (the LLM-tier 300s-timeout one and the
//! prefetch-tier 60s-timeout one), the embedded prompts. These
//! match what would be shared across calls in a real desktop session
//! and are not the variance source.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

use situation_room_apps_common::sources::load_source_descriptors;
use situation_room_llm::{AnthropicProvider, LlmProvider, ModelTier, XaiProvider};
use situation_room_pipeline::fetch_backoff::{BackoffFetcher, HostBackoff};
use situation_room_pipeline::fetch_executor::{
    run_fetch_for_plan as run_fetch_for_plan_impl, ExecutorContext, FetchExecutorError,
    FetchReport, RecipeOutcome,
};
use situation_room_pipeline::research::DocumentSourceEntry;
use situation_room_pipeline::research_classifier::{
    classify_topic, ClassificationContext, SourceDescriptor, TopicUsage as ClassifierTopicUsage,
};
use situation_room_pipeline::research_plans_store::save_research_plan;
use situation_room_secure::{
    http::{SecureHttpClient, SecureHttpConfig},
    logging,
};
use situation_room_storage::{research_plans::PlanStatus, Store};

// ---------------------------------------------------------------------------
// Embedded prompts — same files the desktop and CLI binaries embed,
// included here at compile time so the harness binary is self-
// contained.
// ---------------------------------------------------------------------------

const CLASSIFIER_PROMPT: &str =
    include_str!("../../../config/prompts/research_classifier.md");
const RECIPE_AUTHOR_PROMPT: &str =
    include_str!("../../../config/prompts/recipe_author.md");
const PROPOSE_URL_PROMPT: &str =
    include_str!("../../../config/prompts/propose_source_url.md");

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "eval-harness",
    version,
    about = "Run N (classify → accept → run_fetch) trials and emit JSONL metrics."
)]
struct Cli {
    /// The topic to classify on every trial. Verbatim — no
    /// normalization beyond `.trim()` so the operator can
    /// reproduce a specific topic string.
    #[arg(long)]
    topic: String,

    /// How many independent trials to run. Each trial gets its own
    /// DuckDB file and its own `HostBackoff` so cross-trial state
    /// can't contaminate the variance measurement.
    #[arg(long, default_value_t = 5)]
    trials: u32,

    /// Where to write the JSONL output. One line per trial.
    /// Defaults to `eval-runs/<topic-slug>-<timestamp>.jsonl` in the
    /// CWD; the directory is created if absent.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Where to put the per-trial DuckDB files. Defaults to
    /// `/tmp/situation_room-eval-<run-id>/`. The directory is
    /// created at startup and removed at clean exit unless
    /// `--keep-dbs` is set.
    #[arg(long)]
    db_dir: Option<PathBuf>,

    /// Keep the per-trial DuckDB files after the harness exits.
    /// Useful when investigating an outlier trial post-hoc — the
    /// JSONL line carries the per-trial DB path and `--keep-dbs`
    /// makes that path live until the operator deletes it.
    #[arg(long, default_value_t = false)]
    keep_dbs: bool,

    /// Path to the source descriptors TOML. Doc-narrowed under
    /// ADR 0015 (Session 37) and Session 39 — production authoring
    /// no longer consults this file. Loaded only because the
    /// `ExecutorContext` carries it. Defaults to `config/sources.toml`
    /// (which holds the two demo fixtures); a missing file is
    /// non-fatal.
    #[arg(long, default_value = "config/sources.toml")]
    sources: PathBuf,

    /// How long, in seconds, to allow a single trial's classify-
    /// to-fetch-complete cycle to run before reporting the trial as
    /// `timed_out` and continuing to the next one. Per-trial
    /// guard against a single hung run consuming the whole harness
    /// budget. Defaults to 1800s (30 min) — generous enough that
    /// healthy runs finish well inside it.
    #[arg(long, default_value_t = 1800)]
    trial_timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// JSONL row shape
// ---------------------------------------------------------------------------

/// One JSONL line. Stable schema — every key is always present
/// (using `null` / empty for missing values) so downstream tools can
/// rely on a uniform shape across trials and across harness runs.
#[derive(Debug, Serialize)]
struct TrialReport {
    /// 0-indexed trial number within this harness run.
    trial: u32,
    /// The topic string passed on the CLI, verbatim.
    topic: String,
    /// Provider id at the time of the run (`"xai"`, `"anthropic"`).
    /// Threaded into the JSONL so a future analysis pass can split
    /// by provider without spelunking the LLM_PROVIDER env var.
    provider: String,
    /// Wall-clock start of the trial.
    started_at: DateTime<Utc>,
    /// Wall-clock end of the trial.
    finished_at: DateTime<Utc>,
    /// `finished_at - started_at` in seconds.
    wall_clock_s: f64,
    /// Path to the per-trial DuckDB file. Populated even on error so
    /// the operator can poke at the partial state of a failing trial.
    db_path: PathBuf,
    /// Plan id, if classification reached the persistence step.
    plan_id: Option<Uuid>,
    /// Run id, if `run_fetch_for_plan` opened a fetch_run row.
    run_id: Option<Uuid>,
    /// Number of nominations the classifier produced (`document_sources`
    /// length). Useful for normalising records-per-nomination across
    /// trials whose classifier output happened to differ.
    nominations_total: u32,
    /// `recipes_attempted` from the FetchReport — recipes the
    /// executor tried to apply (decline outcomes are NOT counted here
    /// per the FetchReport contract).
    recipes_attempted: u32,
    /// `recipes_succeeded` from the FetchReport.
    recipes_succeeded: u32,
    /// `records_produced` — the headline number a prompt experiment
    /// is usually trying to move.
    records_produced: u32,
    /// Total recipes persisted for this trial's plan, regardless of
    /// outcome at apply time. Equivalent to `store.recipes_for_plan
    /// (plan_id).len()`. Includes recipes that succeeded, failed at
    /// apply, and were authored but never applied because of a
    /// downstream error. The denominator for `recipes_with_extracted_inner`
    /// and for cross-trial authoring-rate analysis.
    recipes_persisted: u32,
    /// Number of persisted recipes whose `produces` JSON contains at
    /// least one binding with `FieldValueSource::ExtractedInner`
    /// (serde-tagged `"kind":"extracted_inner"`). This is the
    /// ADR 0019 Phase 2A acceptance-gate metric: prompt experiments
    /// that aim to lift multi-leaf recognition are measured against
    /// this count, not against `records_produced` (which conflates
    /// extraction shape with successful apply).
    recipes_with_extracted_inner: u32,
    /// Per-outcome summary, one entry per `RecipeOutcome` in the
    /// report. Slim shape (no recipe-id, no per-target detail) so
    /// the JSONL stays human-readable on a single line.
    outcomes: Vec<OutcomeSummary>,
    /// `error_summary` from the FetchReport when the run failed
    /// before processing any recipe (closed run row, nothing to
    /// iterate). `None` on the normal path.
    fetch_error_summary: Option<String>,
    /// Trial-level error: an exception that escaped any of the
    /// stages (classify, accept, fetch). `None` on the normal path.
    /// Distinct from `fetch_error_summary`: this carries
    /// classification or DB errors that prevented `run_fetch_for_plan`
    /// from being called at all.
    trial_error: Option<String>,
}

/// Tagged enum of the six `RecipeOutcome` variants, flattened for
/// JSONL friendliness. Only the fields useful for cross-trial
/// analysis are kept; recipe-ids and per-target details live in the
/// per-trial DuckDB file for anyone who wants to reconstruct full
/// state.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OutcomeSummary {
    Succeeded {
        source_id: String,
        records_produced: u32,
    },
    Skipped {
        source_id: String,
        reason: String,
    },
    Failed {
        source_id: String,
        stage: String,
        message: String,
    },
    RateLimited {
        source_id: String,
        retry_after_seconds: Option<u64>,
    },
    Declined {
        source_id: String,
        reason: String,
    },
    LegacyPlanCannotAuthor {
        source_id: String,
    },
}

impl From<&RecipeOutcome> for OutcomeSummary {
    fn from(o: &RecipeOutcome) -> Self {
        match o {
            RecipeOutcome::Succeeded {
                source_id,
                records_produced,
                ..
            } => Self::Succeeded {
                source_id: source_id.clone(),
                records_produced: *records_produced,
            },
            RecipeOutcome::Skipped {
                source_id, reason, ..
            } => Self::Skipped {
                source_id: source_id.clone(),
                reason: reason.clone(),
            },
            RecipeOutcome::Failed {
                source_id,
                stage,
                message,
                ..
            } => Self::Failed {
                source_id: source_id.clone(),
                // Stage is a closed enum in the executor; render it
                // as snake_case here so the JSONL value matches the
                // serde rename used elsewhere.
                stage: format!("{:?}", stage).to_lowercase(),
                message: message.clone(),
            },
            RecipeOutcome::RateLimited {
                source_id,
                retry_after_seconds,
                ..
            } => Self::RateLimited {
                source_id: source_id.clone(),
                retry_after_seconds: *retry_after_seconds,
            },
            RecipeOutcome::Declined { source_id, reason } => Self::Declined {
                source_id: source_id.clone(),
                reason: reason.clone(),
            },
            RecipeOutcome::LegacyPlanCannotAuthor { source_id } => {
                Self::LegacyPlanCannotAuthor {
                    source_id: source_id.clone(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    // Default tracing posture for the harness: per-trial summary
    // lines from the harness itself, errors from anything else.
    // The pipeline's per-attempt INFO/WARN chatter (every URL
    // proposal, every prefetch, every per-target decline) was the
    // right shape for the desktop binary's interactive use, but
    // for an N-trial run on lithium it produces ~50–200 lines per
    // trial and drowns the per-trial summary the harness emits.
    // Operators who want the firehose can opt in by setting
    // `RUST_LOG` to whatever they prefer (e.g. `RUST_LOG=situation_room=info,warn`
    // reproduces the desktop default); the override is honoured
    // because we only set the env var when it's not already set.
    if std::env::var_os("RUST_LOG").is_none() {
        // SAFETY note: env::set_var becomes unsafe in edition 2024.
        // We're on 2021 where it's safe. When migrating to 2024,
        // wrap this in `unsafe { … }` with a SAFETY comment noting
        // it runs in main before any threads spawn.
        // The first directive's target is the binary's `[[bin]]
        // name` with hyphens turned to underscores
        // (`eval-harness` → `eval_harness`), NOT the package name.
        // tracing's `module_path!()` resolves at the crate root of
        // whichever crate emitted the log; for a binary that's the
        // bin name. The second directive captures every workspace
        // crate (which all share the `situation_room` prefix) at
        // ERROR so per-step pipeline chatter stays out of the
        // terminal during an N-trial run.
        std::env::set_var(
            "RUST_LOG",
            "eval_harness=info,situation_room=error",
        );
    }
    logging::init();

    let cli = Cli::parse();
    let topic = cli.topic.trim().to_string();
    if topic.is_empty() {
        anyhow::bail!("--topic must be non-empty");
    }
    if cli.trials == 0 {
        anyhow::bail!("--trials must be at least 1");
    }

    // Identify this harness run with a UUIDv7 — sortable, contains
    // the construction time. Used for the default db_dir and the
    // default JSONL filename.
    let harness_run_id = Uuid::now_v7();
    let started_at_utc = Utc::now();

    let db_dir = cli.db_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("situation_room-eval-{harness_run_id}"))
    });
    std::fs::create_dir_all(&db_dir)
        .with_context(|| format!("creating db dir {}", db_dir.display()))?;

    let out_path = cli.out.clone().unwrap_or_else(|| {
        let slug: String = topic
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_lowercase();
        let stamp = started_at_utc.format("%Y%m%dT%H%M%SZ");
        PathBuf::from("eval-runs").join(format!("{slug}-{stamp}.jsonl"))
    });
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output dir {}", parent.display()))?;
    }

    info!(
        harness_run_id = %harness_run_id,
        topic = %topic,
        trials = cli.trials,
        db_dir = %db_dir.display(),
        out = %out_path.display(),
        "eval harness starting"
    );

    // --- Provider + HTTP clients ---------------------------------------
    //
    // Same shape the desktop binary builds: one SecureHttpClient at
    // the LLM-tier 300s ceiling (used for provider calls AND for the
    // executor's runtime fetches), and a tighter 60s prefetch client
    // for the propose-URL retry loop's pre-fetch step.
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("building secure http client")?;
    let http_for_provider = http.clone();
    let http_arc = Arc::new(http);

    let prefetch_http_config = SecureHttpConfig {
        total_timeout: std::time::Duration::from_secs(60),
        ..SecureHttpConfig::default()
    };
    let prefetch_http = SecureHttpClient::new(prefetch_http_config)
        .context("building secure prefetch http client")?;
    let prefetch_http_arc = Arc::new(prefetch_http);

    let provider = pick_provider(http_for_provider)?;
    let provider_id = provider.id().to_string();

    // --- Source descriptors --------------------------------------------
    //
    // Threaded into the executor context, but production authoring no
    // longer consults this slice (Session 39). A missing file is
    // non-fatal — the same posture the desktop binary takes.
    let sources: Vec<SourceDescriptor> =
        match load_source_descriptors(&cli.sources, 30) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    path = %cli.sources.display(),
                    error = %e,
                    "loading sources failed; continuing with empty slice (production authoring does not consult this)"
                );
                Vec::new()
            }
        };

    // --- Trial loop ----------------------------------------------------
    //
    // Each trial:
    //   1. Fresh DuckDB at db_dir/trial-N.duckdb
    //   2. Migrate
    //   3. Classify topic → save plan
    //   4. Set status to Accepted
    //   5. Build ExecutorContext with a FRESH HostBackoff so 429s /
    //      timeouts on trial K don't poison trial K+1
    //   6. Call run_fetch_for_plan_impl
    //   7. Append a TrialReport to the JSONL.
    //
    // A trial that fails at any step writes a TrialReport with
    // `trial_error` populated and the loop continues.
    let mut out_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .with_context(|| format!("opening output {}", out_path.display()))?;
    use std::io::Write as _;

    let mut total_records: u64 = 0;
    let mut records_each: Vec<u32> = Vec::with_capacity(cli.trials as usize);
    let mut wall_clocks: Vec<f64> = Vec::with_capacity(cli.trials as usize);
    let mut trial_errors: u32 = 0;

    for trial in 0..cli.trials {
        let trial_db = db_dir.join(format!("trial-{trial}.duckdb"));
        let report = run_one_trial(
            trial,
            &topic,
            &trial_db,
            provider.as_ref(),
            &provider_id,
            http_arc.as_ref(),
            prefetch_http_arc.as_ref(),
            &sources,
            std::time::Duration::from_secs(cli.trial_timeout_secs),
        )
        .await;

        if report.trial_error.is_some() || report.fetch_error_summary.is_some() {
            trial_errors += 1;
        }
        total_records += report.records_produced as u64;
        records_each.push(report.records_produced);
        wall_clocks.push(report.wall_clock_s);

        // One JSON line per trial. Flush after each so an
        // interrupted harness run still leaves the partial JSONL on
        // disk for inspection.
        let line = serde_json::to_string(&report)
            .context("serializing trial report")?;
        writeln!(out_file, "{line}").context("writing trial report")?;
        out_file.flush().ok();

        info!(
            trial = trial,
            wall_clock_s = report.wall_clock_s,
            records = report.records_produced,
            attempted = report.recipes_attempted,
            succeeded = report.recipes_succeeded,
            "trial complete"
        );
    }

    // --- Summary -------------------------------------------------------
    //
    // Stderr only — JSONL stays one-line-per-trial so jq / pandas
    // can consume it as a stream.
    let mean_records = if cli.trials > 0 {
        total_records as f64 / cli.trials as f64
    } else {
        0.0
    };
    let min_records = records_each.iter().copied().min().unwrap_or(0);
    let max_records = records_each.iter().copied().max().unwrap_or(0);
    let stddev_records = stddev(&records_each);
    let mean_wall = if !wall_clocks.is_empty() {
        wall_clocks.iter().sum::<f64>() / wall_clocks.len() as f64
    } else {
        0.0
    };

    eprintln!();
    eprintln!("eval-harness summary");
    eprintln!("  topic:        {}", topic);
    eprintln!("  provider:     {}", provider_id);
    eprintln!("  trials:       {}", cli.trials);
    eprintln!("  trial_errors: {}", trial_errors);
    eprintln!(
        "  records:      mean {:.2}  min {}  max {}  stddev {:.2}",
        mean_records, min_records, max_records, stddev_records
    );
    eprintln!("  wall_clock:   mean {:.1} s", mean_wall);
    eprintln!("  jsonl:        {}", out_path.display());
    eprintln!("  per-trial DBs: {}", db_dir.display());

    // --- Cleanup -------------------------------------------------------
    if !cli.keep_dbs {
        if let Err(e) = std::fs::remove_dir_all(&db_dir) {
            warn!(
                db_dir = %db_dir.display(),
                error = %e,
                "could not clean up per-trial DB dir; remove it manually"
            );
        }
    } else {
        info!(db_dir = %db_dir.display(), "--keep-dbs set; per-trial DBs retained");
    }

    Ok(())
}

/// Successful return shape of the trial body's async block.
/// Carries the metadata the four `TrialReport` match arms below
/// need to assemble the JSONL line, including the two ADR 0019
/// authoring-shape counters (Session 64 instrumentation).
struct RunOneTrialOk {
    plan_id: Uuid,
    nominations_total: u32,
    recipes_persisted: u32,
    recipes_with_extracted_inner: u32,
    fetch_result: Result<FetchReport, FetchExecutorError>,
}

/// Run one trial end-to-end. Returns a `TrialReport` regardless of
/// outcome — failures are reported through `trial_error` /
/// `fetch_error_summary` rather than returning `Err`. The harness
/// loop relies on this so a failed trial does not abort the run.
async fn run_one_trial(
    trial: u32,
    topic: &str,
    db_path: &PathBuf,
    provider: &(dyn LlmProvider + Send + Sync),
    provider_id: &str,
    http: &SecureHttpClient,
    prefetch_http: &SecureHttpClient,
    sources: &[SourceDescriptor],
    trial_timeout: std::time::Duration,
) -> TrialReport {
    let started = Utc::now();
    let started_instant = Instant::now();

    // The wrapping `tokio::time::timeout` guards against a single
    // hung trial eating the entire harness budget. The inner future
    // is a single async block so any error path lands here and gets
    // turned into a TrialReport rather than a panic.
    let body = async {
        // 1. Open + migrate a fresh DB. A failure here is a trial
        //    error (no plan was ever persisted, no run row opened).
        let store = Store::open(db_path)
            .with_context(|| format!("opening store at {}", db_path.display()))?;
        store.migrate().context("running migrations")?;
        let store = Arc::new(store);

        // 2. Classify. The classifier's existing-topics injection
        //    is empty on a fresh DB; sources_memory likewise. This
        //    matches the "first run on a new install" baseline.
        let topic_rows = store
            .topics_in_use(30)
            .context("querying topics_in_use")?;
        let existing_topics: Vec<ClassifierTopicUsage> = topic_rows
            .into_iter()
            .map(|r| ClassifierTopicUsage {
                topic: r.topic.as_str().to_string(),
                uses: r.count,
            })
            .collect();
        let sources_memory = store
            .sources_memory(situation_room_storage::SOURCES_MEMORY_LIMIT)
            .context("querying sources_memory")?;
        let ctx = ClassificationContext {
            existing_topics,
            sources_memory,
            previous_rejection_reason: None,
        };
        let plan = classify_topic(
            provider,
            ModelTier::Workhorse,
            CLASSIFIER_PROMPT,
            topic,
            &ctx,
        )
        .await
        .context("classification failed")?;

        // 3. Persist + accept.
        save_research_plan(store.as_ref(), &plan, provider_id)
            .context("persisting plan")?;
        store
            .set_plan_status(plan.id, PlanStatus::Accepted)
            .context("setting plan status to accepted")?;

        let nominations_total = plan
            .expectations
            .document_sources
            .iter()
            .filter(|e| matches!(e, DocumentSourceEntry::Nomination(_)))
            .count() as u32;
        let plan_id = plan.id;

        // 4. Build a FRESH HostBackoff for this trial. The desktop
        //    binary creates one per binary boot and shares it
        //    forever; here we want each trial to be measured as a
        //    cold start so observed throttling on trial K cannot
        //    contaminate trial K+1's wall-clock or behaviour.
        let host_backoff = Arc::new(HostBackoff::new());
        let backoff_fetcher = BackoffFetcher::new(http, host_backoff.clone());
        let backoff_prefetch = BackoffFetcher::new(prefetch_http, host_backoff.clone());

        let executor_ctx = ExecutorContext {
            store: store.as_ref(),
            http: &backoff_fetcher,
            prefetch_http: Some(&backoff_prefetch),
            provider,
            recipe_author_prompt: RECIPE_AUTHOR_PROMPT,
            propose_url_prompt: PROPOSE_URL_PROMPT,
            // Session 77 — the eval harness measures fetch + apply
            // outcomes; per-Document Assertion extraction is an
            // additional LLM call per article that would distort
            // wall-clock measurements without changing the harness's
            // observable signal (it's measuring records, not
            // assertions). Skip explicitly here so the harness path
            // stays cost-bounded for repeat trials.
            document_assertions_prompt: None,
            // Session 78 — event extraction also skipped in eval
            // harness to keep wall-clock measurements clean.
            document_events_prompt: None,
            // Session 79 — observation extraction skipped for the
            // same reason: the extra workhorse-tier call per
            // Document distorts wall-clock and cost measurements,
            // and the harness's observable signal (records produced)
            // doesn't change with extracted observations.
            document_observations_prompt: None,
            sources,
        };

        // 5. Run the fetch. Per-recipe failures don't abort — they
        //    show up inside the FetchReport.outcomes vector.
        let fetch_result = run_fetch_for_plan_impl(&executor_ctx, plan.id).await;

        // 6. Inspect the recipes that landed in storage for this plan.
        //    We compute two counts here, outside the FetchReport
        //    contract, because the report doesn't surface authoring
        //    shape — only apply outcomes. ADR 0019 Phase 2A's
        //    acceptance gate is "did the LLM reach for the
        //    ExtractedInner shape at all," which is decided at
        //    authoring time and can be answered even when the recipe
        //    later failed at apply.
        //
        //    Substring match on the `produces_json` column is robust:
        //    `FieldValueSource::ExtractedInner` serialises with the
        //    tag `"kind":"extracted_inner"` (the same tag the
        //    recipe-apply tests assert against — see
        //    `crates/pipeline/src/recipes.rs` test
        //    `extracted_inner_round_trips_through_serde`). The
        //    substring cannot collide with the path or with any other
        //    field on the binding because `"kind"` is unique to the
        //    `FieldValueSource` tag.
        let recipes_for_plan = store.recipes_for_plan(plan_id)?;
        let recipes_persisted = recipes_for_plan.len() as u32;
        let recipes_with_extracted_inner = recipes_for_plan
            .iter()
            .filter(|r| r.produces_json.contains("\"kind\":\"extracted_inner\""))
            .count() as u32;

        Ok::<RunOneTrialOk, anyhow::Error>(RunOneTrialOk {
            plan_id,
            nominations_total,
            recipes_persisted,
            recipes_with_extracted_inner,
            fetch_result,
        })
    };

    let outcome = tokio::time::timeout(trial_timeout, body).await;

    let finished = Utc::now();
    let wall_clock_s = started_instant.elapsed().as_secs_f64();

    // Unpack the three failure modes:
    //   1. trial_timeout fired before the body completed
    //   2. body returned an Err — pre-fetch error (open, classify, …)
    //   3. body returned Ok but run_fetch_for_plan returned Err —
    //      executor wholesale failure
    //   4. body returned Ok with Ok(FetchReport) — normal path
    // The body's two new counters (recipes_persisted +
    // recipes_with_extracted_inner) are computed *after*
    // run_fetch_for_plan returns inside the body. Therefore they're
    // only populated on the Ok(Ok(_)) match arms. The two pre-fetch
    // failure arms (timeout, body Err) and the executor-wholesale-Err
    // arm leave them at zero — accurate: in those paths the harness
    // either never reached the recipe-author step or doesn't know
    // what landed in storage before the failure. Operators looking at
    // an authoring-shape signal should filter by `trial_error is None
    // && fetch_error_summary is None` first.
    match outcome {
        Err(_elapsed) => TrialReport {
            trial,
            topic: topic.to_string(),
            provider: provider_id.to_string(),
            started_at: started,
            finished_at: finished,
            wall_clock_s,
            db_path: db_path.clone(),
            plan_id: None,
            run_id: None,
            nominations_total: 0,
            recipes_attempted: 0,
            recipes_succeeded: 0,
            records_produced: 0,
            recipes_persisted: 0,
            recipes_with_extracted_inner: 0,
            outcomes: Vec::new(),
            fetch_error_summary: None,
            trial_error: Some(format!(
                "trial timed out after {}s",
                trial_timeout.as_secs()
            )),
        },
        Ok(Err(e)) => TrialReport {
            trial,
            topic: topic.to_string(),
            provider: provider_id.to_string(),
            started_at: started,
            finished_at: finished,
            wall_clock_s,
            db_path: db_path.clone(),
            plan_id: None,
            run_id: None,
            nominations_total: 0,
            recipes_attempted: 0,
            recipes_succeeded: 0,
            records_produced: 0,
            recipes_persisted: 0,
            recipes_with_extracted_inner: 0,
            outcomes: Vec::new(),
            fetch_error_summary: None,
            trial_error: Some(format!("{e:#}")),
        },
        Ok(Ok(RunOneTrialOk {
            plan_id,
            nominations_total,
            recipes_persisted,
            recipes_with_extracted_inner,
            fetch_result: Err(executor_err),
        })) => TrialReport {
            trial,
            topic: topic.to_string(),
            provider: provider_id.to_string(),
            started_at: started,
            finished_at: finished,
            wall_clock_s,
            db_path: db_path.clone(),
            plan_id: Some(plan_id),
            run_id: None,
            nominations_total,
            recipes_attempted: 0,
            recipes_succeeded: 0,
            records_produced: 0,
            recipes_persisted,
            recipes_with_extracted_inner,
            outcomes: Vec::new(),
            fetch_error_summary: Some(executor_err.to_string()),
            trial_error: None,
        },
        Ok(Ok(RunOneTrialOk {
            plan_id,
            nominations_total,
            recipes_persisted,
            recipes_with_extracted_inner,
            fetch_result: Ok(report),
        })) => {
            let outcomes: Vec<OutcomeSummary> =
                report.outcomes.iter().map(OutcomeSummary::from).collect();
            TrialReport {
                trial,
                topic: topic.to_string(),
                provider: provider_id.to_string(),
                started_at: started,
                finished_at: finished,
                wall_clock_s,
                db_path: db_path.clone(),
                plan_id: Some(plan_id),
                run_id: Some(report.run_id),
                nominations_total,
                recipes_attempted: report.recipes_attempted,
                recipes_succeeded: report.recipes_succeeded,
                records_produced: report.records_produced,
                recipes_persisted,
                recipes_with_extracted_inner,
                outcomes,
                fetch_error_summary: report.error_summary,
                trial_error: None,
            }
        }
    }
}

/// Population standard deviation over a slice of u32 records-per-
/// trial counts. Returns 0.0 for empty / single-element slices —
/// the metric is undefined there but the harness summary is more
/// useful with 0 than with NaN.
fn stddev(xs: &[u32]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let n = xs.len() as f64;
    let mean = xs.iter().map(|x| *x as f64).sum::<f64>() / n;
    let var = xs
        .iter()
        .map(|x| {
            let d = *x as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n;
    var.sqrt()
}

// ---------------------------------------------------------------------------
// LLM provider selection — copy of the desktop binary's `pick_provider`
// helper. Intentionally duplicated rather than shared (per the same
// rationale as the CLI's copy): pulling it into a library crate would
// expose app-level boot decisions through a crate boundary.
// ---------------------------------------------------------------------------

const LLM_PROVIDER_ENV: &str = "LLM_PROVIDER";

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
            anyhow::bail!(
                "unknown LLM_PROVIDER {other:?}; valid values are 'xai' or 'anthropic'"
            )
        }
    }
}
