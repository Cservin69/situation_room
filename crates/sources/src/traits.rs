//! The [`Source`] trait — the contract every data adapter implements.
//!
//! A Source knows how to fetch data from somewhere (an API, an RSS feed, a
//! PDF on a government website) and produce records conforming to the core
//! schema. It does *not* know about storage, scheduling, or downstream
//! pipeline stages; those are the orchestrator's concern.
//!
//! Design notes:
//!
//! - Sources are async because nearly all real sources involve I/O.
//! - Sources track a watermark (the most recent `observed_at` they've seen)
//!   so the scheduler can do incremental fetches.
//! - Sources declare metadata (cadence, license, authoritative-for) up front
//!   so the pipeline can route records correctly without inspecting them.
//! - Errors are the source's concern — the trait returns a typed error so
//!   the orchestrator can decide whether to retry, back off, or alert.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use stockpile_core::schema::Record;
use stockpile_core::vocab::Topic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Metadata declared by every Source.
///
/// This is what the registry uses to surface sources to the user, what the
/// scheduler uses to decide cadence, and what the promotion pipeline uses
/// to recognize authoritative claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMetadata {
    /// Stable identifier — slug used in config files, logs, registry.
    pub id: String,
    /// Human-readable name for the UI.
    pub display_name: String,
    /// One-paragraph description of what this source provides.
    pub description: String,
    /// License of the data this source returns. Affects caching policy.
    pub license: DataLicense,
    /// What this source is considered authoritative for, if anything.
    /// Used by `pipeline::promote` to decide when to elevate Assertions.
    pub authoritative_for: Vec<AuthoritativeDomain>,
    /// Suggested fetch cadence (the scheduler may override based on config).
    pub default_cadence: Cadence,
    /// Source homepage / documentation URL.
    pub homepage: Option<String>,
}

/// What a source's data is licensed under. Determines whether we can cache
/// and redistribute it, or must fetch on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataLicense {
    /// Public domain or government work — cache freely, redistribute freely.
    PublicDomain,
    /// Open data license (CC-BY, ODbL, etc.) — cache + redistribute with attribution.
    OpenWithAttribution { license_id: String },
    /// Terms allow caching for personal use only — do not redistribute.
    PersonalUseOnly,
    /// Proprietary preview/snippet — fetch on demand only, never cache full content.
    ProprietaryPreview,
    /// Custom license — see notes.
    Other { notes: String },
}

/// Domain in which a source is considered authoritative.
/// Used by promotion rules to decide which Assertions auto-promote to Observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthoritativeDomain {
    /// Topic this applies to (None = any topic — the source is authoritative
    /// for this metric regardless of subject). Per ADR 0010 the subject axis
    /// is topic-based; there is no commodity dimension in the schema.
    pub topic: Option<Topic>,
    /// Metric this applies to (e.g. "production", "price", "warehouse_stock").
    pub metric: String,
}

/// Suggested fetch cadence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cadence {
    /// Tick every N seconds (real-time data).
    Seconds(u64),
    /// Once per N minutes.
    Minutes(u64),
    /// Once per N hours.
    Hours(u64),
    /// Once per day at HH:MM UTC.
    DailyAt { hour: u8, minute: u8 },
    /// Once per month on day D.
    MonthlyOn { day: u8 },
    /// Once per year on month/day.
    YearlyOn { month: u8, day: u8 },
    /// Manually triggered only — not scheduled.
    OnDemand,
}

/// Result of one fetch attempt.
#[derive(Debug)]
pub struct FetchOutcome {
    /// Records produced by this fetch.
    pub records: Vec<Record>,
    /// New watermark — the most recent `observed_at` the source saw.
    /// The scheduler stores this so the next fetch can be incremental.
    pub new_watermark: Option<DateTime<Utc>>,
    /// Free-form notes the source wants to surface (e.g. "API rate-limited,
    /// only fetched first 100 records").
    pub notes: Vec<String>,
}

/// Parameters passed to each fetch.
#[derive(Debug, Clone)]
pub struct FetchContext {
    /// Last successful watermark, if any. Sources use this for incremental fetches.
    pub since: Option<DateTime<Utc>>,
    /// Topics or subjects the fetch should focus on, if the source supports filtering.
    /// Empty means "everything you'd normally fetch."
    pub focus: Vec<String>,
}

/// Errors a source can return.
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("network error: {0}")]
    Network(String),
    #[error("rate limited; retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("upstream returned malformed data: {0}")]
    MalformedResponse(String),
    #[error("source temporarily unavailable: {0}")]
    Unavailable(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("internal error: {0}")]
    Internal(String),
}

/// The contract every data adapter implements.
#[async_trait]
pub trait Source: Send + Sync {
    /// Static metadata for this source.
    fn metadata(&self) -> SourceMetadata;

    /// Fetch records, optionally incrementally from `ctx.since`.
    async fn fetch(&self, ctx: FetchContext) -> Result<FetchOutcome, SourceError>;

    /// Optional: a cheap health check the registry can run.
    /// Default implementation does nothing and returns Ok.
    async fn health_check(&self) -> Result<(), SourceError> {
        Ok(())
    }
}
