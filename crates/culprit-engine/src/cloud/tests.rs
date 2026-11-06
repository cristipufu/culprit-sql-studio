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
    let fetch = Fetch { signals, at: end };
    assert!(reusable(&fetch, start, end, end));
    assert!(!reusable(&fetch, end - 3600.0, end, end));
    assert!(!reusable(&fetch, end - 6.0 * 3600.0, end, end));
    assert!(!reusable(&fetch, start, end + 86400.0, end + 86400.0));
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
