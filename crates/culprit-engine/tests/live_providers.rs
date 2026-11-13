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

/// Native data crosses the real journal, restart, replay and destination-independent analytical projection.
#[cfg(feature = "collector")]
#[tokio::test]
#[ignore = "requires an owned loopback fixture from dev/smoke-providers.py"]
async fn native_telemetry_survives_outbox_restart_and_analytical_projection() {
    telemetry_roundtrip(connection(), true).await;
}

#[cfg(feature = "collector")]
#[tokio::test]
async fn sqlite_telemetry_survives_outbox_restart_and_analytical_projection() {
    let root = tempfile::TempDir::with_prefix("culprit-file-pipeline-").unwrap();
    let path = root.path().join("fixture.sqlite");
    let spec = ConnectionSpec {
        engine: Some(Engine::Sqlite),
        database: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    };
    // The file is created through the fixture connection, never by the read-only provider.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT); CREATE INDEX ix_name ON items(name); INSERT INTO items VALUES(1,'private-value'); ANALYZE;").unwrap();
    drop(conn);
    telemetry_roundtrip(spec, false).await;
}

#[cfg(feature = "collector")]
async fn telemetry_roundtrip(spec: ConnectionSpec, activity: bool) {
    use culprit_collector::analytical::{catalog::LogicalCatalog, envelope::Envelope, projection};
    use culprit_collector::{capture::CollectionCancellation, delivery::contract::Contract, outbox::Outbox};
    use culprit_core::{AuthKind, telemetry::*};
    use culprit_engine::{NewConnection, Workspace};
    use std::sync::Arc;

    let engine = spec.engine.unwrap();
    let input = match engine {
        Engine::Sqlite => format!("sqlite://{}", spec.database.as_ref().unwrap()),
        _ => format!("{}://reader@fixture.invalid/sample", serde_json::to_value(engine).unwrap().as_str().unwrap()),
    };
    let root = tempfile::TempDir::with_prefix("culprit-native-pipeline-").unwrap();
    let workspace = Workspace::open_in(root.path().join("workspace"))
        .with_provider(engine, Arc::new(move |_| culprit_engine::registry::provider(&spec)));
    workspace
        .add_connection(NewConnection {
            name: "native".into(),
            input,
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    let catalog = LogicalCatalog::new().unwrap();
    let contract =
        Contract::new("owned-test".into(), "shard-0".into(), "projection:fixture".into(), catalog.digest().into())
            .unwrap();
    let mut projected = 0;
    let mut selections: Vec<_> =
        [SnapshotDataset::Operational, SnapshotDataset::Settings, SnapshotDataset::Tables, SnapshotDataset::Columns]
            .into_iter()
            .map(|dataset| CollectionSelection::Snapshot { dataset })
            .collect();
    if activity {
        selections.extend([
            CollectionSelection::Snapshot { dataset: SnapshotDataset::StatementCounters },
            CollectionSelection::Snapshot { dataset: SnapshotDataset::StatementDefinitions },
            CollectionSelection::Activity { policy: Default::default() },
        ]);
    }
    for (number, selection) in selections.into_iter().enumerate() {
        let request = CollectionRequest {
            subject: culprit_core::telemetry::CollectionSubject::Database,
            selection: selection.clone(),
            budget: CollectionBudget { page_rows: 2, max_snapshot_rows: 10000, max_pages: 1000, ..Default::default() },
        };
        let manifest = workspace
            .collect_into_outbox("native", request, &mut outbox, &CollectionCancellation::default())
            .await
            .unwrap();
        assert_eq!(manifest.status, CollectionStatus::Complete, "{selection:?}: {manifest:?}");
        drop(outbox);
        outbox = Outbox::open(&path).unwrap();
        let replay_dir = root.path().join(format!("replay-{number}"));
        let replay = Outbox::replay(&path, &manifest.capture.observation_id, &replay_dir).await.unwrap();
        assert_eq!(replay.capture, manifest.capture);
        let prepared =
            outbox.delivery().prepare(&manifest.capture.observation_id, contract.clone(), vec![]).await.unwrap();
        for file in &manifest.files {
            if file.stream == TelemetryStream::ActivityCheckpoints {
                continue; // Recovery-only state is deliberately absent from the analytical schema.
            }
            let schema = catalog.get(file.stream).unwrap();
            let envelope = Envelope::new(prepared.manifest(), &file.batch_id).unwrap();
            let bytes = std::fs::read(replay_dir.join(&file.file)).unwrap();
            for (index, line) in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).enumerate() {
                let row: ExportedRecord = serde_json::from_slice(line).unwrap();
                let input = envelope.input(index + 1, &row).unwrap();
                let values = schema.project(&input).unwrap();
                let encoded: serde_json::Value = serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap();
                let returned: Vec<_> = schema.columns().iter().map(|column| encoded[&column.name].clone()).collect();
                let normalized = schema
                    .columns()
                    .iter()
                    .zip(&returned)
                    .map(|(column, value)| column.kind.returned(value, column.nullable).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(projection::digest(&normalized).unwrap(), projection::digest(&values).unwrap());
                assert_eq!(encoded["Engine"], serde_json::to_value(engine).unwrap());
                projected += 1;
            }
        }
    }
    assert!(projected > 10, "the native fixture must project actual rows, not only empty streams");
}

#[tokio::test]
#[ignore = "requires an owned disposable database from dev/smoke-providers.py"]
async fn live_measured_plan_reports_confirmed_protection_without_changing_data() {
    use culprit_core::{ExplainMode, plan_facts::ExecutionProtection};
    let spec = connection();
    let engine = spec.engine.unwrap();
    let provider = culprit_engine::registry::provider(&spec).unwrap();
    let table = engine.dialect().qualified(&[&configured("CULPRIT_SMOKE_SCHEMA"), "smoke_rows"]);
    let sql = format!("SELECT id, value FROM {table} WHERE id = 42");
    let before = provider.run(&sql, 2).await.unwrap();
    let measured = provider.explain(&sql, ExplainMode::Analyze).await.unwrap();
    let protection = match engine {
        Engine::Mssql | Engine::Postgres | Engine::Yugabyte => ExecutionProtection::RolledBackTransaction,
        Engine::Mysql => ExecutionProtection::ReadOnlyTransaction,
        Engine::Sqlite => panic!("SQLite does not measure application statements"),
    };
    assert_eq!(measured.facts().execution_protection, Some(protection));
    assert_eq!(provider.run(&sql, 2).await.unwrap().rows, before.rows);
}
