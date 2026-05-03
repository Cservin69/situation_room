//! situation_room situation room — CLI entry point for Level-1 classification.
//!
//! Usage:
//!     situation-room "lithium supply chain"
//!     situation-room --db ./my.duckdb --sources ./sources.toml "EU AI Act"
//!     situation-room --topics-limit 50 --sources-limit 20 "container shipping rates"
//!     situation-room recent --limit 5
//!
//! The default subcommand is classify-and-persist for the topic given as
//! the bare argument. The `recent` subcommand lists the most recently
//! created plans without making any LLM calls.
//!
//! ## What this binary does
//!
//! 1. Loads `XAI_API_KEY` from the process env or a `.env` file.
//! 2. Opens the DuckDB store at `--db` (default `situation_room.duckdb` in CWD).
//!    Runs migrations (idempotent).
//! 3. Queries `Store::topics_in_use(limit)` to populate the classifier's
//!    existing-topics injection.
//! 4. Loads source descriptors from `--sources` (default
//!    `config/sources.toml`) for the registered-sources injection.
//! 5. Builds a [`ClassificationContext`] and calls [`classify_topic`]
//!    against xAI with the production prompt loaded via `include_str!`.
//! 6. Persists the resulting [`ResearchPlan`] via
//!    [`research_plans_store::save_research_plan`].
//! 7. Prints the plan as pretty JSON to stdout, with a small human
//!    summary on stderr.
//!
//! ## What this binary does NOT do
//!
//! Recipe authoring (Level-2) is not invoked here. Persisting the plan
//! is the v1 stopping point; downstream sessions wire authoring on top.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use situation_room_apps_common::sources::load_source_descriptors;
use situation_room_llm::{AnthropicProvider, LlmProvider, ModelTier, XaiProvider};
use situation_room_pipeline::research_classifier::{
    classify_topic, ClassificationContext, TopicUsage as ClassifierTopicUsage,
};
use situation_room_pipeline::research_plans_store::save_research_plan;
use situation_room_secure::http::{SecureHttpClient, SecureHttpConfig};
use situation_room_storage::Store;

/// The production classifier prompt, embedded at compile time so the
/// binary doesn't have to discover the markdown at runtime.
const CLASSIFIER_PROMPT: &str = include_str!("../../../config/prompts/research_classifier.md");

#[derive(Parser, Debug)]
#[command(name = "situation-room", version, about = "Classify a topic into a situation_room research plan.")]
struct Cli {
    /// Path to the DuckDB store. Created if absent.
    #[arg(long, default_value = "situation_room.duckdb")]
    db: PathBuf,

    /// Path to the source descriptors TOML.
    #[arg(long, default_value = "config/sources.toml")]
    sources: PathBuf,

    /// How many topics-in-use to surface to the classifier.
    #[arg(long, default_value_t = 30)]
    topics_limit: usize,

    /// Cap on source descriptors surfaced to the classifier (after
    /// reading sources.toml). Mostly a guard against pathological
    /// configurations.
    #[arg(long, default_value_t = 30)]
    sources_limit: usize,

    /// Subcommand. Omitting it requires a topic argument and runs
    /// classification.
    #[command(subcommand)]
    command: Option<Command>,

    /// The topic to classify (used when no subcommand is given).
    #[arg(required = false)]
    topic: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List the most recently created plans without calling the LLM.
    Recent {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logging: a sensible default, overridable via RUST_LOG. We
    // deliberately log to stderr so stdout stays a clean JSON channel
    // for the persisted plan.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    // Open the store once for both subcommands.
    let store = Store::open(&cli.db)
        .with_context(|| format!("opening store at {}", cli.db.display()))?;
    store.migrate().context("running migrations")?;

    match cli.command {
        Some(Command::Recent { limit }) => run_recent(&store, limit),
        None => {
            let topic = cli
                .topic
                .clone()
                .context("no topic given (pass a topic as a positional argument, or use a subcommand)")?;
            run_classify(&store, &cli, &topic).await
        }
    }
}

fn run_recent(store: &Store, limit: usize) -> Result<()> {
    let plans = store.recent_research_plans(limit).context("listing plans")?;
    if plans.is_empty() {
        eprintln!("(no research plans persisted yet)");
        return Ok(());
    }
    eprintln!("{} most recent plans:", plans.len());
    for p in &plans {
        // Stderr summary, stdout machine-readable id+topic per line.
        eprintln!(
            "  {}  {}  ({})",
            p.created_at.format("%Y-%m-%d %H:%M"),
            p.topic,
            p.id
        );
        println!("{}\t{}", p.id, p.topic);
    }
    Ok(())
}

async fn run_classify(store: &Store, cli: &Cli, topic: &str) -> Result<()> {
    if topic.trim().is_empty() {
        anyhow::bail!("topic must be non-empty");
    }

    // 1. Build the LLM provider. SecureHttpClient applies the same
    //    network defenses as every other situation_room HTTP call.
    //    `pick_provider` reads `LLM_PROVIDER` (default `"xai"`) and
    //    builds the matching concrete provider. Session 23 promoted
    //    Anthropic from stub to real; both providers are now valid
    //    picks. The trait object is what the classify_topic function
    //    takes (`&dyn LlmProvider`) so the call site is unchanged.
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("building secure http client")?;
    let provider = pick_provider(http)?;

    // 2. Existing-topics injection — the classifier's hygiene context.
    let topic_rows = store
        .topics_in_use(cli.topics_limit)
        .context("querying topics_in_use")?;
    let existing_topics: Vec<ClassifierTopicUsage> = topic_rows
        .into_iter()
        .map(|r| ClassifierTopicUsage {
            // The storage layer returns Topic newtypes; the classifier
            // takes plain strings for crate-boundary hygiene.
            topic: r.topic.as_str().to_string(),
            uses: r.count,
        })
        .collect();

    // 3. Registered-sources injection — loaded from the TOML file.
    let registered_sources = load_source_descriptors(&cli.sources, cli.sources_limit)
        .with_context(|| format!("loading sources from {}", cli.sources.display()))?;

    let ctx = ClassificationContext {
        existing_topics,
        registered_sources,
        // CLI does not carry a re-classification flow today; the
        // GUI is the (Session 15) home for rejection feedback. This
        // field is kept None so the classifier prompt's
        // {{USER_FEEDBACK}} placeholder substitutes to empty,
        // preserving the prior behaviour bit-for-bit.
        previous_rejection_reason: None,
    };

    eprintln!(
        "classifying topic ({} existing topics in scope, {} registered sources)…",
        ctx.existing_topics.len(),
        ctx.registered_sources.len()
    );

    // 4. Classify. `provider.as_ref()` deref-coerces the
    //    `Box<dyn LlmProvider>` into the `&dyn LlmProvider` the
    //    classifier takes.
    let plan = classify_topic(
        provider.as_ref(),
        ModelTier::Workhorse,
        CLASSIFIER_PROMPT,
        topic,
        &ctx,
    )
    .await
    .context("classification failed")?;

    // 5. Persist. The lineage column carries the provider id chosen
    //    at boot — `"xai"`, `"anthropic"`, or whatever future
    //    providers register. This is the source of truth for "which
    //    LLM ran this classification" and survives when the running
    //    binary moves between providers.
    save_research_plan(store, &plan, provider.id()).context("persisting plan")?;

    // 6. Stderr summary, stdout pretty JSON.
    eprintln!(
        "plan {} saved ({} topic_tags, {} obs, {} events, {} entities, {} relations, {} doc-sources)",
        plan.id,
        plan.topic_tags.len(),
        plan.expectations.observation_metrics.len(),
        plan.expectations.event_types.len(),
        plan.expectations.entity_kinds.len(),
        plan.expectations.relation_kinds.len(),
        plan.expectations.document_sources.len(),
    );
    let pretty = serde_json::to_string_pretty(&plan).context("serializing plan to JSON")?;
    println!("{pretty}");

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
/// it so the rest of the function can call `classify_topic` without
/// knowing which provider it got. Mirrors the desktop binary's helper
/// of the same name; the two are intentionally duplicated rather than
/// shared because pulling the helper into a library crate would expose
/// app-level boot decisions through a crate boundary.
///
/// Returns a clear error if the chosen provider's API key isn't set —
/// rather than silently falling back to the other provider, which
/// would surprise an operator who explicitly asked for one.
fn pick_provider(
    http: SecureHttpClient,
) -> Result<Box<dyn LlmProvider + Send + Sync>> {
    let choice = std::env::var(LLM_PROVIDER_ENV)
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xai".to_string());

    eprintln!("provider: {choice}");

    match choice.as_str() {
        "xai" | "grok" => {
            let p = XaiProvider::from_env(http).context(
                "XAI_API_KEY not found — set it in the environment or in a .env file at the workspace root",
            )?;
            Ok(Box::new(p))
        }
        "anthropic" | "claude" => {
            let p = AnthropicProvider::from_env(http).context(
                "ANTHROPIC_API_KEY not found — set it in the environment or in a .env file at the workspace root",
            )?;
            Ok(Box::new(p))
        }
        other => {
            anyhow::bail!(
                "unknown LLM_PROVIDER {other:?}; valid values are 'xai' or 'anthropic'"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Source descriptor loading
// ---------------------------------------------------------------------------
//
// Session 24: this used to be a local copy of the loader, word-for-
// word identical to the desktop binary's. Both copies now call into
// `situation_room_apps_common::sources::load_source_descriptors`. See
// `crates/apps_common/src/lib.rs` for the contract on what does and
// does not belong in that crate.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Session 24: the seven `load_source_descriptors_*` tests that lived
// here moved alongside the loader into
// `crates/apps_common/src/sources.rs::tests` (where the new
// `parses_entry_without_endpoint_hint_documents_omission` test
// joined them). Only the binary-specific prompt-shape sanity check
// remains here, because it asserts on this binary's `CLASSIFIER_PROMPT`
// const and would not be a meaningful test in a shared crate.

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: the production prompt is non-trivially long and
    /// contains the placeholders the classifier expects. This is a
    /// guard against accidentally shipping an empty or stripped
    /// markdown file.
    #[test]
    fn classifier_prompt_contains_required_placeholders() {
        assert!(
            CLASSIFIER_PROMPT.len() > 500,
            "embedded prompt is suspiciously short ({} bytes)",
            CLASSIFIER_PROMPT.len()
        );
        assert!(CLASSIFIER_PROMPT.contains("{{TOPIC}}"));
        assert!(CLASSIFIER_PROMPT.contains("{{EXISTING_TOPICS}}"));
        assert!(CLASSIFIER_PROMPT.contains("{{REGISTERED_SOURCES}}"));
    }
}
