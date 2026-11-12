use crate::AwsCloud;
use culprit_core::cloud::{
    CloudPoint, CloudProvider, CloudSeries, CloudSignals, CloudUnit, Rule, ThrottleEpisode, ThrottleKind, episodes,
    gp2_baseline_iops, rds_cpu_baseline, rds_vcpus, sum_series,
};
use culprit_core::{Error, ErrorCode, Result, format, key, t};
use serde_json::Value;

use super::{CloudAssessment, Resource, finish, iso, normalize_time, period_for};

pub(crate) async fn resolve_rds(
    client: &AwsCloud,
    region: &str,
    id: &str,
    cluster: bool,
    reader: bool,
) -> Result<Resource> {
    let instance = if cluster { cluster_member(client, region, id, reader).await? } else { id.to_string() };
    let json = aws_json(
        client,
        &[
            "rds",
            "describe-db-instances",
            "--db-instance-identifier",
            &instance,
            "--region",
            region,
            "--output",
            "json",
        ],
    )
    .await?;
    let i =
        json.get("DBInstances").and_then(|x| x.as_array()).and_then(|a| a.first()).ok_or_else(|| {
            Error::not_found(t!("engine.cloud.rds.no_instance", instance = &instance, region = region))
        })?;
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

/// The AWS product's name, which no language translates.
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

async fn cluster_member(client: &AwsCloud, region: &str, cluster: &str, reader: bool) -> Result<String> {
    let json = aws_json(
        client,
        &["rds", "describe-db-clusters", "--db-cluster-identifier", cluster, "--region", region, "--output", "json"],
    )
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
        .ok_or_else(|| {
            Error::not_found(t!("engine.cloud.rds.no_cluster_instances", cluster = cluster, region = region))
        })
}

async fn aws_json(client: &AwsCloud, args: &[&str]) -> Result<Value> {
    let bytes = client.commands.run(args).await.map_err(|e| match e.code {
        ErrorCode::PermissionDenied => e.with_hint(t!("engine.cloud.rds.permissions_hint")),
        _ => e,
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::internal(t!("engine.cloud.rds.unexpected_output", error = e.to_string())))
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

pub(crate) async fn rds_signals(client: &AwsCloud, resource: &Resource, start: f64, end: f64) -> Result<CloudSignals> {
    let (Some(region), Some(instance)) = (&resource.region, &resource.instance) else {
        return Err(Error::internal(t!("engine.cloud.rds.incomplete_resource")));
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
    let json = aws_json(
        client,
        &[
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
        ],
    )
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

pub(crate) fn rds_judge(
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

    let cpu = CloudSeries::new("CPUUtilization", &t!("engine.cloud.series.cpu"), CloudUnit::Percent, take("cpu"));
    let cpu = match baseline {
        Some(b) => cpu.baseline(b, t!("engine.cloud.rds.limit.cpu_baseline", percent = whole(b))),
        None => cpu,
    };
    let saturated = |worst: f64, lasted: &str| t!("engine.cloud.rds.detail.cpu", lasted = lasted, worst = whole(worst));
    let title = t!("engine.cloud.episode.cpu_high");
    let said = t!("engine.cloud.episode_said.cpu_high");
    found.extend(episodes(
        &cpu,
        period,
        &rule("CPUUtilization", ThrottleKind::CpuLimit, 90.0, false, 3, (&title, &said), &saturated),
    ));
    series.push(cpu);

    let credits = take("credits");
    let surplus = take("surplus");
    if baseline.is_some() && !credits.is_empty() {
        let unlimited = surplus.iter().any(|p| p.value > 0.0);
        let b = baseline.unwrap_or_default();
        let detail = move |_: f64, lasted: &str| {
            if unlimited {
                t!("engine.cloud.rds.detail.credits_unlimited", lasted = lasted, baseline = whole(b))
            } else {
                t!("engine.cloud.rds.detail.credits_standard", lasted = lasted, baseline = whole(b))
            }
        };
        let s =
            CloudSeries::new("CPUCreditBalance", &t!("engine.cloud.series.cpu_credits"), CloudUnit::Credits, credits)
                .floor();
        let title = t!("engine.cloud.episode.cpu_credits");
        let said = t!("engine.cloud.episode_said.cpu_credits");
        found.extend(episodes(
            &s,
            period,
            &rule("CPUCreditBalance", ThrottleKind::CpuCredits, 0.0, true, 2, (&title, &said), &detail),
        ));
        series.push(s);
    }

    let (read, write) = (take("riops"), take("wiops"));
    let read = CloudSeries::new("ReadIOPS", &t!("engine.cloud.rds.series.read_iops"), CloudUnit::Iops, read);
    let write = CloudSeries::new("WriteIOPS", &t!("engine.cloud.rds.series.write_iops"), CloudUnit::Iops, write);
    if let Some(iops) = sum_series("IOPS", &t!("engine.cloud.series.iops"), CloudUnit::Iops, &[&read, &write]) {
        let provisioned = resource.iops.filter(|i| *i > 0.0 && matches!(storage.as_str(), "gp3" | "io1" | "io2"));
        let iops = match (provisioned, storage.as_str()) {
            (Some(limit), _) => {
                let limit_words = t!("engine.cloud.rds.limit.provisioned_iops", iops = format::count(limit));
                let detail = move |worst: f64, lasted: &str| {
                    t!(
                        "engine.cloud.rds.detail.provisioned_iops",
                        worst = format::count(worst),
                        limit = format::count(limit),
                        lasted = lasted,
                    )
                };
                let s = iops.limit(limit, limit_words);
                let title = t!("engine.cloud.rds.episode.provisioned_iops");
                let said = t!("engine.cloud.rds.episode_said.provisioned_iops");
                found.extend(episodes(
                    &s,
                    period,
                    &rule("IOPS", ThrottleKind::IoLimit, limit * 0.95, false, 3, (&title, &said), &detail),
                ));
                s
            }
            (None, "gp2") if gb > 0.0 => {
                let base = gp2_baseline_iops(gb);
                iops.baseline(base, t!("engine.cloud.rds.limit.gp2_baseline", iops = format::count(base)))
            }
            _ => iops,
        };
        series.push(iops);
    }

    let burst = take("burst");
    if !burst.is_empty() {
        let base = gp2_baseline_iops(gb);
        let detail = move |_: f64, lasted: &str| {
            t!("engine.cloud.rds.detail.burst", lasted = lasted, iops = format::count(base))
        };
        let label = t!("engine.cloud.rds.series.burst_balance");
        let s = CloudSeries::new("BurstBalance", &label, CloudUnit::Percent, burst).floor();
        let title = t!("engine.cloud.rds.episode.burst");
        let said = t!("engine.cloud.rds.episode_said.burst");
        found.extend(episodes(
            &s,
            period,
            &rule("BurstBalance", ThrottleKind::IoCredits, 0.0, true, 2, (&title, &said), &detail),
        ));
        series.push(s);
    }

    for (id, metric, label) in [
        ("ebsio", "EBSIOBalance%", key!("engine.cloud.rds.series.ebs_io_balance")),
        ("ebsbytes", "EBSByteBalance%", key!("engine.cloud.rds.series.ebs_throughput_balance")),
    ] {
        let points = take(id);
        if points.is_empty() {
            continue;
        }
        let class_words = class.clone();
        let detail =
            move |_: f64, lasted: &str| t!("engine.cloud.rds.detail.ebs_burst", class = &class_words, lasted = lasted);
        let s = CloudSeries::new(metric, &culprit_core::i18n::t(label, &[]), CloudUnit::Percent, points).floor();
        let title = t!("engine.cloud.rds.episode.ebs_burst");
        let said = t!("engine.cloud.rds.episode_said.ebs_burst");
        found.extend(episodes(
            &s,
            period,
            &rule(metric, ThrottleKind::IoCredits, 0.0, true, 2, (&title, &said), &detail),
        ));
        series.push(s);
    }

    let read_lat = CloudSeries::new(
        "ReadLatency",
        &t!("engine.cloud.rds.series.read_latency"),
        CloudUnit::Ms,
        scale(take("rlat"), 1000.0),
    );
    let write_lat = CloudSeries::new(
        "WriteLatency",
        &t!("engine.cloud.rds.series.write_latency"),
        CloudUnit::Ms,
        scale(take("wlat"), 1000.0),
    );
    let label = t!("engine.cloud.rds.series.disk_latency");
    if let Some(latency) = max_series("DiskLatency", &label, CloudUnit::Ms, &read_lat, &write_lat) {
        let detail = |worst: f64, lasted: &str| {
            t!("engine.cloud.rds.detail.disk_latency", worst = format::duration(worst), lasted = lasted)
        };
        let title = t!("engine.cloud.rds.episode.disk_latency");
        let said = t!("engine.cloud.rds.episode_said.disk_latency");
        found.extend(episodes(
            &latency,
            period,
            &rule("DiskLatency", ThrottleKind::DiskLatency, 20.0, false, 3, (&title, &said), &detail),
        ));
        series.push(latency);
    }

    let load = take("load");
    if !load.is_empty() {
        let s = CloudSeries::new("DBLoad", &t!("engine.cloud.rds.series.db_load"), CloudUnit::Sessions, load);
        let s = match vcpus {
            Some(v) => {
                let detail = move |worst: f64, lasted: &str| {
                    t!(
                        "engine.cloud.rds.detail.load",
                        worst = format::digits(format!("{worst:.1}")),
                        n = v,
                        count = whole(v),
                        lasted = lasted,
                    )
                };
                let s = s.limit(v, t!("engine.cloud.rds.limit.vcpus", n = v, count = whole(v)));
                let title = t!("engine.cloud.rds.episode.load");
                let said = t!("engine.cloud.rds.episode_said.load");
                found.extend(episodes(
                    &s,
                    period,
                    &rule("DBLoad", ThrottleKind::Load, v * 1.5, false, 3, (&title, &said), &detail),
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
            t!("engine.cloud.rds.detail.free_storage", worst = format::bytes(worst), lasted = lasted)
        };
        let s =
            CloudSeries::new("FreeStorageSpace", &t!("engine.cloud.rds.series.free_storage"), CloudUnit::Bytes, free)
                .floor()
                .limit(floor, t!("engine.cloud.rds.limit.free_storage", floor = format::bytes(floor), gb = whole(gb)));
        let title = t!("engine.cloud.episode.storage_full");
        let said = t!("engine.cloud.episode_said.storage_full");
        found.extend(episodes(
            &s,
            period,
            &rule("FreeStorageSpace", ThrottleKind::Storage, floor, true, 2, (&title, &said), &detail),
        ));
        series.push(s);
    }
    let mem = take("mem");
    if !mem.is_empty() {
        let label = t!("engine.cloud.rds.series.freeable_memory");
        series.push(CloudSeries::new("FreeableMemory", &label, CloudUnit::Bytes, mem).floor());
    }
    let conns = take("conns");
    if !conns.is_empty() {
        let label = t!("engine.cloud.series.connections");
        series.push(CloudSeries::new("DatabaseConnections", &label, CloudUnit::Sessions, conns));
    }

    let mut tier = vec![];
    if !class.is_empty() {
        tier.push(match (baseline.is_some(), vcpus) {
            (true, Some(v)) => t!("engine.cloud.rds.tier.burstable_vcpus", class = &class, n = v, count = whole(v)),
            (true, None) => t!("engine.cloud.rds.tier.burstable", class = &class),
            (false, Some(v)) => t!("engine.cloud.rds.tier.vcpus", class = &class, n = v, count = whole(v)),
            (false, None) => class.clone(),
        });
    }
    if !storage.is_empty() {
        let size = (gb > 0.0).then(|| whole(gb));
        tier.push(match (size, resource.iops.filter(|i| *i > 0.0).map(format::count)) {
            (Some(gb), Some(iops)) => {
                t!("engine.cloud.rds.tier.storage_size_iops", storage = &storage, gb = gb, iops = iops)
            }
            (Some(gb), None) => t!("engine.cloud.rds.tier.storage_size", storage = &storage, gb = gb),
            (None, Some(iops)) => t!("engine.cloud.rds.tier.storage_iops", storage = &storage, iops = iops),
            (None, None) => storage.clone(),
        });
    }
    let mut notes = Vec::new();
    if !resource.insights {
        notes.push(t!("engine.cloud.rds.note.no_insights"));
    }
    finish(CloudAssessment {
        provider: CloudProvider::Aws,
        resource,
        tier,
        series,
        episodes: found,
        notes,
        start,
        end,
        period,
        // A product's name, which no language translates.
        source: "CloudWatch",
    })
}

/// `value` rounded to a whole number, in the current language's digits.
fn whole(value: f64) -> String {
    format::digits(format!("{value:.0}"))
}

fn rule<'a>(
    metric: &'a str,
    kind: ThrottleKind,
    threshold: f64,
    floor: bool,
    min_points: usize,
    // The episode's title, and how a summary lists it.
    (title, said): (&'a str, &'a str),
    detail: &'a dyn Fn(f64, &str) -> String,
) -> Rule<'a> {
    Rule { metric, kind, threshold, floor, min_points, title, said, detail }
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
