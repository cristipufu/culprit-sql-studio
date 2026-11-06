use culprit_core::cloud::{
    CloudPoint, CloudProvider, CloudSeries, CloudSignals, CloudUnit, Rule, ThrottleKind, episodes,
};
use culprit_core::{Engine, Error, ErrorCode, Result, format};
use serde::Deserialize;
use serde_json::Value;

use super::{CloudAssessment, Resource, Target, finish, iso, normalize_time, period_for};
use crate::discover;

pub(super) async fn resolve_azure_sql(server: &str, database: &str) -> Result<Resource> {
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
    let row = discover::resource_graph::<Row>(&query).await?.into_iter().next().ok_or_else(|| {
        Error::not_found(format!("Azure Resource Graph has no database '{database}' on server '{server}'"))
            .with_hint("Sign in with `az login` to the tenant and subscription that hold it")
    })?;
    Ok(Resource {
        service: "Azure SQL Database".into(),
        name: format!("{server}/{}", row.name),
        azure_id: Some(row.id),
        sku: row.sku.filter(|s| !s.is_empty()),
        sku_tier: row.tier.filter(|s| !s.is_empty()),
        ..Default::default()
    })
}

pub(super) async fn resolve_azure_flexible(engine: Engine, server: &str) -> Result<Resource> {
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
    let row = discover::resource_graph::<Row>(&query).await?.into_iter().next().ok_or_else(|| {
        Error::not_found(format!("Azure Resource Graph has no flexible server '{server}'"))
            .with_hint("Sign in with `az login` to the tenant and subscription that hold it")
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

/// (metric, label, unit) per resource kind.
fn azure_metrics(target: &Target) -> &'static [(&'static str, &'static str, CloudUnit)] {
    match target {
        Target::AzureSql { .. } => &[
            ("cpu_percent", "CPU", CloudUnit::Percent),
            ("physical_data_read_percent", "Data IO", CloudUnit::Percent),
            ("log_write_percent", "Log writes", CloudUnit::Percent),
            ("workers_percent", "Workers", CloudUnit::Percent),
            ("sessions_percent", "Sessions", CloudUnit::Percent),
        ],
        Target::AzureFlexible { engine: Engine::Postgres, .. } => &[
            ("cpu_percent", "CPU", CloudUnit::Percent),
            ("memory_percent", "Memory", CloudUnit::Percent),
            ("disk_iops_consumed_percentage", "IOPS used", CloudUnit::Percent),
            ("cpu_credits_remaining", "CPU credits", CloudUnit::Credits),
            ("storage_percent", "Storage used", CloudUnit::Percent),
            ("active_connections", "Connections", CloudUnit::Sessions),
        ],
        _ => &[
            ("cpu_percent", "CPU", CloudUnit::Percent),
            ("memory_percent", "Memory", CloudUnit::Percent),
            ("io_consumption_percent", "IO used", CloudUnit::Percent),
            ("cpu_credits_remaining", "CPU credits", CloudUnit::Credits),
            ("storage_percent", "Storage used", CloudUnit::Percent),
            ("active_connections", "Connections", CloudUnit::Sessions),
        ],
    }
}

async fn az_metrics(id: &str, metrics: &[&str], start: f64, end: f64, period: u32) -> Result<std::process::Output> {
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
    let mut command = crate::program::command(crate::program::Cli::Azure)?;
    command.args(&args);
    crate::cloud_process::output(command).await.map_err(|error| {
        if error.code == ErrorCode::NotFound {
            Error::new(ErrorCode::AuthFailed, "The Azure CLI was not found in PATH or standard install locations")
                .with_hint("Install the Azure CLI and run `az login`")
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
        return Err(Error::new(code, format!("Azure Monitor query failed: {stderr}")).with_hint(
            "Run `az login`; reading metrics needs the Monitoring Reader role (or Reader) on the resource",
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| Error::internal(format!("Unexpected Azure CLI output: {e}")))
}

pub(super) async fn azure_signals(target: &Target, resource: &Resource, start: f64, end: f64) -> Result<CloudSignals> {
    let id = resource.azure_id.clone().ok_or_else(|| Error::internal("Azure resource without an id"))?;
    let period = period_for(end - start);
    let wanted = azure_metrics(target);
    let names: Vec<&str> = wanted.iter().map(|(m, _, _)| *m).collect();
    // A metric this resource doesn't publish fails the whole call; fall back to the ones every tier has.
    let mut notes = Vec::new();
    // Process failures (including output limits) bypass the fallback for server-rejected metric names.
    let json = match parse_az_metrics(az_metrics(&id, &names, start, end, period).await?) {
        Ok(json) => json,
        Err(e) if e.code == ErrorCode::QueryFailed => {
            notes.push("Some metrics aren't published for this tier; showing CPU, memory and storage.".to_string());
            let basic: Vec<&str> = names
                .iter()
                .copied()
                .filter(|m| matches!(*m, "cpu_percent" | "memory_percent" | "storage_percent"))
                .collect();
            parse_az_metrics(az_metrics(&id, &basic, start, end, period).await?)?
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

/// How an Azure metric is judged: the kind of episode, the threshold, whether lower is worse, the title and the sentence.
type Judged = (ThrottleKind, f64, bool, &'static str, Box<dyn Fn(f64, &str) -> String>);

pub(super) fn azure_judge(
    target: &Target,
    resource: &Resource,
    mut raw: std::collections::BTreeMap<String, Vec<CloudPoint>>,
    notes: Vec<String>,
    start: f64,
    end: f64,
    period: u32,
) -> CloudSignals {
    let sku = resource.sku.clone().unwrap_or_else(|| "this tier".to_string());
    let burstable = resource.sku_tier.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("burstable"));
    let mut series = Vec::new();
    let mut found = Vec::new();
    for (metric, label, unit) in azure_metrics(target) {
        let points = raw.remove(*metric).unwrap_or_default();
        if points.is_empty() {
            continue;
        }
        let mut s = CloudSeries::new(metric, label, *unit, points);
        let sku_words = sku.clone();
        let limit_detail = move |what: &'static str, waits: &'static str| {
            let sku_words = sku_words.clone();
            move |worst: f64, lasted: &str| {
                format!(
                    "{what} reached {worst:.0}% of {sku_words}'s limit in recorded samples over {lasted}. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage{waits}."
                )
            }
        };
        let judged: Option<Judged> = match (*metric, target) {
            ("cpu_percent", Target::AzureSql { .. }) => Some((
                ThrottleKind::CpuLimit,
                95.0,
                false,
                "CPU was near the tier's limit",
                Box::new(limit_detail("CPU", "")),
            )),
            ("cpu_percent", _) => {
                Some((ThrottleKind::CpuLimit, 95.0, false, "CPU use was high", Box::new(limit_detail("CPU", ""))))
            }
            ("physical_data_read_percent", _) => Some((
                ThrottleKind::IoLimit,
                95.0,
                false,
                "Data IO was near the tier's limit",
                Box::new(limit_detail("Data IO", "")),
            )),
            ("log_write_percent", _) => Some((
                ThrottleKind::LogRate,
                95.0,
                false,
                "Log writes were near the tier's limit",
                Box::new(limit_detail("Log writes", "")),
            )),
            ("workers_percent", _) => Some((
                ThrottleKind::Workers,
                90.0,
                false,
                "Worker use neared the tier's limit",
                Box::new(limit_detail("Workers in use", "; requests can be refused at the cap")),
            )),
            ("sessions_percent", _) => Some((
                ThrottleKind::Workers,
                90.0,
                false,
                "Sessions neared the tier's limit",
                Box::new(limit_detail("Sessions", "; connections can be refused at 100%")),
            )),
            ("disk_iops_consumed_percentage" | "io_consumption_percent", _) => Some((
                ThrottleKind::IoLimit,
                95.0,
                false,
                "Storage IOPS were near their limit",
                Box::new(limit_detail("IOPS", "")),
            )),
            // Allocated memory can include useful cache; a high percentage alone establishes no shortage.
            ("memory_percent", _) => None,
            ("storage_percent", _) => Some((
                ThrottleKind::Storage,
                95.0,
                false,
                "Storage nearly full",
                Box::new(limit_detail("Storage used", "")),
            )),
            ("cpu_credits_remaining", _) if burstable => Some((
                ThrottleKind::CpuCredits,
                0.0,
                true,
                "CPU credits ran out",
                Box::new(|_: f64, lasted: &str| {
                    format!(
                        "The Burstable server's CPU-credit balance was zero in samples over {lasted}. Demand above baseline can be limited; these counters alone do not measure query delays."
                    )
                }),
            )),
            _ => None,
        };
        if matches!(unit, CloudUnit::Percent) {
            s = s.limit(100.0, "tier limit");
        }
        if *metric == "cpu_credits_remaining" {
            s = s.floor();
            s.limit = None;
            s.limit_label = None;
        }
        if let Some((kind, threshold, floor, title, detail)) = judged {
            found.extend(episodes(
                &s,
                period,
                &Rule { metric, kind, threshold, floor, min_points: 2, title, detail: &*detail },
            ));
        }
        series.push(s);
    }
    let tier = [resource.sku.clone(), resource.sku_tier.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let tier = if let Some(iops) = resource.iops.filter(|i| *i > 0.0) {
        vec![tier, format!("{} IOPS", format::count(iops))]
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
        source: "Azure Monitor",
    })
}
