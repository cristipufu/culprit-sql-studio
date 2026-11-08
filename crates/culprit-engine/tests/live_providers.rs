//! Real collection against a disposable database owned by dev/smoke-providers.py.
use culprit_core::{ConnectionSpec, Engine, Environment, Snapshot, SnapshotOptions, SourceState};
use std::time::Duration;

const COLLECTION_BUDGET: Duration = Duration::from_secs(120);

fn configured(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required; run python3 dev/smoke-providers.py"))
}

fn connection() -> ConnectionSpec {
    let engine: Engine =
        serde_json::from_value(serde_json::json!(configured("CULPRIT_SMOKE_ENGINE"))).expect("supported smoke engine");
    let mut spec = ConnectionSpec {
        engine: Some(engine),
        host: "127.0.0.1".into(),
        port: Some(configured("CULPRIT_SMOKE_PORT").parse().expect("disposable loopback port")),
        database: Some(configured("CULPRIT_SMOKE_DATABASE")),
        user: Some(configured("CULPRIT_SMOKE_USER")),
        password: Some(configured("CULPRIT_SMOKE_PASSWORD")),
        environment: Environment::Local,
        ..Default::default()
    };
    // The runner owns loopback-only servers with disposable development certificates.
    match engine {
        Engine::Postgres | Engine::Yugabyte => {
            spec.options.insert("sslmode".into(), "disable".into());
        }
        Engine::Mysql => {
            spec.options.insert("sslmode".into(), "disabled".into());
        }
        Engine::Mssql => {
            spec.options.insert("encrypt".into(), "false".into());
            spec.options.insert("trustServerCertificate".into(), "true".into());
        }
        Engine::Sqlite => panic!("SQLite's real-file tests do not need a container"),
    }
    spec
}

fn degraded_from(baseline: &Snapshot, restricted: &Snapshot) -> bool {
    restricted.server.missing.iter().any(|missing| baseline.server.has(missing.capability))
        || restricted.sources.iter().any(|source| {
            matches!(source.state, SourceState::Partial | SourceState::Unavailable)
                && baseline.sources.iter().any(|before| {
                    before.source == source.source
                        && matches!(before.state, SourceState::Collected | SourceState::Empty)
                })
        })
}

#[tokio::test]
#[ignore = "requires the owned disposable database from dev/smoke-providers.py"]
async fn live_registered_provider_collects_and_degrades_without_changing_data() {
    let mut spec = connection();
    let engine = spec.engine.unwrap();
    let provider = culprit_engine::registry::provider(&spec).unwrap();
    let schema = configured("CULPRIT_SMOKE_SCHEMA");
    let table = engine.dialect().qualified(&[&schema, "smoke_rows"]);
    let rows_sql = format!("SELECT id, value FROM {table} ORDER BY id");
    let before = provider.run(&rows_sql, 101).await.unwrap();
    assert_eq!(before.rows.len(), 100);
    let options = SnapshotOptions { plans: 0, ..Default::default() };
    let baseline = tokio::time::timeout(COLLECTION_BUDGET, provider.snapshot("provider-smoke", &options))
        .await
        .expect("collection must finish within its smoke budget")
        .unwrap();
    assert_eq!(baseline.server.engine, engine);
    assert!(baseline.tables.iter().any(|table| table.name == "smoke_rows"), "the real catalog was not read");
    let workload = baseline.workload.as_ref().expect("the seeded workload must be collected");
    assert!(workload.total_executions > 0.0 && !workload.queries.is_empty());
    assert!(
        workload.queries.iter().any(|query| query.text.contains("smoke_rows")),
        "the seeded statement was not collected"
    );
    assert_eq!(provider.run(&rows_sql, 101).await.unwrap().rows, before.rows, "collection changed the fixture data");

    spec.user = Some(configured("CULPRIT_SMOKE_LIMITED_USER"));
    spec.password = Some(configured("CULPRIT_SMOKE_LIMITED_PASSWORD"));
    let limited = culprit_engine::registry::provider(&spec).unwrap();
    let restricted = tokio::time::timeout(COLLECTION_BUDGET, limited.snapshot("restricted-smoke", &options))
        .await
        .expect("restricted collection must finish within its smoke budget")
        .unwrap();
    assert_eq!(restricted.server.engine, engine);
    assert!(restricted.tables.iter().any(|table| table.name == "smoke_rows"));
    assert!(degraded_from(&baseline, &restricted), "missing privileges must become explicit coverage gaps");
    assert_eq!(limited.run(&rows_sql, 101).await.unwrap().rows, before.rows);
}
