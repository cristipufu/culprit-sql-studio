//! Cloud metrics for a connection, read through the person's own CLI sign-in: Amazon CloudWatch with `aws`, Azure Monitor
//! with `az`. Only metadata and metrics are read (`rds:Describe*`, `cloudwatch:GetMetricData`, Resource Graph,
//! `monitor metrics list`); Culprit stores no cloud credentials. Results are cached next to the connection's reports for
//! a few minutes, so the app, the CLI and agents share one fetch.

pub(crate) mod aws;
pub(crate) mod azure;
#[cfg(test)]
mod tests;

use crate::{CloudConnection, MetricsCache, MetricsProvider};
use serde_json::Value;
use std::time::Duration;

use culprit_core::changes::{epoch, short_time};
use culprit_core::cloud::{CloudProvider, CloudSeries, CloudSignals, ThrottleEpisode, summarize};
use culprit_core::{Engine, Error, ErrorCode, Result, format, t};
use serde::{Deserialize, Serialize};

use culprit_core::cloud::aws as aws_cli;

/// Metrics fetched within this long are reused.
const FRESH_SECONDS: f64 = 300.0;
/// What a resource is (class, storage, tier) changes rarely; look it up again after a day.
const RESOLVE_SECONDS: f64 = 86_400.0;
/// CloudWatch keeps 1-minute points for 15 days; never ask for more than 14.
const MAX_DAYS: f64 = 30.0;
const CLI_TIMEOUT: Duration = Duration::from_secs(25);

/// Where a connection lives in the cloud, from its host name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Target {
    /// An RDS instance endpoint, or an Aurora cluster endpoint (`reader` for `cluster-ro-`).
    Rds {
        region: String,
        id: String,
        cluster: bool,
        reader: bool,
    },
    AzureSql {
        server: String,
        database: String,
    },
    AzureFlexible {
        engine: Engine,
        server: String,
    },
}

/// The connection's cloud resource, when its host is one Culprit can read metrics for.
pub fn target(info: &CloudConnection) -> Option<Target> {
    let host = info.host.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let first = labels.first().copied().unwrap_or_default();
    if culprit_core::aws_iam_host_allowed(&host) {
        let region = aws_cli::region_from_host(&host)?;
        let second = labels.get(1).copied().unwrap_or_default();
        if second.starts_with("proxy-") || !valid_rds_id(first) {
            return None;
        }
        let cluster = second.starts_with("cluster-");
        return Some(Target::Rds { region, id: first.to_string(), cluster, reader: second.starts_with("cluster-ro-") });
    }
    if !culprit_core::entra_host_allowed(info.engine, &host) || !valid_azure_name(first) {
        return None;
    }
    match info.engine {
        Engine::Mssql if host.contains(".database.") => {
            let database = info.database.clone().filter(|d| valid_azure_database(d))?;
            Some(Target::AzureSql { server: first.to_string(), database })
        }
        Engine::Postgres | Engine::Mysql => {
            Some(Target::AzureFlexible { engine: info.engine, server: first.to_string() })
        }
        _ => None,
    }
}

fn valid_rds_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 63
        && id.starts_with(|c: char| c.is_ascii_alphabetic())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_azure_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Database names are embedded in a Resource Graph query: letters, digits, `_`, `-` and `.` only.
fn valid_azure_database(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// What the cloud says the resource is.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Resource {
    pub(crate) service: String,
    pub(crate) name: String,
    /// RDS: region and instance id. Azure: the resource id.
    pub(crate) region: Option<String>,
    pub(crate) instance: Option<String>,
    pub(crate) azure_id: Option<String>,
    pub(crate) class: Option<String>,
    pub(crate) storage_type: Option<String>,
    pub(crate) allocated_gb: Option<f64>,
    pub(crate) iops: Option<f64>,
    pub(crate) insights: bool,
    pub(crate) sku: Option<String>,
    pub(crate) sku_tier: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    host: String,
    resource: Option<Value>,
    provider: String,
    resolved_at: Option<f64>,
    /// Recent fetches at each resolution, assessed for their original period.
    #[serde(default)]
    fetches: Vec<Fetch>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Fetch {
    pub(crate) signals: CloudSignals,
    pub(crate) at: f64,
    /// The language its words were said in: a fetch in another isn't reused.
    pub(crate) lang: String,
}

fn read_cache(cache: &dyn MetricsCache, host: &str, provider: &str) -> Cache {
    cache
        .read()
        .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
        .filter(|cache| cache.host == host && cache.provider == provider)
        .unwrap_or_else(|| Cache { host: host.into(), provider: provider.into(), ..Default::default() })
}
fn write_cache(store: &dyn MetricsCache, cache: &Cache) {
    if let Ok(bytes) = serde_json::to_vec(cache) {
        store.write(&bytes);
    }
}

fn now() -> f64 {
    chrono::Utc::now().timestamp() as f64
}

pub(crate) fn iso(seconds: f64) -> String {
    chrono::DateTime::from_timestamp(seconds.floor() as i64, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// CloudWatch and Azure report `2026-10-11T12:05:00+00:00` or `…Z`; keep one form so series line up.
pub(crate) fn normalize_time(text: &str) -> Option<String> {
    epoch(text).map(iso)
}

/// Seconds per point: fine enough to see a 10-minute episode, coarse enough to stay under ~400 points.
pub(crate) fn period_for(span: f64) -> u32 {
    match span {
        s if s <= 3.0 * 3600.0 => 60,
        s if s <= 26.0 * 3600.0 => 300,
        s if s <= 8.0 * 86_400.0 => 1800,
        _ => 3600,
    }
}

pub(crate) fn reusable(fetch: &Fetch, start: f64, end: f64, now: f64) -> bool {
    let step = f64::from(fetch.signals.period_seconds);
    let (from, to) = (epoch(&fetch.signals.start).unwrap_or(f64::MAX), epoch(&fetch.signals.end).unwrap_or(0.0));
    let fresh = now - fetch.at < FRESH_SECONDS;
    fetch.lang == culprit_core::i18n::current().code
        && ((to - from) - (end - start)).abs() < 2.0
        && from <= start + step
        && (to >= end - step || fresh)
        && fetch.signals.period_seconds <= period_for(end - start)
}

/// The cloud's view of `info` between `start` and `end` (epoch seconds), from the cache when it is fresh.
pub(crate) async fn signals(
    provider: &dyn MetricsProvider,
    store: &dyn MetricsCache,
    info: &CloudConnection,
    start: f64,
    end: f64,
    refresh: bool,
) -> Result<CloudSignals> {
    let now = now();
    let end = end.min(now);
    let start = start.max(end - MAX_DAYS * 86_400.0).min(end - 600.0);
    let cache_key = format!("{}:{}/{}", info.engine, info.host, info.database.as_deref().unwrap_or(""));
    let mut cache = read_cache(store, &cache_key, provider.name());
    if !refresh {
        // Reuse a fetch that covers the period at least as finely as a fresh one would.
        if let Some(f) = cache.fetches.iter().find(|f| reusable(f, start, end, now)) {
            // Keep its actual recorded range and assessment together; a different selected span needs a new fetch.
            return Ok(f.signals.clone());
        }
    }
    let resource = match (&cache.resource, cache.resolved_at) {
        (Some(r), Some(at)) if !refresh && now - at < RESOLVE_SECONDS => r.clone(),
        _ => {
            let r = timed(provider.resolve(info)).await?;
            cache.resource = Some(r.clone());
            cache.resolved_at = Some(now);
            r
        }
    };
    let signals = timed(provider.read(&resource, start, end)).await?;
    cache.fetches.retain(|f| f.signals.period_seconds != signals.period_seconds);
    cache.fetches.push(Fetch { signals: signals.clone(), at: now, lang: culprit_core::i18n::current().code.clone() });
    write_cache(store, &cache);
    Ok(signals)
}

/// "last 24 h", or "6 h up to Tue 14:00 UTC" for a period in the past: what a sentence says after "the" (the length is
/// written with an abbreviated unit, which the words around it don't agree with).
fn span_words(start: f64, end: f64) -> String {
    let length = format::duration((end - start) * 1000.0);
    if now() - end < 900.0 {
        t!("engine.cloud.span.recent", length = length)
    } else {
        t!("engine.cloud.span.until", length = length, time = short_time(&iso(end)))
    }
}

async fn timed<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(CLI_TIMEOUT, work).await.map_err(|_| {
        Error::new(ErrorCode::Timeout, t!("engine.cloud.timeout")).with_hint(t!("engine.cloud.timeout_hint"))
    })?
}

pub(crate) struct CloudAssessment<'a> {
    pub(crate) provider: CloudProvider,
    pub(crate) resource: &'a Resource,
    pub(crate) tier: Vec<String>,
    pub(crate) series: Vec<CloudSeries>,
    pub(crate) episodes: Vec<ThrottleEpisode>,
    pub(crate) notes: Vec<String>,
    pub(crate) start: f64,
    pub(crate) end: f64,
    pub(crate) period: u32,
    pub(crate) source: &'a str,
}

pub(crate) fn finish(assessment: CloudAssessment<'_>) -> CloudSignals {
    let CloudAssessment { provider, resource, tier, series, mut episodes, mut notes, start, end, period, source } =
        assessment;
    episodes.sort_by(|a, b| a.start.cmp(&b.start));
    let span = span_words(start, end);
    let no_data = series.iter().all(|s| s.points.is_empty());
    if no_data {
        notes.push(t!("engine.cloud.no_points", source = source));
    }
    let minutes = format::duration(f64::from(period) * 1000.0);
    notes.push(t!("engine.cloud.resolution", source = source, period = minutes));
    CloudSignals {
        provider,
        service: resource.service.clone(),
        resource: resource.name.clone(),
        tier: (!tier.is_empty()).then(|| tier.join(" · ")),
        start: iso(start),
        end: iso(end),
        period_seconds: period,
        summary: if no_data { t!("engine.cloud.no_measurements", span = &span) } else { summarize(&episodes, &span) },
        series,
        episodes,
        notes,
        fetched_at: iso(now()),
    }
}
