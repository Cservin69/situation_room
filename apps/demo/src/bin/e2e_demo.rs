//! # stockpile-e2e
//!
//! End-to-end demo of the Level-2 pipeline (ADR 0007).
//!
//! Walks every stage:
//!
//! 1. Open the DuckDB store and apply migrations (including the
//!    recipes table, migration 0003).
//! 2. Build a [`ResearchPlan`] in code. (Level-1 classification is
//!    its own session's work; for this demo the plan is hand-written
//!    and clearly labeled as a placeholder.)
//! 3. Fetch a sample of the source through [`SecureHttpClient`] so
//!    the LLM has a real excerpt to author against.
//! 4. Ask xAI to author a [`FetchRecipe`] via [`author_recipe`].
//! 5. Persist the recipe through [`recipes_store::save_recipe`].
//! 6. Fetch the source again — demonstrates that the runtime path
//!    is fully separate from the authoring path.
//! 7. Apply the recipe via [`recipe_apply::apply`] to produce
//!    records deterministically, with no LLM involvement.
//! 8. Store the resulting records.
//! 9. Query back and print the Observation with its provenance
//!    chain, confirming the recipe id + version made it into the
//!    `source_id` field per ADR 0007.
//!
//! ## The source
//!
//! World Bank Indicators API. Auth-free, small JSON payload, stable
//! shape. For this demo we ask for Chile's total population in 2022.
//! The response shape is an array whose second element is an array
//! of datapoints — JSONPath `$[1][0].value` picks the scalar.
//!
//! ## Why not USGS / PDF
//!
//! `ExtractionSpec::PdfTable` is explicitly not implemented in the
//! runtime today — see the apply module's doc comment and the 2026-04-22
//! review note on ADR 0007. Once positional PDF table extraction
//! lands as its own session, a sibling `stockpile-e2e-usgs` demo can
//! exercise that path.
//!
//! ## Running
//!
//! ```text
//! # XAI_API_KEY must be in the workspace .env (or your shell env).
//! cargo run -p stockpile-demo --bin stockpile-e2e
//! cargo run -p stockpile-demo --bin stockpile-e2e -- --db /tmp/stockpile-e2e.duckdb
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use chrono::{TimeZone, Utc};
use clap::Parser;
use stockpile_core::vocab::{EventType, Topic, Unit};
use stockpile_core::Record;
use stockpile_llm::{ModelTier, XaiProvider};
use stockpile_pipeline::recipe_apply::{apply, ApplyContext};
use stockpile_pipeline::recipe_author::{author_recipe, AuthoringContext};
use stockpile_pipeline::recipes_store;
use stockpile_pipeline::research::{
    DocumentSourceHint, EntityKindExpectation, EventTypeExpectation, MetricExpectation,
    RecordExpectations, RelationKindExpectation, ResearchPlan,
};
use stockpile_secure::http::{SecureHttpClient, SecureHttpConfig};
use stockpile_storage::Store;
use url::Url;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(
    name = "stockpile-e2e",
    about = "End-to-end pipeline demo: plan → author recipe via xAI → store → apply → Observation."
)]
struct Args {
    /// DuckDB file. Created if absent. Fresh file is recommended
    /// for demos so the recipe table starts empty.
    #[arg(long, default_value = "stockpile-e2e.duckdb")]
    db: PathBuf,

    /// Skip the LLM authoring step and use a hand-written recipe.
    /// Useful when xAI is unavailable or for deterministic smoke
    /// tests against the apply path alone.
    #[arg(long)]
    offline: bool,
}

/// The URL the demo fetches. Intentionally a constant, not LLM-
/// decided, because Level-2 authoring is about extraction shape,
/// not source discovery — the source registry is what tells a
/// future Level-2 what URL to author against, and for the demo
/// the registry's single candidate is World Bank.
const WORLD_BANK_URL: &str =
    "https://api.worldbank.org/v2/country/CL/indicator/SP.POP.TOTL?format=json&date=2022";

/// Prompt template for recipe authoring. Loaded from the versioned
/// markdown file at workspace root.
const RECIPE_AUTHOR_PROMPT: &str = include_str!("../../../../config/prompts/recipe_author.md");

fn hr(width: usize) {
    println!("{}", "─".repeat(width));
}

fn step(n: u32, msg: &str) {
    println!();
    println!("▸ step {n}: {msg}");
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env before anything else — XAI_API_KEY must be visible
    // to ApiKey::from_env. Ignore errors: missing .env is fine if
    // the shell env has the key, and the --offline path doesn't
    // need it at all.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,stockpile_e2e=info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

    let term_width = 72;
    hr(term_width);
    println!("Stockpile e2e demo — World Bank / Chile population 2022");
    println!("DB: {}", args.db.display());
    if args.offline {
        println!("Mode: OFFLINE (recipe hand-written, no LLM call)");
    } else {
        println!("Mode: LIVE (recipe authored via xAI)");
    }
    hr(term_width);

    // -----------------------------------------------------------------
    step(1, "open store + apply migrations");
    let t0 = Instant::now();
    let store = Store::open(&args.db).context("open DuckDB store")?;
    store.migrate().context("apply migrations")?;
    println!("  ✓ ready ({:.2?})", t0.elapsed());

    // -----------------------------------------------------------------
    step(2, "build ResearchPlan (hand-written — Level-1 is a future session)");
    let plan = build_plan()?;
    println!(
        "  ✓ plan: topic={:?}, topics={}, metrics={}",
        plan.topic,
        plan.topic_tags.len(),
        plan.expectations.observation_metrics.len(),
    );

    // -----------------------------------------------------------------
    step(3, "fetch source excerpt (for recipe authoring context)");
    let http = SecureHttpClient::new(SecureHttpConfig::default())
        .context("build SecureHttpClient")?;
    let sample_url = Url::parse(WORLD_BANK_URL).context("parse world bank url")?;

    let t0 = Instant::now();
    let bytes = http
        .get_bytes(sample_url.as_str())
        .await
        .context("fetch world bank sample")?;
    println!("  ✓ fetched {} bytes ({:.2?})", bytes.len(), t0.elapsed());

    let excerpt =
        std::str::from_utf8(&bytes).context("world bank response was not utf-8")?;

    // -----------------------------------------------------------------
    step(4, "author recipe");
    let recipe = if args.offline {
        println!("  [offline] constructing recipe in code");
        hand_written_recipe(&plan, &sample_url)?
    } else {
        author_via_xai(&http, &plan, &sample_url, excerpt).await?
    };
    println!(
        "  ✓ recipe id={}, version={}, authored_by={}",
        recipe.id, recipe.version, recipe.authored_by
    );
    println!("  ✓ extraction: {:?}", recipe.extraction);
    println!("  ✓ produces {} binding(s)", recipe.produces.len());

    // -----------------------------------------------------------------
    step(5, "persist recipe");
    recipes_store::save_recipe(&store, &recipe).context("save recipe")?;
    println!("  ✓ saved; store now holds {} recipe(s)", store.count_recipes()?);

    // -----------------------------------------------------------------
    step(6, "fetch source again (runtime path, separate from authoring)");
    let t0 = Instant::now();
    let runtime_bytes = http
        .get_bytes(recipe.source_url.as_str())
        .await
        .context("runtime fetch")?;
    let fetched_at = Utc::now();
    println!(
        "  ✓ fetched {} bytes ({:.2?}) from {}",
        runtime_bytes.len(),
        t0.elapsed(),
        recipe.source_url
    );

    // -----------------------------------------------------------------
    step(7, "apply recipe (deterministic, no LLM)");
    let t0 = Instant::now();
    let ctx = ApplyContext {
        recipe: &recipe,
        plan: &plan,
        bytes: &runtime_bytes,
        fetched_at,
    };
    let records = apply(ctx).context("recipe apply")?;
    println!(
        "  ✓ produced {} record(s) ({:.2?})",
        records.len(),
        t0.elapsed()
    );

    // -----------------------------------------------------------------
    step(8, "persist produced records");
    let mut inserted = 0usize;
    let mut dedup_skipped = 0usize;
    for rec in &records {
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
                    return Err(anyhow!("insert failed: {e}"));
                }
            }
        }
    }
    println!(
        "  ✓ inserted={}, dedup_skipped={}",
        inserted, dedup_skipped
    );

    // -----------------------------------------------------------------
    step(9, "summarize");
    println!();
    for rec in &records {
        if let Record::Observation(obs) = rec {
            hr(term_width);
            println!("Observation");
            println!("  id         : {}", obs.id);
            println!("  metric     : {}", obs.content.metric);
            println!("  value      : {}", obs.content.value);
            println!("  unit       : {}", obs.content.unit.as_str());
            println!("  period     : {:?}", obs.content.period);
            println!("  observed_at: {}", obs.envelope.observed_at);
            println!("  topics     : {:?}",
                obs.envelope.subjects.topics.iter().map(|t| t.as_str()).collect::<Vec<_>>());
            println!("  provenance : {}", obs.envelope.provenance.source_id);
            if let Some(url) = &obs.envelope.provenance.source_url {
                println!("  source_url : {}", url);
            }
            hr(term_width);
        }
    }

    let topics = store.topics_in_use(10).context("topics_in_use")?;
    if !topics.is_empty() {
        println!();
        println!("Top topics in store:");
        for t in &topics {
            println!("  {:<20} {} record(s)", t.topic.to_string(), t.count);
        }
    }

    println!();
    hr(term_width);
    println!("End-to-end pipeline complete.");
    hr(term_width);

    Ok(())
}

// ---------------------------------------------------------------------------
// Recipe authoring — two paths: xAI (default) and offline (hand-written)
// ---------------------------------------------------------------------------

async fn author_via_xai(
    http: &SecureHttpClient,
    plan: &ResearchPlan,
    sample_url: &Url,
    excerpt: &str,
) -> Result<stockpile_pipeline::recipes::FetchRecipe> {
    let provider = XaiProvider::from_env(http.clone()).ok_or_else(|| {
        anyhow!(
            "XAI_API_KEY not found in env or .env. Set it in your workspace \
             .env file, or re-run with --offline to skip the LLM authoring step."
        )
    })?;

    let ctx = AuthoringContext {
        source_id: "world_bank_indicators".into(),
        sample_url: sample_url.clone(),
        // Excerpts over the prompt-body bound are rejected at build
        // time — World Bank responses for a single indicator/year
        // are tiny (single-digit KB) so this is a non-issue in
        // practice. The check stays on for safety.
        document_excerpt: excerpt.to_string(),
    };

    let t0 = Instant::now();
    let recipe = author_recipe(&provider, ModelTier::Workhorse, RECIPE_AUTHOR_PROMPT, plan, &ctx)
        .await
        .context("xAI recipe authoring failed")?;
    println!("  ✓ xAI authored recipe ({:.2?})", t0.elapsed());

    // The authoring function stamps `source_id` empty by design —
    // it comes from the source registry, which hasn't been built
    // yet. Set it here so the provenance string is honest.
    let mut recipe = recipe;
    recipe.source_id = "world_bank_indicators".into();
    recipe.dedup_key = Some(format!(
        "{}:{}:population",
        recipe.plan_id, recipe.source_id
    ));

    Ok(recipe)
}

/// Hand-written recipe for the --offline path. Matches what a
/// correct LLM author should produce for this URL + plan.
///
/// JSONPath `$[1][0].value` — World Bank wraps data in an outer
/// `[metadata, datapoints]` pair; the first datapoint's `value` is
/// the scalar we want.
fn hand_written_recipe(
    plan: &ResearchPlan,
    sample_url: &Url,
) -> Result<stockpile_pipeline::recipes::FetchRecipe> {
    use stockpile_core::RecordType;
    use stockpile_pipeline::recipes::{
        ExpectationRef, ExtractionSpec, FetchRecipe, FieldMap, FieldValueSource,
        ProductionBinding,
    };

    let plan_id = plan_id_stable(plan);

    Ok(FetchRecipe {
        id: Uuid::now_v7(),
        dedup_key: Some(format!("{plan_id}:world_bank_indicators:population")),
        plan_id,
        source_id: "world_bank_indicators".into(),
        source_url: sample_url.clone(),
        extraction: ExtractionSpec::JsonPath {
            path: "$[1][0].value".into(),
        },
        produces: vec![ProductionBinding {
            record_type: RecordType::Observation,
            expectation: ExpectationRef::ObservationMetric { index: 0 },
            field_mappings: vec![
                FieldMap {
                    path: "value".into(),
                    source: FieldValueSource::Extracted,
                },
                FieldMap {
                    path: "metric".into(),
                    source: FieldValueSource::FromPlan {
                        pointer: "expectations.observation_metrics.0.name".into(),
                    },
                },
                FieldMap {
                    path: "unit".into(),
                    source: FieldValueSource::Literal {
                        value: serde_json::json!("1"),
                    },
                },
                FieldMap {
                    path: "period".into(),
                    source: FieldValueSource::Literal {
                        value: serde_json::json!("annual"),
                    },
                },
            ],
        }],
        authored_at: Utc::now(),
        authored_by: "offline".into(),
        version: 1,
    })
}

/// Deterministic stand-in plan_id for the offline path. ResearchPlan
/// doesn't carry an id field today; when Level-1 stores the plan,
/// the storage id will be threaded through. For the demo we derive
/// a stable UUID from the plan's topic so re-runs use the same
/// plan_id and the dedup_key remains stable.
fn plan_id_stable(plan: &ResearchPlan) -> Uuid {
    // A UUIDv4 from a fixed namespace keyed on the topic would be
    // a v5 (name-based). Without a uuid v5 feature here we just
    // generate a v7 once per run — acceptable for a demo; the
    // dedup_key rotates per run accordingly.
    let _ = plan;
    Uuid::now_v7()
}

// ---------------------------------------------------------------------------
// Plan construction — the hand-written placeholder Level-1 output
// ---------------------------------------------------------------------------

fn build_plan() -> Result<ResearchPlan> {
    Ok(ResearchPlan {
        topic: "Chile total population 2022".into(),
        interpretation:
            "Demographic observation: Chile's total population as of \
             year-end 2022, from World Bank Indicators. This plan is \
             hand-written for the end-to-end demo — Level-1 classification \
             is a future session's work."
                .into(),
        topic_tags: vec![
            Topic::new("demographics")
                .map_err(|e| anyhow!("topic construction: {e}"))?,
            Topic::new("chile").map_err(|e| anyhow!("topic construction: {e}"))?,
        ],
        geographic_scope: vec!["CL".into()],
        historical_window_days: 365,
        expectations: RecordExpectations {
            observation_metrics: vec![MetricExpectation {
                name: "population".into(),
                unit_hint: Some(
                    Unit::new("1").map_err(|e| anyhow!("unit construction: {e}"))?,
                ),
                rationale: "Core demographic indicator for any country-level study.".into(),
            }],
            event_types: vec![EventTypeExpectation {
                event_type: EventType::new("census_published")
                    .map_err(|e| anyhow!("event type construction: {e}"))?,
                rationale: "Census publications are the authoritative population source.".into(),
            }],
            entity_kinds: vec![EntityKindExpectation {
                kind: "statistics_agency".into(),
                exemplars: vec![],
                rationale: "National stats agencies produce the primary numbers.".into(),
            }],
            relation_kinds: vec![RelationKindExpectation {
                kind: "reports_to".into(),
                rationale: "Tracks which agency reports to which ministry.".into(),
            }],
            document_sources: vec![DocumentSourceHint {
                description: "World Bank Indicators API".into(),
                preferred_source_ids: vec!["world_bank_indicators".into()],
            }],
            assertion_guidance: None,
        },
        created_at: Utc
            .with_ymd_and_hms(2026, 4, 22, 0, 0, 0)
            .single()
            .ok_or_else(|| anyhow!("construct timestamp"))?,
    })
}
