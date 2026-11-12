use super::*;
use culprit_core::{ConnectionSpec, Environment, MeasurementScope, Provider};
fn spec() -> ConnectionSpec {
    let mut spec = ConnectionSpec {
        engine: Some(Engine::Yugabyte),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_YSQL_TEST_PORT").expect("use owned Yugabyte fixture").parse().unwrap()),
        database: Some("culprit_yugabyte".into()),
        user: Some("yugabyte".into()),
        environment: Environment::Local,
        ..Default::default()
    };
    spec.options.insert("sslmode".into(), "disable".into());
    spec
}
#[test]
fn construction_is_offline_and_does_not_advertise_unimplemented_native_sources() {
    let provider = crate::YugabyteProvider::new(&ConnectionSpec {
        engine: Some(Engine::Yugabyte),
        host: "fixture.invalid".into(),
        port: Some(9),
        ..Default::default()
    })
    .unwrap();
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: Default::default(),
    };
    let source = provider.telemetry_source(&request).unwrap();
    assert_eq!(source.query_count(), 0);
    let unsupported = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::MissingIndexes },
        ..request
    };
    assert_eq!(provider.telemetry_source(&unsupported).err().unwrap().code, ErrorCode::Unsupported);
}
#[tokio::test]
#[ignore = "requires the owned YugabyteDB fixture"]
async fn node_snapshots_preserve_native_scope_and_export_no_postgres_heap_counters() {
    let provider = crate::YugabyteProvider::new(&spec()).unwrap();
    for dataset in [
        SnapshotDataset::Settings,
        SnapshotDataset::StatementCounters,
        SnapshotDataset::StatementDefinitions,
        SnapshotDataset::Tables,
        SnapshotDataset::Indexes,
        SnapshotDataset::Columns,
        SnapshotDataset::ForeignKeys,
        SnapshotDataset::CheckConstraints,
        SnapshotDataset::Objects,
        SnapshotDataset::Triggers,
        SnapshotDataset::Partitions,
        SnapshotDataset::Statistics,
        SnapshotDataset::RetainedPlans,
    ] {
        let request = CollectionRequest {
            selection: CollectionSelection::Snapshot { dataset },
            budget: CollectionBudget { page_rows: 2, ..Default::default() },
        };
        let mut source = provider.telemetry_source(&request).unwrap();
        let initial = source.describe().await.unwrap();
        let mut cursor = None;
        let mut rows = vec![];
        loop {
            let CollectionRead::Page { records, next_cursor } =
                source.read_page(dataset.stream(), cursor.as_deref()).await.unwrap()
            else {
                panic!()
            };
            assert!(records.len() <= 2);
            assert!(records.iter().all(TelemetryRecord::is_valid));
            rows.extend(records);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        assert!(completed.observation.covers_request(), "{dataset:?}: {:?}", completed.observation);
        if matches!(
            dataset,
            SnapshotDataset::Settings
                | SnapshotDataset::StatementCounters
                | SnapshotDataset::StatementDefinitions
                | SnapshotDataset::Tables
                | SnapshotDataset::Indexes
                | SnapshotDataset::Columns
        ) {
            assert!(!rows.is_empty(), "{dataset:?}");
        }
        for row in rows {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::StatementCounters(sample),
                ..
            }) = row
            {
                assert_eq!(sample.scope, MeasurementScope::Node);
                assert!(sample.identity.qualifiers.contains_key("node"));
                assert_eq!(sample.reset, culprit_core::ResetMarker::Unknown);
                assert!(sample.totals.iter().all(|metric| !matches!(metric.unit, RuntimeMetricUnit::Pages { .. })));
            }
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    }
}

async fn capture_source(
    provider: &crate::YugabyteProvider,
    selection: CollectionSelection,
) -> (CollectionDescription, Vec<TelemetryRecord>, ReadWorkMetrics) {
    let request = CollectionRequest { selection, budget: Default::default() };
    let collection = request.collection();
    let mut source = provider.telemetry_source(&request).unwrap();
    let initial = source.describe().await.unwrap();
    let mut all = vec![];
    for stream in collection.source_streams() {
        let mut cursor = None;
        loop {
            let CollectionRead::Page { records, next_cursor } =
                source.read_page(*stream, cursor.as_deref()).await.unwrap()
            else {
                panic!()
            };
            assert!(records.iter().all(TelemetryRecord::is_valid), "{records:?}");
            all.extend(records);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    let metrics = source.read_metrics().unwrap();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    (completed, all, metrics)
}
#[tokio::test]
#[ignore = "requires the owned YugabyteDB fixture"]
async fn operational_sources_keep_sampling_and_retained_history_distinct_and_healthy_activity_avoids_locks() {
    let provider = crate::YugabyteProvider::new(&spec()).unwrap();
    let (completed, records, _) =
        capture_source(&provider, CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational }).await;
    assert!(
        records
            .iter()
            .any(|record| matches!(record,TelemetryRecord::Operational(row) if row.source=="pg_stat_database")),
        "{:?}",
        completed.observation
    );
    assert!(
        records
            .iter()
            .any(|record| matches!(record,TelemetryRecord::Operational(row) if row.source=="yb_servers_metrics")),
        "{:?}",
        completed.observation
    );
    let (activity, rows, metrics) =
        capture_source(&provider, CollectionSelection::Activity { policy: Default::default() }).await;
    assert!(activity.observation.covers_request());
    assert!(rows.is_empty(), "healthy fixture has no persistent requests: {rows:?}");
    let templates = serde_json::to_string(&metrics).unwrap();
    assert!(!templates.contains("yb_lock_status"));
    assert!(!templates.contains("a.query ELSE"));
}
#[tokio::test]
#[ignore = "requires the owned YugabyteDB fixture"]
async fn persistent_docdb_blocking_preserves_the_idle_owner_without_claiming_complete_lock_history() {
    use culprit_postgres::wire::{INTERACTIVE, tagged};
    let provider = crate::YugabyteProvider::new(&spec()).unwrap();
    let owner = provider.connector.connect(INTERACTIVE).await.unwrap();
    owner.batch_execute(&tagged("SET application_name='culprit_telemetry_owner'; BEGIN READ WRITE; UPDATE public.orders SET total=total WHERE id=42")).await.unwrap();
    let waiter = provider.connector.connect(INTERACTIVE).await.unwrap();
    waiter.batch_execute(&tagged("SET application_name='culprit_telemetry_waiter'; BEGIN READ WRITE")).await.unwrap();
    let wait = tokio::spawn(async move {
        let result = waiter.batch_execute(&tagged("UPDATE public.orders SET total=total WHERE id=42")).await;
        let _ = waiter.batch_execute(&tagged("ROLLBACK")).await;
        result
    });
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let request = CollectionRequest {
        selection: CollectionSelection::Activity {
            policy: ActivityPolicy { blocked_after_ms: 1000, running_after_ms: 1000, ..Default::default() },
        },
        budget: Default::default(),
    };
    // Always release the fixture transaction even if capture fails; no stranded test holds the row lock.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let (mut source, result) = loop {
        let mut source = provider.telemetry_source(&request).unwrap();
        let result = source.describe().await;
        let linked = result.as_ref().is_ok_and(|description| {
            description.observation.activity().is_some_and(|activity| activity.blocking_edges > 0)
        });
        if linked || result.is_err() || tokio::time::Instant::now() >= deadline {
            break (source, result);
        }
        source.close().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    owner.batch_execute(&tagged("ROLLBACK")).await.unwrap();
    wait.await.unwrap().unwrap();
    let initial = result.unwrap();
    let mut records = vec![];
    for stream in request.collection().source_streams() {
        let mut cursor = None;
        loop {
            let CollectionRead::Page { records: rows, next_cursor } =
                source.read_page(*stream, cursor.as_deref()).await.unwrap()
            else {
                panic!()
            };
            records.extend(rows);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert!(
        completed.observation.covers_request(),
        "the base request frame is complete; uncertainty is declared per request"
    );
    assert!(records.iter().any(|record|matches!(record,TelemetryRecord::Activity(row) if row.idle && row.client.as_ref().and_then(|c|c.application.as_deref())==Some("culprit_telemetry_owner"))),"{records:?}");
    assert!(records.iter().any(|record| matches!(record, TelemetryRecord::Blocking(_))), "{records:?}");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires the owned YugabyteDB fixture"]
async fn retained_plan_payloads_use_the_common_inventory_and_restricted_logins_never_touch_the_unsafe_view() {
    use culprit_postgres::wire::INTERACTIVE;
    let provider = crate::YugabyteProvider::new(&spec()).unwrap();
    let client = provider.connector.connect(INTERACTIVE).await.unwrap();
    client
        .simple_query("SELECT sum(total),max(id),count(*) FROM public.orders WHERE id=42 /* retained_export_fixture */")
        .await
        .unwrap();
    let (description, records, _) =
        capture_source(&provider, CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans }).await;
    assert!(description.observation.covers_request());
    assert!(!records.is_empty());
    for record in records {
        let record = ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "retained-plan-fixture".into(), revision: None },
            record,
        )
        .unwrap();
        assert!(!serde_json::to_string(&record).unwrap().contains("max_exec_time_params"));
    }
    let mut restricted = spec();
    restricted.user = Some("smoke_reader".into());
    restricted.password = std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").ok();
    let provider = crate::YugabyteProvider::new(&restricted).unwrap();
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans },
        budget: Default::default(),
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    source.describe().await.unwrap();
    let error = source.read_page(SnapshotDataset::RetainedPlans.stream(), None).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let metrics = serde_json::to_string(&source.read_metrics()).unwrap();
    assert!(!metrics.contains("FROM pg_catalog.yb_pg_stat_plans"));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
