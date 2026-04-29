//! Stockpile situation room — CLI entry point for Level-1 classification.
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
//! 2. Opens the DuckDB store at `--db` (default `stockpile.duckdb` in CWD).
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
use serde::Deserialize;
use std::path::{Path, PathBuf};

use stockpile_llm::{ModelTier, XaiProvider};
use stockpile_pipeline::research_classifier::{
    classify_topic, ClassificationContext, SourceDescriptor, TopicUsage as ClassifierTopicUsage,
};
use stockpile_pipeline::research_plans_store::save_research_plan;
use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};
use stockpile_storage::Store;

/// The production classifier prompt, embedded at compile time so the
/// binary doesn't have to discover the markdown at runtime.
const CLASSIFIER_PROMPT: &str = include_str!("../../../config/prompts/research_classifier.md");

#[derive(Parser, Debug)]
#[command(name = "situation-room", version, about = "Classify a topic into a Stockpile research plan.")]
struct Cli {
    /// Path to the DuckDB store. Created if absent.
    #[arg(long, default_value = "stockpile.duckdb")]
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
    //    network defenses as every other Stockpile HTTP call.
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("building secure http client")?;
    let provider = XaiProvider::from_env(http).context(
        "XAI_API_KEY not found — set it in the environment or in a .env file at the workspace root",
    )?;

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
    };

    eprintln!(
        "classifying topic ({} existing topics in scope, {} registered sources)…",
        ctx.existing_topics.len(),
        ctx.registered_sources.len()
    );

    // 4. Classify.
    let plan = classify_topic(
        &provider,
        ModelTier::Workhorse,
        CLASSIFIER_PROMPT,
        topic,
        &ctx,
    )
    .await
    .context("classification failed")?;

    // 5. Persist.
    save_research_plan(store, &plan, "xai").context("persisting plan")?;

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
// Source descriptor loading
// ---------------------------------------------------------------------------

/// On-disk shape of `config/sources.toml`. Mirrors
/// [`SourceDescriptor`] one-for-one but keeps `authoritative_for`
/// optional so simple entries don't need to declare it.
#[derive(Debug, Deserialize)]
struct SourcesFile {
    #[serde(default)]
    source: Vec<SourceEntry>,
}

#[derive(Debug, Deserialize)]
struct SourceEntry {
    id: String,
    display_name: String,
    description: String,
    #[serde(default)]
    authoritative_for: Vec<String>,
}

fn load_source_descriptors(path: &Path, limit: usize) -> Result<Vec<SourceDescriptor>> {
    if !path.exists() {
        // Missing file is not an error — the classifier handles an
        // empty list by telling the LLM to nominate by description
        // only. We log a warning so users notice if they expected it
        // to load.
        tracing::warn!(
            path = %path.display(),
            "sources file not found; classifier will see no registered sources"
        );
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let file: SourcesFile = toml::from_str(&raw)
        .with_context(|| format!("parsing TOML in {}", path.display()))?;

    let descriptors: Vec<SourceDescriptor> = file
        .source
        .into_iter()
        .take(limit)
        .map(|e| SourceDescriptor {
            id: e.id,
            display_name: e.display_name,
            description: e.description.trim().to_string(),
            authoritative_for: e.authoritative_for,
        })
        .collect();

    Ok(descriptors)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_source_descriptors_reads_real_config_file() {
        // The real config file should always parse — if this fails,
        // someone broke the schema or removed a required field.
        let path = Path::new("../../config/sources.toml");
        if !path.exists() {
            // Tests may be run from various CWDs; skip cleanly rather
            // than fail when the relative path doesn't resolve.
            return;
        }
        let out = load_source_descriptors(path, 100).expect("real config should parse");
        assert!(!out.is_empty(), "real config should have at least one source");
    }

    #[test]
    fn load_source_descriptors_returns_empty_for_missing_file() {
        let out = load_source_descriptors(Path::new("/nonexistent/path/sources.toml"), 10).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn load_source_descriptors_respects_limit() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "a"
display_name = "A"
description = "first"

[[source]]
id = "b"
display_name = "B"
description = "second"

[[source]]
id = "c"
display_name = "C"
description = "third"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
    }

    #[test]
    fn load_source_descriptors_handles_empty_authoritative_for() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "x"
display_name = "X"
description = "no authority field"
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].authoritative_for.is_empty());
    }

    #[test]
    fn load_source_descriptors_trims_description_whitespace() {
        let dir = tempdir();
        let p = dir.join("sources.toml");
        let toml = r#"
[[source]]
id = "y"
display_name = "Y"
description = """

  Indented description.

"""
"#;
        std::fs::write(&p, toml).unwrap();

        let out = load_source_descriptors(&p, 10).unwrap();
        assert_eq!(out[0].description, "Indented description.");
    }

    /// Tiny in-process tempdir helper. We don't pull in `tempfile`
    /// for one test fixture; this is enough.
    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce: u64 = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        };
        p.push(format!("stockpile_situation_room_test_{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        // The dir leaks on test crash; acceptable for now.
        p
    }

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
