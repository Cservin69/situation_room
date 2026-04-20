//! # stockpile-demo
//!
//! A console demo that runs the pipeline we have today — fetch a
//! USGS MCS PDF, extract text, store as a `Document`, query back.
//!
//! Observations do not land yet. The architecture (ADR 0007) calls
//! for LLM-authored `FetchRecipe`s to produce Observations
//! deterministically at runtime. That machinery is under
//! construction; until it exists, this demo stops at Document
//! ingestion. See `STOCKPILE_HANDOFF_SESSION2.md`.
//!
//! Usage:
//!
//! ```text
//! cargo run -p stockpile-demo -- --commodity lithium --year 2025 --db stockpile.duckdb
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use stockpile_core::vocab::{Topic, Unit};
use stockpile_core::Record;
use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};
use stockpile_sources::adapters::usgs::UsgsMcsAdapter;
use stockpile_sources::traits::{FetchContext, Source};
use stockpile_storage::Store;

#[derive(Parser, Debug)]
#[command(
    name = "stockpile-demo",
    about = "End-to-end demo: fetch a USGS commodity PDF, extract text, store a Document."
)]
struct Args {
    #[arg(long, default_value = "lithium")]
    commodity: String,

    #[arg(long, default_value_t = 2025)]
    year: u16,

    #[arg(long, default_value = "stockpile.duckdb")]
    db: PathBuf,

    #[arg(long)]
    topic: Option<String>,
}

fn default_topic(commodity: &str) -> &'static str {
    match commodity {
        "lithium" => "Li",
        "copper" => "Cu",
        "nickel" => "Ni",
        "cobalt" => "Co",
        "aluminum" => "Al",
        "zinc" => "Zn",
        "lead" => "Pb",
        "silver" => "Ag",
        "gold" => "Au",
        "uranium" => "U",
        _ => "unknown",
    }
}

fn hr(width: usize) {
    println!("{}", "─".repeat(width));
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,stockpile_demo=info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

    let topic_str = args
        .topic
        .clone()
        .unwrap_or_else(|| default_topic(&args.commodity).to_string());
    if topic_str == "unknown" && args.topic.is_none() {
        anyhow::bail!(
            "No default topic known for commodity '{}'. Pass --topic <TAG>.",
            args.commodity
        );
    }
    let topic = Topic::new(&topic_str)
        .with_context(|| format!("invalid topic string: {topic_str:?}"))?;
    let unit = Unit::new("t").context("unit construction")?;

    let term_width: usize = 72;
    hr(term_width);
    println!(
        "Stockpile demo — USGS MCS {}: {} (topic = {})",
        args.year, args.commodity, topic
    );
    println!("DB: {}", args.db.display());
    hr(term_width);

    // Step 1: storage
    let t0 = Instant::now();
    let store = Store::open(&args.db).context("open DuckDB store")?;
    store.migrate().context("apply migrations")?;
    println!("✓ storage ready          ({:.2?})", t0.elapsed());

    // Step 2: adapter
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("build SecureHttpClient")?;
    let adapter =
        UsgsMcsAdapter::new(http, args.year, &args.commodity, topic.clone(), unit);

    // Step 3: fetch
    let t0 = Instant::now();
    println!("  fetching from: {}", adapter.pdf_url());
    let outcome = adapter
        .fetch(FetchContext {
            since: None,
            focus: vec![],
        })
        .await
        .context("adapter fetch")?;
    println!("✓ fetch + extract text   ({:.2?})", t0.elapsed());

    for note in &outcome.notes {
        println!("  note: {}", note);
    }

    // Step 4: store. On re-run, dedup_key conflicts are expected.
    let t0 = Instant::now();
    let mut inserted = 0usize;
    let mut dedup_skipped = 0usize;
    for rec in &outcome.records {
        match store.insert_record(rec) {
            Ok(()) => inserted += 1,
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("unique")
                    || msg.contains("duplicate")
                    || msg.contains("primary key")
                    || msg.contains("constraint")
                {
                    dedup_skipped += 1;
                } else {
                    return Err(anyhow::anyhow!("insert failed: {e}"));
                }
            }
        }
    }
    println!(
        "✓ storage writes         ({:.2?})  [{} inserted, {} dedup-skipped]",
        t0.elapsed(),
        inserted,
        dedup_skipped
    );

    // Step 5: summarize
    println!();
    println!("Top topics in store:");
    let topics = store.topics_in_use(5).context("query topics_in_use")?;
    if topics.is_empty() {
        println!("  (none)");
    } else {
        for t in &topics {
            println!("  {:<16}  {} record(s)", t.topic.to_string(), t.count);
        }
    }

    // Document summary (not a production table — we don't have
    // Observations yet).
    println!();
    let doc_count = outcome
        .records
        .iter()
        .filter(|r| matches!(r, Record::Document(_)))
        .count();
    println!(
        "Documents from this fetch: {} (body preserved as text/plain in DuckDB)",
        doc_count
    );
    for rec in &outcome.records {
        if let Record::Document(doc) = rec {
            let title = doc.title.as_deref().unwrap_or("(untitled)");
            let body_preview: String = doc.body.chars().take(120).collect();
            let body_preview = body_preview.replace('\n', " ").trim().to_string();
            println!("  • {} — {} chars", title, doc.body.len());
            if !body_preview.is_empty() {
                println!("    preview: {}…", body_preview);
            }
        }
    }

    println!();
    hr(term_width);
    println!(
        "Note: Observations are not emitted by this demo. They require \
         the Level-2 FetchRecipe apply runtime (ADR 0007). Under \
         construction — see STOCKPILE_HANDOFF_SESSION2.md."
    );
    hr(term_width);

    Ok(())
}
