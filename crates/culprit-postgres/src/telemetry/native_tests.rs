//! Full capture uses the common controller and files, not a provider-specific export path.
use super::*;
use culprit_collector::{
    admission::SourceGate,
    capture::{CollectionCancellation, SourceFactory, collect_once},
};
use culprit_core::{ConnectionSpec, Engine, Provider};
fn spec() -> ConnectionSpec {
    let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap();
    assert!(matches!(database.as_str(), "culprit_smoke" | "culprit_timescale"));
    ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_PREVIEW_PG_PORT").unwrap().parse().unwrap()),
        database: Some(database),
        user: Some("postgres".into()),
        password: Some(std::env::var("CULPRIT_PREVIEW_PG_PASSWORD").unwrap()),
        options: [("sslmode".into(), "disable".into())].into(),
        ..Default::default()
    }
}
struct Factory(crate::PostgresProvider);
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.0.telemetry_source(request)
    }
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_streamed_statements_definitions_and_health_pass_through_common_capture() {
    let provider = crate::PostgresProvider::new(&spec()).unwrap();
    let admin = provider.connector.connect_unconfigured().await.unwrap().client;
    admin.batch_execute(&crate::rows::tagged("CREATE SCHEMA culprit_native;
       CREATE TABLE culprit_native.parent (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY);
       CREATE TABLE culprit_native.child (id bigserial PRIMARY KEY, parent_id bigint REFERENCES culprit_native.parent(id), note text DEFAULT 'private-literal', value integer);
       INSERT INTO culprit_native.parent DEFAULT VALUES;
       INSERT INTO culprit_native.child(parent_id,value) VALUES(1,1);
       ALTER TABLE culprit_native.child ADD CONSTRAINT positive_value CHECK (value>0) NOT VALID;
       CREATE TABLE culprit_native.partitioned(id integer) PARTITION BY RANGE(id);
       CREATE TABLE culprit_native.partition_zero PARTITION OF culprit_native.partitioned FOR VALUES FROM (0) TO (10);
       CREATE FUNCTION culprit_native.reject_writes() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''collector must never invoke a trigger''; END';
       CREATE TRIGGER reject_writes BEFORE INSERT ON culprit_native.child FOR EACH ROW EXECUTE FUNCTION culprit_native.reject_writes();
       ANALYZE culprit_native.child; ANALYZE culprit_native.parent;
       SELECT pg_catalog.pg_create_physical_replication_slot('culprit_native_slot',false);")).await.unwrap();
    let columns = crate::catalog::columns(&admin, 180_000).await.unwrap();
    assert!(columns.iter().any(|column| column.schema == "culprit_native"
        && column.name == "note"
        && column.default.as_deref().is_some_and(|value| value.contains("private-literal"))));
    assert!(
        crate::catalog::foreign_keys(&admin)
            .await
            .unwrap()
            .iter()
            .any(|key| key.schema == "culprit_native" && key.ref_table == "parent")
    );
    let probe = crate::probe::probe(&admin, "127.0.0.1").await.unwrap();
    assert!(
        crate::catalog::indexes(&admin, &probe)
            .await
            .unwrap()
            .iter()
            .any(|index| index.schema == "culprit_native" && index.is_primary_key)
    );
    assert!(
        crate::catalog::tables(&admin, probe.version_num)
            .await
            .unwrap()
            .iter()
            .any(|table| table.schema == "culprit_native" && table.name == "child")
    );
    let horizon = provider.connector.connect_unconfigured().await.unwrap().client;
    horizon
        .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ;SELECT count(*) FROM culprit_native.parent")
        .await
        .unwrap();
    for dataset in [
        SnapshotDataset::StatementCounters,
        SnapshotDataset::StatementDefinitions,
        SnapshotDataset::Operational,
        SnapshotDataset::Settings,
        SnapshotDataset::Tables,
        SnapshotDataset::Indexes,
        SnapshotDataset::Columns,
        SnapshotDataset::ForeignKeys,
        SnapshotDataset::CheckConstraints,
        SnapshotDataset::Objects,
        SnapshotDataset::Triggers,
        SnapshotDataset::Partitions,
        SnapshotDataset::Statistics,
        SnapshotDataset::Replication,
        SnapshotDataset::Sequences,
        SnapshotDataset::Cleanup,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("capture");
        let manifest = collect_once(
            &SourceGate::default(),
            &Factory(crate::PostgresProvider::new(&spec()).unwrap()),
            CollectionRequest {
                selection: CollectionSelection::Snapshot { dataset },
                budget: CollectionBudget {
                    page_rows: 2,
                    max_snapshot_rows: 10000,
                    max_pages: 1000,
                    ..Default::default()
                },
            },
            &output,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_or_else(|error| {
            let status = std::fs::read(output.join("manifest.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            panic!(
                "{dataset:?}: {error:?}; status={:?}; stop={:?}",
                status.as_ref().and_then(|value| value.get("status")),
                status.as_ref().and_then(|value| value.get("stop"))
            )
        });
        assert_eq!(manifest.status, CollectionStatus::Complete, "{dataset:?}: {:?}", manifest.status);
        assert!(manifest.references.complete);
        let file = &manifest.files[0];
        assert!(file.rows > 0, "{dataset:?} must contain real native observations");
        let content = std::fs::read_to_string(output.join(&file.file)).unwrap();
        assert!(!content.contains("private-literal"), "raw default values must not escape into export");
        let records =
            content.lines().map(|line| serde_json::from_str::<ExportedRecord>(line).unwrap()).collect::<Vec<_>>();
        assert_eq!(records.len(), file.rows as usize);
        for exported in records {
            assert!(exported.record.is_valid());
            assert!(exported.record_id.is_some());
            match exported.record {
                TelemetryRecord::Operational(row) => match row.fact {
                    OperationalFact::StatementCounters(sample) => {
                        assert!(!sample.totals.iter().any(|metric| metric.key == RuntimeMetricKey::CpuTime));
                        assert!(sample.totals.iter().any(|metric| metric.key == RuntimeMetricKey::Executions));
                        if sample
                            .totals
                            .iter()
                            .find(|metric| metric.key == RuntimeMetricKey::Executions)
                            .and_then(|metric| metric.total.integer())
                            .is_some_and(|calls| calls > 0)
                        {
                            assert!(!sample.distributions.is_empty());
                        }
                    }
                    OperationalFact::CatalogCounters(set) => {
                        assert!(set.is_valid());
                    }
                    OperationalFact::CounterSet(set) => {
                        assert!(!set.counters.is_empty());
                    }
                    _ => panic!("wrong PostgreSQL native fact"),
                },
                TelemetryRecord::ObjectDefinition(row) => {
                    assert!(matches!(
                        row.fact,
                        ObjectDefinitionFact::Statement(_) | ObjectDefinitionFact::NativeCatalog(_)
                    ))
                }
                TelemetryRecord::Replication(row) => {
                    let ReplicationFact::Native(sample) = row.fact else {
                        panic!("native PG replication");
                    };
                    assert!(sample.is_valid());
                    assert_eq!(sample.measurements.scope, culprit_core::MeasurementScope::Server);
                    assert_eq!(
                        sample.measurements.properties.get("name").map(String::as_str),
                        Some("culprit_native_slot")
                    );
                    assert!(
                        !sample.measurements.counters.iter().any(|value| value.key == "kept"),
                        "an unreserved slot has no restart LSN, not zero retained WAL"
                    );
                }
                TelemetryRecord::Setting(row) => {
                    assert_eq!(row.source, "pg_settings");
                    assert!(row.is_valid());
                    if row.name == "work_mem" {
                        assert_eq!(row.scope, culprit_core::MeasurementScope::Session);
                        assert_eq!(row.native_source.as_deref(), Some("session"));
                        assert!(
                            row.value.is_none(),
                            "pg_settings reports the effective value, not a separately persisted configuration"
                        );
                        assert_eq!(row.value_in_use.as_deref(), Some("4096"));
                        assert_eq!(row.unit.as_deref(), Some("kB"));
                    }
                }
                _ => panic!("wrong PostgreSQL stream"),
            }
        }
    }
    horizon.batch_execute("ROLLBACK").await.unwrap();
    let unsupported = CollectionRequest {
        selection: CollectionSelection::QueryHistory {
            from: "2020-01-01T00:00:00Z".into(),
            until: "2020-01-01T01:00:00Z".into(),
        },
        budget: CollectionBudget::default(),
    };
    assert!(
        provider.telemetry_source(&unsupported).is_err(),
        "cumulative counters cannot impersonate native intervals"
    );
}

#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_activity_shares_live_facts_and_only_reads_locks_for_persistent_waiters() {
    use crate::read::QueryRead;
    let provider = crate::PostgresProvider::new(&spec()).unwrap();
    let admin = provider.connector.connect_unconfigured().await.unwrap().client;
    admin.batch_execute(&crate::rows::tagged("CREATE TABLE public.culprit_activity_fixture(id integer PRIMARY KEY,value integer); INSERT INTO public.culprit_activity_fixture VALUES(1,0)")).await.unwrap();
    let holder = provider.connector.connect_unconfigured().await.unwrap().client;
    holder.batch_execute("SET application_name='culprit-owner-fixture'; BEGIN; UPDATE public.culprit_activity_fixture SET value=1 WHERE id=1").await.unwrap();
    let owner_pid = holder.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get::<_, i32>(0).to_string();
    let waiter = provider.connector.connect_unconfigured().await.unwrap().client;
    waiter.batch_execute("SET application_name='culprit-blocked-fixture';SET lock_timeout='10s'").await.unwrap();
    let waiter_pid = waiter.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get::<_, i32>(0).to_string();
    let waiting = tokio::spawn(async move {
        let result =
            waiter.batch_execute("BEGIN;UPDATE public.culprit_activity_fixture SET value=2 WHERE id=1;ROLLBACK").await;
        (waiter, result)
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let mut read = &admin;
        let row = QueryRead::query(
            &mut read,
            &format!("SELECT wait_event_type FROM pg_catalog.pg_stat_activity WHERE pid={waiter_pid}"),
            1,
        )
        .await
        .unwrap();
        if row.first().is_some_and(|row| row.text("wait_event_type").as_deref() == Some("Lock")) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "fixture waiter must reach its native lock wait");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let request = CollectionRequest {
        selection: CollectionSelection::Activity {
            policy: ActivityPolicy { blocked_after_ms: 1000, running_after_ms: 1000, update_every_ms: 60000 },
        },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    };
    let factory = Factory(crate::PostgresProvider::new(&spec()).unwrap());
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("blocked");
    let manifest =
        collect_once(&SourceGate::default(), &factory, request.clone(), &first, &CollectionCancellation::default())
            .await
            .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    assert!(manifest.references.complete);
    let records = manifest
        .files
        .iter()
        .flat_map(|file| {
            std::fs::read_to_string(first.join(&file.file))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<ExportedRecord>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let observations = records
        .iter()
        .filter_map(|row| match &row.record {
            TelemetryRecord::Activity(row) => Some(row),
            _ => None,
        })
        .collect::<Vec<_>>();
    let blocked = observations.iter().find(|row| row.key.session.id == waiter_pid).expect("native blocked statement");
    assert!(blocked.conditions(request.activity_policy().unwrap()).contains(&ActivityCondition::Blocked));
    assert!(blocked.request_counters.as_ref().unwrap().cpu_ms.is_none());
    assert!(
        blocked.wait.as_ref().unwrap().resources.as_ref().unwrap().iter().any(|resource| resource.resource.is_some())
    );
    let owner = observations.iter().find(|row| row.key.session.id == owner_pid).expect("idle blocker context");
    assert!(owner.idle && owner.key.transaction.is_some());
    assert!(records.iter().any(|row|matches!(&row.record,TelemetryRecord::Blocking(edge) if edge.waiter.session.id==waiter_pid&&edge.owner.session().is_some_and(|owner|owner.id==owner_pid)&&edge.owner_observed)));
    let live = provider.live().await.unwrap();
    assert_eq!(
        live.sessions.iter().find(|row| row.id == waiter_pid).unwrap().blocked_by.as_deref(),
        Some(owner_pid.as_str())
    );
    assert!(live.blocking.is_some(), "Live already carries the graph, so the engine need not read pg_locks again");
    holder.batch_execute("ROLLBACK").await.unwrap();
    waiting.await.unwrap().1.unwrap();
    let quiet = directory.path().join("quiet");
    let healthy = collect_once(&SourceGate::default(), &factory, request, &quiet, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(healthy.status, CollectionStatus::Complete);
    assert_eq!(
        healthy.files.iter().map(|file| file.rows).sum::<u32>(),
        0,
        "successful empty captures allow persistent incidents to resolve"
    );
    let sql = serde_json::to_value(&healthy).unwrap();
    assert!(!sql.to_string().contains("pg_locks"), "healthy activity polls must not acquire a lock-manager snapshot");
}
