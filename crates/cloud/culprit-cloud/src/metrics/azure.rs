use crate::AzureCloud;
use culprit_core::cloud::{
    CloudPoint, CloudProvider, CloudSeries, CloudSignals, CloudUnit, Rule, ThrottleKind, episodes,
};
use culprit_core::{Engine, Error, ErrorCode, Result, format, key, t};
use serde::Deserialize;
use serde_json::Value;

use super::{CloudAssessment, Resource, Target, finish, iso, normalize_time, period_for};
use crate::discovery as discover;

pub(crate) async fn resolve_azure_sql(client: &AzureCloud, server: &str, database: &str) -> Result<Resource> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        name: String,
        sku: Option<String>,
        tier: Option<String>,
    }
    let query = format!(
        "resources | where type =~ 'microsoft.sql/servers/databases' and name =~ '{database}' \
         and tostring(split(id, '/')[8]) =~ '{server}' \
         | project id, name, sku = tostring(sku.name), tier = tostring(sku.tier)"
    );
    let row = discover::resource_graph::<Row>(client, &query).await?.into_iter().next().ok_or_else(|| {
        Error::not_found(t!("engine.cloud.azure.no_database", database = database, server = server))
            .with_hint(t!("engine.cloud.azure.sign_in_hint"))
    })?;
    Ok(Resource {
        // Product names, which no language translates.
        service: "Azure SQL Database".into(),
        name: format!("{server}/{}", row.name),
        azure_id: Some(row.id),
        sku: row.sku.filter(|s| !s.is_empty()),
        sku_tier: row.tier.filter(|s| !s.is_empty()),
        ..Default::default()
    })
}

pub(crate) async fn resolve_azure_flexible(client: &AzureCloud, engine: Engine, server: &str) -> Result<Resource> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        name: String,
        sku: Option<String>,
        tier: Option<String>,
        iops: Option<f64>,
        size: Option<f64>,
    }
    let kind = if engine == Engine::Postgres { "dbforpostgresql" } else { "dbformysql" };
    let query = format!(
        "resources | where type =~ 'microsoft.{kind}/flexibleservers' and name =~ '{server}' \
         | project id, name, sku = tostring(sku.name), tier = tostring(sku.tier), \
           iops = todouble(properties.storage.iops), size = todouble(properties.storage.storageSizeGB)"
    );
    let row = discover::resource_graph::<Row>(client, &query).await?.into_iter().next().ok_or_else(|| {
        Error::not_found(t!("engine.cloud.azure.no_flexible_server", server = server))
            .with_hint(t!("engine.cloud.azure.sign_in_hint"))
    })?;
    Ok(Resource {
        service: format!(
            "Azure Database for {} flexible server",
            if engine == Engine::Postgres { "PostgreSQL" } else { "MySQL" }
        ),
        name: row.name,
        azure_id: Some(row.id),
        sku: row.sku.filter(|s| !s.is_empty()),
        sku_tier: row.tier.filter(|s| !s.is_empty()),
        iops: row.iops,
        allocated_gb: row.size,
        ..Default::default()
    })
}

fn az_interval(period: u32) -> &'static str {
    match period {
        60 => "PT1M",
        300 => "PT5M",
        1800 => "PT30M",
        _ => "PT1H",
    }
}

/// (metric, its label's message key, unit) per resource kind.
fn azure_metrics(target: &Target) -> &'static [(&'static str, &'static str, CloudUnit)] {
    match target {
        Target::AzureSql { .. } => &[
            ("cpu_percent", key!("engine.cloud.series.cpu"), CloudUnit::Percent),
            ("physical_data_read_percent", key!("engine.cloud.azure.series.data_io"), CloudUnit::Percent),
            ("log_write_percent", key!("engine.cloud.azure.series.log_writes"), CloudUnit::Percent),
            ("workers_percent", key!("engine.cloud.azure.series.workers"), CloudUnit::Percent),
            ("sessions_percent", key!("engine.cloud.azure.series.sessions"), CloudUnit::Percent),
        ],
        Target::AzureFlexible { engine: Engine::Postgres, .. } => &[
            ("cpu_percent", key!("engine.cloud.series.cpu"), CloudUnit::Percent),
            ("memory_percent", key!("engine.cloud.azure.series.memory"), CloudUnit::Percent),
            ("disk_iops_consumed_percentage", key!("engine.cloud.azure.series.iops_used"), CloudUnit::Percent),
            ("cpu_credits_remaining", key!("engine.cloud.series.cpu_credits"), CloudUnit::Credits),
            ("storage_percent", key!("engine.cloud.azure.series.storage_used"), CloudUnit::Percent),
            ("active_connections", key!("engine.cloud.series.connections"), CloudUnit::Sessions),
        ],
        _ => &[
            ("cpu_percent", key!("engine.cloud.series.cpu"), CloudUnit::Percent),
            ("memory_percent", key!("engine.cloud.azure.series.memory"), CloudUnit::Percent),
            ("io_consumption_percent", key!("engine.cloud.azure.series.io_used"), CloudUnit::Percent),
            ("cpu_credits_remaining", key!("engine.cloud.series.cpu_credits"), CloudUnit::Credits),
            ("storage_percent", key!("engine.cloud.azure.series.storage_used"), CloudUnit::Percent),
            ("active_connections", key!("engine.cloud.series.connections"), CloudUnit::Sessions),
        ],
    }
}

async fn az_metrics(
    client: &AzureCloud,
    id: &str,
    metrics: &[&str],
    start: f64,
    end: f64,
    period: u32,
) -> Result<std::process::Output> {
    let (from, to) = (iso(start), iso(end));
    let mut args: Vec<&str> = vec!["monitor", "metrics", "list", "--resource", id, "--metrics"];
    args.extend(metrics);
    args.extend([
        "--start-time",
        &from,
        "--end-time",
        &to,
        "--interval",
        az_interval(period),
        "--aggregation",
        "Average",
        "--output",
        "json",
    ]);
    client.commands.output(&args, &[]).await.map_err(|error| {
        if error.code == ErrorCode::NotFound {
            Error::new(ErrorCode::AuthFailed, t!("engine.azure_cli.missing")).with_hint(t!("engine.azure_cli.install"))
        } else {
            error
        }
    })
}

fn parse_az_metrics(output: std::process::Output) -> Result<Value> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = if stderr.contains("az login") || stderr.contains("AADSTS") {
            ErrorCode::AuthFailed
        } else if stderr.contains("AuthorizationFailed") {
            ErrorCode::PermissionDenied
        } else {
            ErrorCode::QueryFailed
        };
        return Err(Error::new(code, t!("engine.cloud.azure.monitor_failed", error = &stderr))
            .with_hint(t!("engine.cloud.azure.monitor_hint")));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::internal(t!("engine.cloud.azure.unexpected_output", error = e.to_string())))
}

pub(crate) async fn azure_signals(
    client: &AzureCloud,
    target: &Target,
    resource: &Resource,
    start: f64,
    end: f64,
) -> Result<CloudSignals> {
    let id = resource.azure_id.clone().ok_or_else(|| Error::internal(t!("engine.cloud.azure.no_id")))?;
    let period = period_for(end - start);
    let wanted = azure_metrics(target);
    let names: Vec<&str> = wanted.iter().map(|(m, _, _)| *m).collect();
    // A metric this resource doesn't publish fails the whole call; fall back to the ones every tier has.
    let mut notes = Vec::new();
    // Process failures (including output limits) bypass the fallback for server-rejected metric names.
    let json = match parse_az_metrics(az_metrics(client, &id, &names, start, end, period).await?) {
        Ok(json) => json,
        Err(e) if e.code == ErrorCode::QueryFailed => {
            notes.push(t!("engine.cloud.azure.note.basic_metrics"));
            let basic: Vec<&str> = names
                .iter()
                .copied()
                .filter(|m| matches!(*m, "cpu_percent" | "memory_percent" | "storage_percent"))
                .collect();
            parse_az_metrics(az_metrics(client, &id, &basic, start, end, period).await?)?
        }
        Err(e) => return Err(e),
    };
    let mut raw: std::collections::BTreeMap<String, Vec<CloudPoint>> = Default::default();
    for metric in json.get("value").and_then(|v| v.as_array()).into_iter().flatten() {
        let name = metric.get("name").and_then(|n| n.get("value")).and_then(|v| v.as_str()).unwrap_or_default();
        let mut points: Vec<CloudPoint> = metric
            .get("timeseries")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
            .flat_map(|ts| ts.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default())
            .filter_map(|d| {
                Some(CloudPoint {
                    at: normalize_time(d.get("timeStamp")?.as_str()?)?,
                    value: d.get("average")?.as_f64()?,
                })
            })
            .collect();
        points.sort_by(|a, b| a.at.cmp(&b.at));
        raw.insert(name.to_string(), points);
    }
    Ok(azure_judge(target, resource, raw, notes, start, end, period))
}

/// How an Azure metric is judged: the kind of episode, the threshold, whether lower is worse, the title (and how a
/// summary lists it) and the sentence.
type Judged = (ThrottleKind, f64, bool, (String, String), Box<dyn Fn(f64, &str) -> String>);

pub(crate) fn azure_judge(
    target: &Target,
    resource: &Resource,
    mut raw: std::collections::BTreeMap<String, Vec<CloudPoint>>,
    notes: Vec<String>,
    start: f64,
    end: f64,
    period: u32,
) -> CloudSignals {
    let sku = resource.sku.clone().unwrap_or_else(|| t!("engine.cloud.azure.this_tier"));
    let burstable = resource.sku_tier.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("burstable"));
    let mut series = Vec::new();
    let mut found = Vec::new();
    for (metric, label, unit) in azure_metrics(target) {
        let points = raw.remove(*metric).unwrap_or_default();
        if points.is_empty() {
            continue;
        }
        let mut s = CloudSeries::new(metric, &culprit_core::i18n::t(label, &[]), *unit, points);
        let sku_words = sku.clone();
        // The sentence `key` says of a metric near its tier's limit.
        let limit_detail = move |key: &'static str| {
            let sku_words = sku_words.clone();
            move |worst: f64, lasted: &str| {
                culprit_core::i18n::t(
                    key,
                    &[
                        ("worst", format::digits(format!("{worst:.0}")).into()),
                        ("sku", sku_words.as_str().into()),
                        ("lasted", lasted.into()),
                    ],
                )
            }
        };
        let judged: Option<Judged> = match (*metric, target) {
            ("cpu_percent", Target::AzureSql { .. }) => Some((
                ThrottleKind::CpuLimit,
                95.0,
                false,
                (t!("engine.cloud.azure.episode.cpu_limit"), t!("engine.cloud.azure.episode_said.cpu_limit")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.cpu"))),
            )),
            ("cpu_percent", _) => Some((
                ThrottleKind::CpuLimit,
                95.0,
                false,
                (t!("engine.cloud.episode.cpu_high"), t!("engine.cloud.episode_said.cpu_high")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.cpu"))),
            )),
            ("physical_data_read_percent", _) => Some((
                ThrottleKind::IoLimit,
                95.0,
                false,
                (t!("engine.cloud.azure.episode.data_io"), t!("engine.cloud.azure.episode_said.data_io")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.data_io"))),
            )),
            ("log_write_percent", _) => Some((
                ThrottleKind::LogRate,
                95.0,
                false,
                (t!("engine.cloud.azure.episode.log_writes"), t!("engine.cloud.azure.episode_said.log_writes")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.log_writes"))),
            )),
            ("workers_percent", _) => Some((
                ThrottleKind::Workers,
                90.0,
                false,
                (t!("engine.cloud.azure.episode.workers"), t!("engine.cloud.azure.episode_said.workers")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.workers"))),
            )),
            ("sessions_percent", _) => Some((
                ThrottleKind::Workers,
                90.0,
                false,
                (t!("engine.cloud.azure.episode.sessions"), t!("engine.cloud.azure.episode_said.sessions")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.sessions"))),
            )),
            ("disk_iops_consumed_percentage" | "io_consumption_percent", _) => Some((
                ThrottleKind::IoLimit,
                95.0,
                false,
                (t!("engine.cloud.azure.episode.iops"), t!("engine.cloud.azure.episode_said.iops")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.iops"))),
            )),
            // Allocated memory can include useful cache; a high percentage alone establishes no shortage.
            ("memory_percent", _) => None,
            ("storage_percent", _) => Some((
                ThrottleKind::Storage,
                95.0,
                false,
                (t!("engine.cloud.episode.storage_full"), t!("engine.cloud.episode_said.storage_full")),
                Box::new(limit_detail(key!("engine.cloud.azure.detail.storage"))),
            )),
            ("cpu_credits_remaining", _) if burstable => Some((
                ThrottleKind::CpuCredits,
                0.0,
                true,
                (t!("engine.cloud.episode.cpu_credits"), t!("engine.cloud.episode_said.cpu_credits")),
                Box::new(|_: f64, lasted: &str| t!("engine.cloud.azure.detail.credits", lasted = lasted)),
            )),
            _ => None,
        };
        if matches!(unit, CloudUnit::Percent) {
            s = s.limit(100.0, t!("engine.cloud.azure.limit.tier"));
        }
        if *metric == "cpu_credits_remaining" {
            s = s.floor();
            s.limit = None;
            s.limit_label = None;
        }
        if let Some((kind, threshold, floor, (title, said), detail)) = judged {
            found.extend(episodes(
                &s,
                period,
                &Rule { metric, kind, threshold, floor, min_points: 2, title: &title, said: &said, detail: &*detail },
            ));
        }
        series.push(s);
    }
    let tier = [resource.sku.clone(), resource.sku_tier.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let tier = if let Some(iops) = resource.iops.filter(|i| *i > 0.0) {
        vec![tier, t!("engine.cloud.azure.tier.iops", iops = format::count(iops))]
    } else {
        vec![tier]
    };
    let tier: Vec<String> = tier.into_iter().filter(|t| !t.is_empty()).collect();
    finish(CloudAssessment {
        provider: CloudProvider::Azure,
        resource,
        tier,
        series,
        episodes: found,
        notes,
        start,
        end,
        period,
        // A product's name, which no language translates.
        source: "Azure Monitor",
    })
}
