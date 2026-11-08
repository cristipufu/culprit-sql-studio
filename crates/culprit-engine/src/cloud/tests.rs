use super::aws::rds_judge;
use super::azure::azure_judge;
use super::*;
use culprit_core::cloud::{CloudPoint, ThrottleKind};
use culprit_core::{AuthKind, Environment};

fn info(engine: Engine, host: &str, database: Option<&str>) -> ConnectionInfo {
    ConnectionInfo {
        name: "x".into(),
        engine,
        environment: Environment::Production,
        host: host.into(),
        port: None,
        database: database.map(str::to_string),
        user: None,
        auth: AuthKind::Password,
        has_secret: true,
        display: String::new(),
        last_report: None,
        agent_access: Default::default(),
        query_log: None,
        ssh: None,
    }
}

#[test]
fn a_cached_day_cannot_supply_an_hours_assessment() {
    let start = 1_791_720_000.0;
    let end = start + 86400.0;
    let signals = rds_judge(&Resource::default(), Default::default(), start, end, 300);
    let fetch = Fetch { signals, at: end, lang: "en".into() };
    assert!(reusable(&fetch, start, end, end));
    assert!(!reusable(&fetch, end - 3600.0, end, end));
    assert!(!reusable(&fetch, end - 6.0 * 3600.0, end, end));
    assert!(!reusable(&fetch, start, end + 86400.0, end + 86400.0));
}

// A fetch's words are in the language it was said in: after the person picks another, it is fetched again.
#[test]
fn a_fetch_said_in_another_language_is_not_reused() {
    let start = 1_791_720_000.0;
    let end = start + 86400.0;
    let signals = rds_judge(&Resource::default(), Default::default(), start, end, 300);
    let fetch = Fetch { signals, at: end, lang: "fr".into() };
    assert!(!reusable(&fetch, start, end, end));
}

#[test]
fn hosts_map_to_cloud_resources() {
    assert_eq!(
        target(&info(Engine::Postgres, "shop-db.abc123.us-east-1.rds.amazonaws.com", None)),
        Some(Target::Rds { region: "us-east-1".into(), id: "shop-db".into(), cluster: false, reader: false })
    );
    assert_eq!(
        target(&info(Engine::Mysql, "shop.cluster-ro-abc.eu-central-1.rds.amazonaws.com", None)),
        Some(Target::Rds { region: "eu-central-1".into(), id: "shop".into(), cluster: true, reader: true })
    );
    assert_eq!(target(&info(Engine::Postgres, "p.proxy-abc.us-east-1.rds.amazonaws.com", None)), None);
    assert_eq!(
        target(&info(Engine::Mssql, "contoso.database.windows.net", Some("orders"))),
        Some(Target::AzureSql { server: "contoso".into(), database: "orders".into() })
    );
    assert_eq!(target(&info(Engine::Mssql, "contoso.database.windows.net", Some("o'rders"))), None);
    assert_eq!(
        target(&info(Engine::Postgres, "pg1.postgres.database.azure.com", Some("app"))),
        Some(Target::AzureFlexible { engine: Engine::Postgres, server: "pg1".into() })
    );
    assert_eq!(target(&info(Engine::Postgres, "localhost", None)), None);
    assert_eq!(target(&info(Engine::Postgres, "db.example.com", None)), None);
}

fn points(values: &[f64]) -> Vec<CloudPoint> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| CloudPoint { at: iso(1_791_720_000.0 + i as f64 * 300.0), value: *v })
        .collect()
}

#[test]
fn zero_credit_balance_warns_without_asserting_query_throttling() {
    let resource = Resource {
        service: "Amazon RDS for PostgreSQL".into(),
        name: "shop".into(),
        class: Some("db.t3.small".into()),
        storage_type: Some("gp2".into()),
        allocated_gb: Some(20.0),
        ..Default::default()
    };
    let mut raw = std::collections::BTreeMap::new();
    raw.insert("cpu".to_string(), points(&[30.0, 25.0, 20.0, 20.0, 20.0, 20.0]));
    raw.insert("credits".to_string(), points(&[12.0, 3.0, 0.0, 0.0, 0.0, 0.2]));
    raw.insert("riops".to_string(), points(&[50.0, 60.0, 70.0, 80.0, 90.0, 100.0]));
    raw.insert("wiops".to_string(), points(&[5.0, 5.0, 5.0, 5.0, 5.0, 5.0]));
    let s = rds_judge(&resource, raw, 1_791_720_000.0, 1_791_721_800.0, 300);
    assert_eq!(s.episodes.len(), 1, "{:?}", s.episodes);
    let e = &s.episodes[0];
    assert_eq!(e.kind, ThrottleKind::CpuCredits);
    assert!(!e.ongoing, "a positive credit balance has recovered");
    assert!(e.detail.contains("20% CPU baseline") && e.detail.contains("alone does not establish"), "{}", e.detail);
    assert_eq!(s.tier.as_deref(), Some("db.t3.small (burstable, 2 vCPUs) · gp2, 20 GB"));
    let iops = s.series("IOPS").unwrap();
    assert_eq!(iops.limit, Some(100.0));
    assert_eq!(iops.points[5].value, 105.0);
    assert!(s.summary.contains("CPU credits ran out once"), "{}", s.summary);
    assert!(s.notes.iter().any(|n| n.contains("Performance Insights is off")));
}

#[test]
fn azure_sql_at_its_log_rate_limit() {
    let resource = Resource {
        service: "Azure SQL Database".into(),
        name: "contoso/orders".into(),
        sku: Some("GP_Gen5_2".into()),
        sku_tier: Some("GeneralPurpose".into()),
        ..Default::default()
    };
    let mut raw = std::collections::BTreeMap::new();
    raw.insert("cpu_percent".to_string(), points(&[40.0, 45.0, 50.0]));
    raw.insert("log_write_percent".to_string(), points(&[60.0, 99.0, 100.0]));
    let target = Target::AzureSql { server: "contoso".into(), database: "orders".into() };
    let s = azure_judge(&target, &resource, raw, vec![], 1_791_720_000.0, 1_791_720_900.0, 300);
    assert_eq!(s.episodes.len(), 1);
    assert_eq!(s.episodes[0].kind, ThrottleKind::LogRate);
    assert_eq!(s.episodes[0].status, culprit_core::SignalStatus::Watch);
    assert!(s.episodes[0].detail.contains("not measured by this percentage"), "{}", s.episodes[0].detail);
    assert_eq!(s.series("cpu_percent").unwrap().limit, Some(100.0));
}

#[test]
fn azure_allocated_memory_is_visible_without_asserting_a_shortage() {
    let target = Target::AzureFlexible { engine: Engine::Postgres, server: "shop".into() };
    let mut raw = std::collections::BTreeMap::new();
    raw.insert("memory_percent".to_string(), points(&[100.0, 100.0, 100.0]));
    let s = azure_judge(&target, &Resource::default(), raw, vec![], 1_791_720_000.0, 1_791_720_900.0, 300);
    assert!(s.series("memory_percent").is_some());
    assert!(s.episodes.is_empty(), "a full cache does not establish memory pressure");
}

/// Every word a person reads in cloud signals, in order: series and limit labels, episode titles and sentences, notes,
/// the tier and the summary.
fn words(s: &CloudSignals) -> Vec<String> {
    let mut out = Vec::new();
    for series in &s.series {
        out.push(format!("series {}: {}", series.metric, series.label));
        if let Some(limit) = &series.limit_label {
            out.push(format!("limit {}: {limit}", series.metric));
        }
    }
    for e in &s.episodes {
        out.push(format!("episode {}: {}", e.metric, e.title));
        out.push(format!("detail {}: {}", e.metric, e.detail));
    }
    out.extend(s.notes.iter().map(|n| format!("note: {n}")));
    out.push(format!("tier: {}", s.tier.clone().unwrap_or_default()));
    out.push(format!("summary: {}", s.summary));
    out
}

fn series_of(values: &[(&str, &[f64])]) -> std::collections::BTreeMap<String, Vec<CloudPoint>> {
    values.iter().map(|(id, v)| (id.to_string(), points(v))).collect()
}

/// A provisioned, burstable RDS instance at every limit CloudWatch shows, and two others that hold the remaining words.
fn rds_cases() -> Vec<CloudSignals> {
    let start = 1_791_720_000.0;
    let end = start + 1800.0;
    let busy = Resource {
        service: "Amazon RDS for PostgreSQL".into(),
        name: "shop".into(),
        class: Some("db.t3.small".into()),
        storage_type: Some("gp3".into()),
        allocated_gb: Some(100.0),
        iops: Some(3000.0),
        ..Default::default()
    };
    let busy_raw = series_of(&[
        ("cpu", &[95.0, 96.0, 97.0, 98.0, 40.0, 40.0]),
        ("credits", &[5.0, 0.0, 0.0, 0.0, 1.0, 2.0]),
        ("surplus", &[0.0, 1.0, 1.0, 0.0, 0.0, 0.0]),
        ("riops", &[2900.0, 2900.0, 2900.0, 100.0, 100.0, 100.0]),
        ("wiops", &[100.0, 100.0, 100.0, 0.0, 0.0, 0.0]),
        ("burst", &[0.0, 0.0, 0.0, 50.0, 50.0, 50.0]),
        ("ebsio", &[0.0, 0.0, 10.0, 10.0, 10.0, 10.0]),
        ("ebsbytes", &[50.0, 50.0, 50.0, 50.0, 50.0, 50.0]),
        ("rlat", &[0.03, 0.03, 0.03, 0.001, 0.001, 0.001]),
        ("wlat", &[0.001, 0.001, 0.001, 0.001, 0.001, 0.001]),
        ("load", &[4.0, 4.0, 4.0, 1.0, 1.0, 1.0]),
        ("storage", &[1e9, 1e9, 1e9, 9e9, 9e9, 9e9]),
        ("mem", &[1e9, 1e9, 1e9, 1e9, 1e9, 1e9]),
        ("conns", &[10.0, 10.0, 10.0, 10.0, 10.0, 10.0]),
    ]);
    let standard = Resource {
        class: Some("db.t3.small".into()),
        storage_type: Some("gp2".into()),
        allocated_gb: Some(20.0),
        insights: true,
        ..Default::default()
    };
    let standard_raw = series_of(&[
        ("cpu", &[30.0, 25.0, 20.0]),
        ("credits", &[0.0, 0.0, 0.0]),
        ("riops", &[50.0, 60.0, 70.0]),
        ("wiops", &[5.0, 5.0, 5.0]),
    ]);
    let other = Resource {
        class: Some("db.custom".into()),
        storage_type: Some("io1".into()),
        iops: Some(1000.0),
        insights: true,
        ..Default::default()
    };
    vec![
        rds_judge(&busy, busy_raw, start, end, 300),
        rds_judge(&standard, standard_raw, start, start + 900.0, 300),
        rds_judge(&other, Default::default(), start, end, 300),
        rds_judge(
            &Resource { class: Some("db.r5.large".into()), storage_type: Some("aurora".into()), ..Default::default() },
            Default::default(),
            start,
            end,
            300,
        ),
    ]
}

fn azure_cases() -> Vec<CloudSignals> {
    let start = 1_791_720_000.0;
    let end = start + 900.0;
    let sql = Resource { sku: Some("GP_Gen5_2".into()), sku_tier: Some("GeneralPurpose".into()), ..Default::default() };
    let sql_raw = series_of(&[
        ("cpu_percent", &[96.0, 97.0, 98.0]),
        ("physical_data_read_percent", &[96.0, 97.0, 98.0]),
        ("log_write_percent", &[96.0, 97.0, 98.0]),
        ("workers_percent", &[91.0, 92.0, 93.0]),
        ("sessions_percent", &[91.0, 92.0, 93.0]),
    ]);
    let flexible = Resource {
        sku: Some("Standard_B1ms".into()),
        sku_tier: Some("Burstable".into()),
        iops: Some(640.0),
        ..Default::default()
    };
    let flexible_raw = series_of(&[
        ("cpu_percent", &[96.0, 97.0, 98.0]),
        ("memory_percent", &[100.0, 100.0, 100.0]),
        ("disk_iops_consumed_percentage", &[96.0, 97.0, 98.0]),
        ("cpu_credits_remaining", &[0.0, 0.0, 0.0]),
        ("storage_percent", &[96.0, 97.0, 98.0]),
        ("active_connections", &[5.0, 5.0, 5.0]),
    ]);
    let mysql_raw = series_of(&[("io_consumption_percent", &[96.0, 97.0, 98.0])]);
    vec![
        azure_judge(
            &Target::AzureSql { server: "contoso".into(), database: "orders".into() },
            &sql,
            sql_raw,
            vec![],
            start,
            end,
            300,
        ),
        azure_judge(
            &Target::AzureFlexible { engine: Engine::Postgres, server: "shop".into() },
            &flexible,
            flexible_raw,
            vec![],
            start,
            end,
            300,
        ),
        azure_judge(
            &Target::AzureFlexible { engine: Engine::Mysql, server: "shop".into() },
            &Resource::default(),
            mysql_raw,
            vec![],
            start,
            end,
            300,
        ),
    ]
}

// What a person reads in cloud signals, word for word: these messages come from locales/en.json.
#[test]
fn cloud_signals_say_what_was_measured_in_english() {
    let rds: Vec<Vec<String>> = rds_cases().iter().map(words).collect();
    let expected: Vec<Vec<&str>> = vec![
        vec![
            "series CPUUtilization: CPU",
            "limit CPUUtilization: baseline 20%",
            "series CPUCreditBalance: CPU credits",
            "series IOPS: IOPS",
            "limit IOPS: provisioned 3.0K IOPS",
            "series BurstBalance: Storage burst balance",
            "series EBSIOBalance%: EBS IO balance",
            "series EBSByteBalance%: EBS throughput balance",
            "series DiskLatency: Disk latency",
            "series DBLoad: DB load",
            "limit DBLoad: 2 vCPUs",
            "series FreeStorageSpace: Free storage",
            "limit FreeStorageSpace: alert below 4.7 GB (5% of 100 GB)",
            "series FreeableMemory: Freeable memory",
            "series DatabaseConnections: Connections",
            "episode CPUUtilization: CPU use was high",
            "detail CPUUtilization: CPU was above 90% in the recorded samples over 20 min, peaking at 98%. That leaves little headroom; these metrics alone do not measure query delays.",
            "episode IOPS: IOPS were near the provisioned limit",
            "detail IOPS: Reads and writes reached 3.0K IOPS against the provisioned 3.0K in samples over 15 min. Near a provisioned limit leaves little headroom; confirm read and write latency before changing capacity.",
            "episode BurstBalance: Storage burst credits ran out",
            "detail BurstBalance: The volume's burst balance was depleted in samples over 15 min; its baseline is 300 IOPS. This alone does not establish query delays.",
            "episode EBSIOBalance%: EBS burst ran out",
            "detail EBSIOBalance%: db.t3.small's EBS burst balance was depleted in samples over 10 min. Demand above the baseline can be limited; compare actual I/O demand and latency before changing capacity.",
            "episode DiskLatency: Storage answered slowly",
            "detail DiskLatency: Reads or writes took 30 ms on average at worst, above 20 ms for 15 min.",
            "episode DBLoad: More work than vCPUs",
            "detail DBLoad: The recorded active-session count peaked at 4.0 on 2 vCPUs over 15 min. Active sessions can be running or waiting on many kinds of events; this count alone does not establish a CPU shortage.",
            "episode FreeStorageSpace: Storage nearly full",
            "detail FreeStorageSpace: Free storage fell to 954 MB for 15 min; writes fail when it runs out.",
            "episode CPUCreditBalance: CPU credits ran out",
            "detail CPUCreditBalance: The credit balance was zero in samples over 15 min, and surplus credits were used. Unlimited mode can continue above the 20% baseline; these counters alone do not establish query delays.",
            "note: Performance Insights is off, so DB load isn't measured; turning it on adds the DBLoad metric.",
            "note: From CloudWatch, one point per 5 min.",
            "tier: db.t3.small (burstable, 2 vCPUs) · gp3, 100 GB, 3.0K IOPS",
            "summary: In the 30 min up to Sun 12:30 UTC: CPU use was high once (20 min), IOPS were near the provisioned limit once (15 min), storage burst credits ran out twice (25 min), storage answered slowly once (15 min), more work than vCPUs once (15 min), storage nearly full once (15 min) and CPU credits ran out once (15 min).",
        ],
        vec![
            "series CPUUtilization: CPU",
            "limit CPUUtilization: baseline 20%",
            "series CPUCreditBalance: CPU credits",
            "series IOPS: IOPS",
            "limit IOPS: baseline 100 IOPS (3 per GB)",
            "episode CPUCreditBalance: CPU credits ran out",
            "detail CPUCreditBalance: The credit balance was zero in samples over 15 min. In standard mode, demand above its 20% CPU baseline is limited; unlimited mode can keep bursting. Credit balance alone does not establish longer query times.",
            "note: From CloudWatch, one point per 5 min.",
            "tier: db.t3.small (burstable, 2 vCPUs) · gp2, 20 GB",
            "summary: In the 15 min up to Sun 12:15 UTC: CPU credits ran out once (15 min, still going).",
        ],
        vec![
            "series CPUUtilization: CPU",
            "note: CloudWatch returned no data points for this period.",
            "note: From CloudWatch, one point per 5 min.",
            "tier: db.custom · io1, 1.0K IOPS",
            "summary: No cloud measurements were available for the 30 min up to Sun 12:30 UTC.",
        ],
        vec![
            "series CPUUtilization: CPU",
            "note: Performance Insights is off, so DB load isn't measured; turning it on adds the DBLoad metric.",
            "note: CloudWatch returned no data points for this period.",
            "note: From CloudWatch, one point per 5 min.",
            "tier: db.r5.large (2 vCPUs) · aurora",
            "summary: No cloud measurements were available for the 30 min up to Sun 12:30 UTC.",
        ],
    ];
    assert_eq!(rds, expected);
    let azure: Vec<Vec<String>> = azure_cases().iter().map(words).collect();
    let expected: Vec<Vec<&str>> = vec![
        vec![
            "series cpu_percent: CPU",
            "limit cpu_percent: tier limit",
            "series physical_data_read_percent: Data IO",
            "limit physical_data_read_percent: tier limit",
            "series log_write_percent: Log writes",
            "limit log_write_percent: tier limit",
            "series workers_percent: Workers",
            "limit workers_percent: tier limit",
            "series sessions_percent: Sessions",
            "limit sessions_percent: tier limit",
            "episode cpu_percent: CPU was near the tier's limit",
            "detail cpu_percent: CPU reached 98% of GP_Gen5_2's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "episode physical_data_read_percent: Data IO was near the tier's limit",
            "detail physical_data_read_percent: Data IO reached 98% of GP_Gen5_2's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "episode log_write_percent: Log writes were near the tier's limit",
            "detail log_write_percent: Log writes reached 98% of GP_Gen5_2's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "episode workers_percent: Worker use neared the tier's limit",
            "detail workers_percent: Workers in use reached 93% of GP_Gen5_2's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage; requests can be refused at the cap.",
            "episode sessions_percent: Sessions neared the tier's limit",
            "detail sessions_percent: Sessions reached 93% of GP_Gen5_2's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage; connections can be refused at 100%.",
            "note: From Azure Monitor, one point per 5 min.",
            "tier: GP_Gen5_2 · GeneralPurpose",
            "summary: In the 15 min up to Sun 12:15 UTC: CPU was near the tier's limit once (15 min, still going), data IO was near the tier's limit once (15 min, still going), log writes were near the tier's limit once (15 min, still going) and worker use neared the tier's limit twice (30 min, still going).",
        ],
        vec![
            "series cpu_percent: CPU",
            "limit cpu_percent: tier limit",
            "series memory_percent: Memory",
            "limit memory_percent: tier limit",
            "series disk_iops_consumed_percentage: IOPS used",
            "limit disk_iops_consumed_percentage: tier limit",
            "series cpu_credits_remaining: CPU credits",
            "series storage_percent: Storage used",
            "limit storage_percent: tier limit",
            "series active_connections: Connections",
            "episode cpu_percent: CPU use was high",
            "detail cpu_percent: CPU reached 98% of Standard_B1ms's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "episode disk_iops_consumed_percentage: Storage IOPS were near their limit",
            "detail disk_iops_consumed_percentage: IOPS reached 98% of Standard_B1ms's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "episode cpu_credits_remaining: CPU credits ran out",
            "detail cpu_credits_remaining: The Burstable server's CPU-credit balance was zero in samples over 15 min. Demand above baseline can be limited; these counters alone do not measure query delays.",
            "episode storage_percent: Storage nearly full",
            "detail storage_percent: Storage used reached 98% of Standard_B1ms's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "note: From Azure Monitor, one point per 5 min.",
            "tier: Standard_B1ms · Burstable · 640 IOPS",
            "summary: In the 15 min up to Sun 12:15 UTC: CPU use was high once (15 min, still going), storage IOPS were near their limit once (15 min, still going), CPU credits ran out once (15 min, still going) and storage nearly full once (15 min, still going).",
        ],
        vec![
            "series io_consumption_percent: IO used",
            "limit io_consumption_percent: tier limit",
            "episode io_consumption_percent: Storage IOPS were near their limit",
            "detail io_consumption_percent: IOPS reached 98% of this tier's limit in recorded samples over 15 min. Near a limit leaves little headroom; actual throttling or query delay is not measured by this percentage.",
            "note: From Azure Monitor, one point per 5 min.",
            "tier: ",
            "summary: In the 15 min up to Sun 12:15 UTC: storage IOPS were near their limit once (15 min, still going).",
        ],
    ];
    assert_eq!(azure, expected);
}

#[tokio::test]
async fn a_database_outside_the_cloud_says_which_ones_have_metrics_in_english() {
    let dir = std::env::temp_dir().join(format!("culprit-cloud-english-{}", std::process::id()));
    let e = signals(&dir, &info(Engine::Postgres, "db.example.com", None), 0.0, 3600.0, false).await.unwrap_err();
    assert_eq!(e.message, "'x' isn't a cloud database Culprit can read metrics for");
    assert_eq!(
        e.hint.as_deref(),
        Some(
            "Cloud metrics cover Amazon RDS and Aurora (*.rds.amazonaws.com) and Azure SQL Database or Azure Database \
             for PostgreSQL/MySQL (*.database.windows.net, *.database.azure.com) endpoints"
        )
    );
}
