use culprit_core::cloud::{
    CloudPoint, CloudProvider, CloudSeries, CloudSignals, CloudUnit, Rule, ThrottleEpisode, ThrottleKind, episodes,
    gp2_baseline_iops, rds_cpu_baseline, rds_vcpus, sum_series,
};
use culprit_core::{Error, ErrorCode, Result, format};
use serde_json::Value;

use super::{Resource, finish, iso, normalize_time, period_for};
use crate::aws;

pub(super) async fn resolve_rds(region: &str, id: &str, cluster: bool, reader: bool) -> Result<Resource> {
    let instance = if cluster { cluster_member(region, id, reader).await? } else { id.to_string() };
    let json = aws_json(&[
        "rds",
        "describe-db-instances",
        "--db-instance-identifier",
        &instance,
        "--region",
        region,
        "--output",
        "json",
    ])
    .await?;
    let i = json
        .get("DBInstances")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| Error::not_found(format!("AWS has no RDS instance '{instance}' in {region}")))?;
    let text = |k: &str| i.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let number = |k: &str| i.get(k).and_then(|v| v.as_f64());
    Ok(Resource {
        service: rds_service(text("Engine").as_deref().unwrap_or_default()),
        name: instance.clone(),
        region: Some(region.to_string()),
        instance: Some(instance),
        class: text("DBInstanceClass"),
        storage_type: text("StorageType"),
        allocated_gb: number("AllocatedStorage"),
        iops: number("Iops"),
        insights: i.get("PerformanceInsightsEnabled").and_then(|v| v.as_bool()).unwrap_or(false),
        ..Default::default()
    })
}

fn rds_service(engine: &str) -> String {
    let product = match engine {
        e if e.starts_with("aurora-postgresql") => return "Amazon Aurora PostgreSQL".into(),
        e if e.starts_with("aurora") => return "Amazon Aurora MySQL".into(),
        e if e.starts_with("postgres") => "PostgreSQL",
        e if e.starts_with("mariadb") => "MariaDB",
        e if e.starts_with("mysql") => "MySQL",
        e if e.starts_with("sqlserver") => "SQL Server",
        _ => return "Amazon RDS".into(),
    };
    format!("Amazon RDS for {product}")
}

async fn cluster_member(region: &str, cluster: &str, reader: bool) -> Result<String> {
    let json = aws_json(&[
        "rds",
        "describe-db-clusters",
        "--db-cluster-identifier",
        cluster,
        "--region",
        region,
        "--output",
        "json",
    ])
    .await?;
    let members = json
        .get("DBClusters")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("DBClusterMembers"))
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    members
        .iter()
        .filter(|m| m.get("IsClusterWriter").and_then(|w| w.as_bool()) == Some(!reader))
        .chain(members.iter())
        .find_map(|m| m.get("DBInstanceIdentifier").and_then(|i| i.as_str()).map(str::to_string))
        .ok_or_else(|| Error::not_found(format!("Aurora cluster '{cluster}' has no instances in {region}")))
}

async fn aws_json(args: &[&str]) -> Result<Value> {
    let bytes = aws::run(args).await.map_err(|e| match e.code {
        ErrorCode::PermissionDenied => e.with_hint(
            "The signed-in AWS identity needs rds:DescribeDBInstances, rds:DescribeDBClusters and cloudwatch:GetMetricData",
        ),
        _ => e,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| Error::internal(format!("Unexpected AWS CLI output: {e}")))
}

/// (query id, CloudWatch metric, statistic)
const RDS_METRICS: &[(&str, &str, &str)] = &[
    ("cpu", "CPUUtilization", "Average"),
    ("credits", "CPUCreditBalance", "Minimum"),
    ("surplus", "CPUSurplusCreditBalance", "Maximum"),
    ("burst", "BurstBalance", "Minimum"),
    ("riops", "ReadIOPS", "Average"),
    ("wiops", "WriteIOPS", "Average"),
    ("rlat", "ReadLatency", "Average"),
    ("wlat", "WriteLatency", "Average"),
    ("load", "DBLoad", "Average"),
    ("mem", "FreeableMemory", "Minimum"),
    ("conns", "DatabaseConnections", "Maximum"),
    ("ebsio", "EBSIOBalance%", "Minimum"),
    ("ebsbytes", "EBSByteBalance%", "Minimum"),
    ("storage", "FreeStorageSpace", "Minimum"),
];

pub(super) async fn rds_signals(resource: &Resource, start: f64, end: f64) -> Result<CloudSignals> {
    let (Some(region), Some(instance)) = (&resource.region, &resource.instance) else {
        return Err(Error::internal("RDS resource without a region or instance"));
    };
    let period = period_for(end - start);
    let queries: Vec<Value> = RDS_METRICS
        .iter()
        .map(|(id, metric, stat)| {
            serde_json::json!({
                "Id": id,
                "MetricStat": {
                    "Metric": {
                        "Namespace": "AWS/RDS",
                        "MetricName": metric,
                        "Dimensions": [{ "Name": "DBInstanceIdentifier", "Value": instance }]
                    },
                    "Period": period,
                    "Stat": stat
                },
                "ReturnData": true
            })
        })
        .collect();
    let queries = Value::Array(queries).to_string();
    let (from, to) = (iso(start), iso(end));
    let json = aws_json(&[
        "cloudwatch",
        "get-metric-data",
        "--region",
        region,
        "--start-time",
        &from,
        "--end-time",
        &to,
        "--scan-by",
        "TimestampAscending",
        "--metric-data-queries",
        &queries,
        "--output",
        "json",
    ])
    .await?;
    let mut raw: std::collections::BTreeMap<String, Vec<CloudPoint>> = Default::default();
    for result in json.get("MetricDataResults").and_then(|r| r.as_array()).into_iter().flatten() {
        let id = result.get("Id").and_then(|i| i.as_str()).unwrap_or_default().to_string();
        let times = result.get("Timestamps").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let values = result.get("Values").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut points: Vec<CloudPoint> = times
            .iter()
            .zip(values.iter())
            .filter_map(|(t, v)| Some(CloudPoint { at: normalize_time(t.as_str()?)?, value: v.as_f64()? }))
            .collect();
        points.sort_by(|a, b| a.at.cmp(&b.at));
        raw.insert(id, points);
    }
    Ok(rds_judge(resource, raw, start, end, period))
}

pub(super) fn rds_judge(
    resource: &Resource,
    mut raw: std::collections::BTreeMap<String, Vec<CloudPoint>>,
    start: f64,
    end: f64,
    period: u32,
) -> CloudSignals {
    let mut take = |id: &str| raw.remove(id).unwrap_or_default();
    let class = resource.class.clone().unwrap_or_default();
    let vcpus = rds_vcpus(&class);
    let baseline = rds_cpu_baseline(&class);
    let storage = resource.storage_type.clone().unwrap_or_default();
    let gb = resource.allocated_gb.unwrap_or(0.0);
    let mut series = Vec::new();
    let mut found: Vec<ThrottleEpisode> = Vec::new();

    let cpu = CloudSeries::new("CPUUtilization", "CPU", CloudUnit::Percent, take("cpu"));
    let cpu = match baseline {
        Some(b) => cpu.limit(b, format!("baseline {b:.0}%")),
        None => cpu,
    };
    let saturated = |worst: f64, lasted: &str| {
        format!(
            "CPU was above 90% in the recorded samples over {lasted}, peaking at {worst:.0}%. That leaves little headroom; these metrics alone do not measure query delays."
        )
    };
    found.extend(episodes(
        &cpu,
        period,
        &rule("CPUUtilization", ThrottleKind::CpuLimit, 90.0, false, 3, "CPU use was high", &saturated),
    ));
    series.push(cpu);

    let credits = take("credits");
    let surplus = take("surplus");
    if baseline.is_some() && !credits.is_empty() {
        let unlimited = surplus.iter().any(|p| p.value > 0.0);
        let b = baseline.unwrap_or_default();
        let detail = move |_: f64, lasted: &str| {
            if unlimited {
                format!(
                    "The credit balance was zero in samples over {lasted}, and surplus credits were used. Unlimited mode can continue above the {b:.0}% baseline; these counters alone do not establish query delays."
                )
            } else {
                format!(
                    "The credit balance was zero in samples over {lasted}. In standard mode, demand above its {b:.0}% CPU baseline is limited; unlimited mode can keep bursting. Credit balance alone does not establish longer query times."
                )
            }
        };
        let s = CloudSeries::new("CPUCreditBalance", "CPU credits", CloudUnit::Credits, credits).floor();
        found.extend(episodes(
            &s,
            period,
            &rule("CPUCreditBalance", ThrottleKind::CpuCredits, 0.0, true, 2, "CPU credits ran out", &detail),
        ));
        series.push(s);
    }

    let (read, write) = (take("riops"), take("wiops"));
    let read = CloudSeries::new("ReadIOPS", "Read IOPS", CloudUnit::Iops, read);
    let write = CloudSeries::new("WriteIOPS", "Write IOPS", CloudUnit::Iops, write);
    if let Some(iops) = sum_series("IOPS", "IOPS", CloudUnit::Iops, &[&read, &write]) {
        let provisioned = resource.iops.filter(|i| *i > 0.0 && matches!(storage.as_str(), "gp3" | "io1" | "io2"));
        let iops = match (provisioned, storage.as_str()) {
            (Some(limit), _) => {
                let limit_words = format!("provisioned {} IOPS", format::count(limit));
                let detail = move |worst: f64, lasted: &str| {
                    format!(
                        "Reads and writes reached {} IOPS against the provisioned {} in samples over {lasted}. Near a provisioned limit leaves little headroom; confirm read and write latency before changing capacity.",
                        format::count(worst),
                        format::count(limit)
                    )
                };
                let s = iops.limit(limit, limit_words);
                found.extend(episodes(
                    &s,
                    period,
                    &rule(
                        "IOPS",
                        ThrottleKind::IoLimit,
                        limit * 0.95,
                        false,
                        3,
                        "IOPS were near the provisioned limit",
                        &detail,
                    ),
                ));
                s
            }
            (None, "gp2") if gb > 0.0 => {
                let base = gp2_baseline_iops(gb);
                iops.limit(base, format!("baseline {} IOPS (3 per GB)", format::count(base)))
            }
            _ => iops,
        };
        series.push(iops);
    }

    let burst = take("burst");
    if !burst.is_empty() {
        let base = gp2_baseline_iops(gb);
        let detail = move |_: f64, lasted: &str| {
            format!(
                "The volume's burst balance was depleted in samples over {lasted}; its baseline is {} IOPS. This alone does not establish query delays.",
                format::count(base)
            )
        };
        let s = CloudSeries::new("BurstBalance", "Storage burst balance", CloudUnit::Percent, burst).floor();
        found.extend(episodes(
            &s,
            period,
            &rule("BurstBalance", ThrottleKind::IoCredits, 0.0, true, 2, "Storage burst credits ran out", &detail),
        ));
        series.push(s);
    }

    for (id, metric, label) in
        [("ebsio", "EBSIOBalance%", "EBS IO balance"), ("ebsbytes", "EBSByteBalance%", "EBS throughput balance")]
    {
        let points = take(id);
        if points.is_empty() {
            continue;
        }
        let class_words = class.clone();
        let detail = move |_: f64, lasted: &str| {
            format!(
                "{class_words}'s EBS burst balance was depleted in samples over {lasted}. Demand above the baseline can be limited; compare actual I/O demand and latency before changing capacity."
            )
        };
        let s = CloudSeries::new(metric, label, CloudUnit::Percent, points).floor();
        found.extend(episodes(
            &s,
            period,
            &rule(metric, ThrottleKind::IoCredits, 0.0, true, 2, "EBS burst ran out", &detail),
        ));
        series.push(s);
    }

    let read_lat = CloudSeries::new("ReadLatency", "Read latency", CloudUnit::Ms, scale(take("rlat"), 1000.0));
    let write_lat = CloudSeries::new("WriteLatency", "Write latency", CloudUnit::Ms, scale(take("wlat"), 1000.0));
    if let Some(latency) = max_series("DiskLatency", "Disk latency", CloudUnit::Ms, &read_lat, &write_lat) {
        let detail = |worst: f64, lasted: &str| {
            format!("Reads or writes took {} on average at worst, above 20 ms for {lasted}.", format::duration(worst))
        };
        found.extend(episodes(
            &latency,
            period,
            &rule("DiskLatency", ThrottleKind::DiskLatency, 20.0, false, 3, "Storage answered slowly", &detail),
        ));
        series.push(latency);
    }

    let load = take("load");
    if !load.is_empty() {
        let s = CloudSeries::new("DBLoad", "DB load", CloudUnit::Sessions, load);
        let s = match vcpus {
            Some(v) => {
                let detail = move |worst: f64, lasted: &str| {
                    format!(
                        "The recorded active-session count peaked at {worst:.1} on {v:.0} vCPUs over {lasted}. Active sessions can be running or waiting on many kinds of events; this count alone does not establish a CPU shortage."
                    )
                };
                let s = s.limit(v, format!("{v:.0} vCPUs"));
                found.extend(episodes(
                    &s,
                    period,
                    &rule("DBLoad", ThrottleKind::Load, v * 1.5, false, 3, "More work than vCPUs", &detail),
                ));
                s
            }
            None => s,
        };
        series.push(s);
    }

    let free = take("storage");
    if !free.is_empty() && gb > 0.0 {
        let floor = gb * 1e9 * 0.05;
        let detail = |worst: f64, lasted: &str| {
            format!("Free storage fell to {} for {lasted}; writes fail when it runs out.", format::bytes(worst))
        };
        let s = CloudSeries::new("FreeStorageSpace", "Free storage", CloudUnit::Bytes, free)
            .floor()
            .limit(floor, format!("alert below {} (5% of {gb:.0} GB)", format::bytes(floor)));
        found.extend(episodes(
            &s,
            period,
            &rule("FreeStorageSpace", ThrottleKind::Storage, floor, true, 2, "Storage nearly full", &detail),
        ));
        series.push(s);
    }
    let mem = take("mem");
    if !mem.is_empty() {
        series.push(CloudSeries::new("FreeableMemory", "Freeable memory", CloudUnit::Bytes, mem).floor());
    }
    let conns = take("conns");
    if !conns.is_empty() {
        series.push(CloudSeries::new("DatabaseConnections", "Connections", CloudUnit::Sessions, conns));
    }

    let mut tier = vec![];
    if !class.is_empty() {
        let mut words = class.clone();
        let mut extra = vec![];
        if baseline.is_some() {
            extra.push("burstable".to_string());
        }
        if let Some(v) = vcpus {
            extra.push(format!("{v:.0} vCPUs"));
        }
        if !extra.is_empty() {
            words = format!("{words} ({})", extra.join(", "));
        }
        tier.push(words);
    }
    if !storage.is_empty() {
        let mut words = storage.clone();
        if gb > 0.0 {
            words = format!("{words}, {gb:.0} GB");
        }
        if let Some(iops) = resource.iops.filter(|i| *i > 0.0) {
            words = format!("{words}, {} IOPS", format::count(iops));
        }
        tier.push(words);
    }
    let mut notes = Vec::new();
    if !resource.insights {
        notes.push(
            "Performance Insights is off, so DB load isn't measured; turning it on adds the DBLoad metric.".to_string(),
        );
    }
    finish(CloudProvider::Aws, resource, tier, series, found, notes, start, end, period, "CloudWatch")
}

fn rule<'a>(
    metric: &'a str,
    kind: ThrottleKind,
    threshold: f64,
    floor: bool,
    min_points: usize,
    title: &'a str,
    detail: &'a dyn Fn(f64, &str) -> String,
) -> Rule<'a> {
    Rule { metric, kind, threshold, floor, min_points, title, detail }
}

fn scale(points: Vec<CloudPoint>, factor: f64) -> Vec<CloudPoint> {
    points.into_iter().map(|p| CloudPoint { value: p.value * factor, ..p }).collect()
}

/// The higher of two series at each time (read or write latency, whichever is worse).
fn max_series(metric: &str, label: &str, unit: CloudUnit, a: &CloudSeries, b: &CloudSeries) -> Option<CloudSeries> {
    let mut points: Vec<CloudPoint> = a.points.clone();
    for p in &b.points {
        match points.iter_mut().find(|x| x.at == p.at) {
            Some(x) => x.value = x.value.max(p.value),
            None => points.push(p.clone()),
        }
    }
    points.sort_by(|x, y| x.at.cmp(&y.at));
    (!points.is_empty()).then(|| CloudSeries::new(metric, label, unit, points))
}
