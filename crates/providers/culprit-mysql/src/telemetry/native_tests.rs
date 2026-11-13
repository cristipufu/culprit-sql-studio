use super::*;
use culprit_core::{ConnectionSpec, Engine, Provider};

fn fixture() -> crate::MysqlProvider {
    crate::MysqlProvider::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_MYSQL_TEST_PORT").expect("use the owned MySQL fixture").parse().unwrap()),
        user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or("root".into())),
        password: Some(std::env::var("CULPRIT_MYSQL_TEST_PASSWORD").expect("use the owned MySQL fixture")),
        database: Some("culprit_smoke".into()),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn routine_namespaces_and_index_visibility_are_shared_with_scans() {
    use mysql_async::prelude::Queryable;
    let provider = fixture();
    let mut admin = provider.connector.connect(crate::connect::Scope::Database).await.unwrap();
    admin.query_drop("/* culprit */ SET SESSION TRANSACTION READ WRITE").await.unwrap();
    admin.query_drop("/* culprit */ CREATE PROCEDURE culprit_namespace_probe() SELECT 1").await.unwrap();
    admin
        .query_drop("/* culprit */ CREATE FUNCTION culprit_namespace_probe() RETURNS INT DETERMINISTIC NO SQL RETURN 1")
        .await
        .unwrap();
    let (_, records) = capture(&provider, SnapshotDataset::Objects).await;
    let identities: std::collections::BTreeSet<_> = records
        .iter()
        .filter_map(|record| match record {
            TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {
                fact: ObjectDefinitionFact::NativeCatalog(catalog),
                ..
            }) if catalog.properties.get("name").is_some_and(|name| name == "culprit_namespace_probe") => {
                Some(catalog.identity.id.clone())
            }
            _ => None,
        })
        .collect();
    admin.query_drop("/* culprit */ DROP PROCEDURE culprit_namespace_probe").await.unwrap();
    admin.query_drop("/* culprit */ DROP FUNCTION culprit_namespace_probe").await.unwrap();
    assert_eq!(identities.len(), 2, "procedures and functions have independent native namespaces");

    let version = provider.connector.version().unwrap();
    let hidden = match provider.connector.flavor() {
        crate::connect::Flavor::Mysql if culprit_core::version_at_least(version, [8, 0, 0]) => Some("INVISIBLE"),
        crate::connect::Flavor::Mariadb if culprit_core::version_at_least(version, [10, 6, 0]) => Some("IGNORED"),
        _ => None,
    };
    if let Some(hidden) = hidden {
        admin
            .query_drop(format!("/* culprit */ CREATE INDEX culprit_visibility_probe ON smoke_rows(value) {hidden}"))
            .await
            .unwrap();
        let (_, records) = capture(&provider, SnapshotDataset::Indexes).await;
        let exported = records.iter().any(|record| {
            matches!(record,
            TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CatalogCounters(row), .. })
            if row.catalog.properties.get("index_name").is_some_and(|name|name=="culprit_visibility_probe")
                && row.catalog.properties.get("visible").is_some_and(|visible|visible=="NO"))
        });
        let snapshot = provider
            .snapshot("visibility", &culprit_core::SnapshotOptions { plans: 0, ..Default::default() })
            .await
            .unwrap();
        admin.query_drop("/* culprit */ DROP INDEX culprit_visibility_probe ON smoke_rows").await.unwrap();
        assert!(exported, "the native collector must preserve hidden/ignored state");
        assert!(snapshot.indexes.iter().any(|index| index.name == "culprit_visibility_probe" && index.is_disabled));
    }
    admin.disconnect().await.unwrap();
}

async fn capture(
    provider: &crate::MysqlProvider,
    dataset: SnapshotDataset,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset },
        budget: CollectionBudget { page_rows: 2, ..Default::default() },
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    let initial = source.describe().await.unwrap();
    let mut cursor = None;
    let mut all = vec![];
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(dataset.stream(), cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert!(records.len() <= request.budget.page_rows as usize);
        assert!(records.iter().all(TelemetryRecord::is_valid));
        all.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let completion = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    (completion, all)
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn every_native_snapshot_preserves_membership_counts_scope_and_native_units() {
    let provider = fixture();
    for dataset in [
        SnapshotDataset::Settings,
        SnapshotDataset::StatementCounters,
        SnapshotDataset::StatementDefinitions,
        SnapshotDataset::Operational,
        SnapshotDataset::Cleanup,
        SnapshotDataset::Replication,
        SnapshotDataset::Tables,
        SnapshotDataset::Indexes,
        SnapshotDataset::Columns,
        SnapshotDataset::ForeignKeys,
        SnapshotDataset::CheckConstraints,
        SnapshotDataset::Objects,
        SnapshotDataset::Triggers,
        SnapshotDataset::Partitions,
        SnapshotDataset::Statistics,
        SnapshotDataset::Sequences,
    ] {
        let (description, records) = capture(&provider, dataset).await;
        let SourceObservation::Snapshot(state) = description.observation else { panic!() };
        assert_eq!(state.records as usize, records.len(), "{dataset:?}");
        assert!(
            state.sources.iter().all(|source| source.complete_scope
                || (source.state == culprit_core::SourceState::NotApplicable && source.records == 0)),
            "{dataset:?}: {:?}",
            state.sources
        );
        if matches!(
            dataset,
            SnapshotDataset::Settings
                | SnapshotDataset::StatementCounters
                | SnapshotDataset::StatementDefinitions
                | SnapshotDataset::Operational
                | SnapshotDataset::Tables
                | SnapshotDataset::Indexes
                | SnapshotDataset::Columns
        ) {
            assert!(!records.is_empty(), "{dataset:?}");
        }
        for record in records {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::StatementCounters(sample),
                ..
            }) = record
            {
                assert_eq!(sample.reset, culprit_core::ResetMarker::Unknown);
                assert!(sample.totals.iter().all(|metric| matches!(metric.total, MeasurementValue::Integer(_))));
                assert!(
                    sample.totals.iter().any(|metric| metric.key == RuntimeMetricKey::Duration
                        && metric.unit == RuntimeMetricUnit::Picoseconds)
                );
            }
        }
    }
}

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget { page_rows: 2, ..Default::default() },
    }
}

async fn activity(provider: &crate::MysqlProvider) -> (CollectionDescription, Vec<TelemetryRecord>, ReadWorkMetrics) {
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Activity {
            policy: ActivityPolicy { blocked_after_ms: 1000, running_after_ms: 1000, ..Default::default() },
        },
        budget: CollectionBudget::default(),
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    let description = source.describe().await.unwrap();
    let mut all = vec![];
    for &stream in request.collection().source_streams() {
        let mut cursor = None;
        loop {
            let CollectionRead::Page { records, next_cursor } =
                source.read_page(stream, cursor.as_deref()).await.unwrap()
            else {
                panic!()
            };
            assert!(records.iter().all(TelemetryRecord::is_valid));
            all.extend(records);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }
    assert!(description.completed(source.finish().await.unwrap()).unwrap().is_some());
    let metrics = source.read_metrics().unwrap();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    (description, all, metrics)
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn healthy_activity_skips_lock_detail_and_sql_and_process_epoch_survives_second_boundaries() {
    let provider = fixture();
    let (first, rows, metrics) = activity(&provider).await;
    assert!(rows.is_empty());
    let SourceObservation::Activity(frame) = &first.observation else { panic!() };
    assert_eq!(frame.details, ActivityDetailStatus::NotNeeded);
    assert!(frame.complete_scope);
    for metric in metrics.templates {
        if let DefinitionPayload::Available { text, .. } = metric.statement {
            let text = text.to_lowercase();
            assert!(!text.contains("data_lock_waits") && !text.contains("innodb_trx") && !text.contains("sql_text"));
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (second, _, _) = activity(&provider).await;
    assert_eq!(first.observation.activity().unwrap().epoch, second.observation.activity().unwrap().epoch);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn persistent_row_blocking_keeps_the_idle_owner_and_current_request_generation() {
    use mysql_async::prelude::Queryable;
    let provider = fixture();
    let mut owner = provider.connector.connect_unconfigured().await.unwrap();
    owner.query_drop("USE culprit_smoke").await.unwrap();
    owner.query_drop("START TRANSACTION").await.unwrap();
    owner.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=1").await.unwrap();
    let owner_id = owner.id().to_string();
    let mut waiter = provider.connector.connect_unconfigured().await.unwrap();
    waiter.query_drop("USE culprit_smoke").await.unwrap();
    waiter.query_drop("SET SESSION innodb_lock_wait_timeout=8").await.unwrap();
    let waiter_id = waiter.id().to_string();
    let blocked = tokio::spawn(async move {
        let result = waiter.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=1").await;
        let _ = waiter.disconnect().await;
        result
    });
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (description, rows, _) = activity(&provider).await;
    let live = provider.live().await.unwrap();
    owner.query_drop("ROLLBACK").await.unwrap();
    let _ = owner.disconnect().await;
    blocked.await.unwrap().unwrap();
    assert!(description.observation.activity().unwrap().complete_scope);
    let waiter = rows
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Activity(row) if row.key.session.id == waiter_id => Some(row),
            _ => None,
        })
        .expect("the blocked request was not selected");
    assert!(waiter.key.request.is_some());
    let resources = &waiter.wait.as_ref().unwrap().identified_resources;
    assert!(!resources.is_empty(), "native lock identity must reach the persistent observation");
    let live_waiter = live.sessions.iter().find(|row| row.id == waiter_id).unwrap();
    assert_eq!(live_waiter.wait_resources.as_ref().unwrap(), resources);
    assert!(resources.iter().all(|resource| resource.kind == culprit_core::WaitResourceKind::DataLock
        && resource.native.qualifiers.get("engine").map(String::as_str) == Some("INNODB")));

    assert!(
        waiter
            .conditions(&ActivityPolicy { blocked_after_ms: 1000, ..Default::default() })
            .contains(&ActivityCondition::Blocked)
    );
    assert!(rows.iter().any(|record| matches!(record, TelemetryRecord::Activity(row) if row.idle && row.key.session.id == owner_id && row.open_transactions > 0)));
    assert!(matches!(waiter.statement, DefinitionPayload::Available { .. }));
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn latest_deadlock_buffer_keeps_stable_native_evidence_without_claiming_full_history() {
    use mysql_async::prelude::Queryable;
    let provider = fixture();
    let mut first = provider.connector.connect_unconfigured().await.unwrap();
    let mut second = provider.connector.connect_unconfigured().await.unwrap();
    for conn in [&mut first, &mut second] {
        conn.query_drop("USE culprit_smoke").await.unwrap();
        conn.query_drop("SET innodb_lock_wait_timeout=5").await.unwrap();
        conn.query_drop("START TRANSACTION").await.unwrap();
    }
    first.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=1").await.unwrap();
    second.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=2").await.unwrap();
    let pending = tokio::spawn(async move {
        let result = first.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=2").await;
        first.query_drop("ROLLBACK").await.unwrap();
        first.disconnect().await.unwrap();
        result
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let second_result = second.query_drop("UPDATE smoke_rows SET value=value+1 WHERE id=1").await;
    second.query_drop("ROLLBACK").await.unwrap();
    second.disconnect().await.unwrap();
    let first_result = pending.await.unwrap();
    assert!(first_result.is_err() || second_result.is_err(), "the owned fixture did not create a deadlock");
    let mut clock = ReadSession::application(provider.connector.clone(), Scope::Server);
    use crate::read::QueryRead;
    let bounds = clock
        .read(
            "SELECT DATE_FORMAT(UTC_TIMESTAMP(6)-INTERVAL 1 HOUR,'%Y-%m-%dT%H:%i:%s.%fZ') AS from_time,
        DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS until_time",
            vec![],
            1,
        )
        .await
        .unwrap();
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::DeadlockHistory {
            from: native::text(&bounds[0], "from_time").unwrap(),
            until: native::text(&bounds[0], "until_time").unwrap(),
        },
        budget: Default::default(),
    };
    clock.close().await;
    let mut positions = vec![];
    for _ in 0..2 {
        let mut source = provider.telemetry_source(&request).unwrap();
        let description = source.describe().await.unwrap();
        let SourceObservation::RetainedEvents(history) = &description.observation else { panic!() };
        assert!(!history.history_complete && history.retained_from.is_none());
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::DeadlockEvents, None).await.unwrap()
        else {
            panic!()
        };
        assert!(next_cursor.is_none());
        assert_eq!(records.len(), 1);
        let TelemetryRecord::Deadlock(event) = &records[0] else { panic!() };
        assert!(event.is_valid());
        assert!(event.processes.len() >= 2);
        positions.push(event.position.clone());
        assert!(description.completed(source.finish().await.unwrap()).unwrap().is_some());
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    assert_eq!(positions[0], positions[1], "a repeated read of one native retained report became a new event");
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn scans_and_live_share_the_owned_reader_and_native_catalog_queries() {
    let provider = fixture();
    let snapshot = provider
        .snapshot(
            "owned-native-fixture",
            &culprit_core::SnapshotOptions { plans: 2, collect_shared_resources: true, ..Default::default() },
        )
        .await
        .unwrap();
    assert!(snapshot.tables.iter().any(|table| table.name == "smoke_rows"));
    assert!(snapshot.columns.iter().any(|column| column.table == "smoke_rows"));
    assert!(snapshot.indexes.iter().any(|index| index.table == "smoke_rows"));
    assert!(snapshot.workload.is_some());
    let unsupported = snapshot
        .sources
        .iter()
        .find(|source| source.source == culprit_core::source_id::SHARED_RESOURCE_DETAILS)
        .unwrap();
    assert_eq!(unsupported.state, culprit_core::SourceState::Unavailable);
    assert!(unsupported.reason_message.is_some());
    assert!(snapshot.pressure.as_ref().is_none_or(|pressure| pressure.resource_counters.is_none()));
    let live = provider.live().await.unwrap();
    assert!(!live.taken_at.is_empty());
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}
#[test]
fn construction_declares_only_real_support_without_a_runtime_or_database_probe() {
    let provider = crate::MysqlProvider::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "fixture.invalid".into(),
        port: Some(9),
        database: Some("fixture".into()),
        user: Some("monitor".into()),
        ..Default::default()
    })
    .unwrap();
    let source = provider.telemetry_source(&request()).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    let unsupported = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::PlanCacheSummary },
        ..request()
    };
    assert_eq!(provider.telemetry_source(&unsupported).err().unwrap().code, ErrorCode::Unsupported);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn native_settings_use_shared_pages_and_preserve_global_scope_after_session_overrides() {
    let provider = crate::MysqlProvider::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_MYSQL_TEST_PORT").expect("use the owned MySQL fixture").parse().unwrap()),
        user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or("root".into())),
        password: Some(std::env::var("CULPRIT_MYSQL_TEST_PASSWORD").expect("use the owned MySQL fixture")),
        database: Some("culprit_smoke".into()),
        ..Default::default()
    })
    .unwrap();
    let request = request();
    let mut source = provider.telemetry_source(&request).unwrap();
    let before = source.describe().await.unwrap();
    let mut cursor = None;
    let mut observations = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(SnapshotDataset::Settings.stream(), cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert!(records.len() <= 2);
        for record in records {
            assert!(record.is_valid());
            let TelemetryRecord::Setting(row) = record else { panic!() };
            assert_eq!(row.scope, MeasurementScope::Server);
            assert!(
                row.value.is_none(),
                "SHOW exposes the global effective value, not independently saved configuration"
            );
            observations.push(row);
        }
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(observations.len() > 2);
    let timeout = observations
        .iter()
        .find(|setting| setting.name == "max_execution_time" || setting.name == "max_statement_time")
        .unwrap();
    assert_eq!(
        timeout.value_in_use.as_deref().unwrap().parse::<f64>().unwrap(),
        0.0,
        "the collector's own session timeout must not become a global configuration observation"
    );
    let completed = before.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert!(completed.observation.covers_request());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn disabled_activity_instrumentation_is_unknown_and_never_a_complete_empty_poll() {
    use mysql_async::prelude::Queryable;
    let provider = fixture();
    let mut admin = provider.connector.connect_unconfigured().await.unwrap();
    admin
        .query_drop("UPDATE performance_schema.setup_consumers SET ENABLED='NO' WHERE NAME='events_statements_current'")
        .await
        .unwrap();
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Activity { policy: Default::default() },
        budget: Default::default(),
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    let result = source.describe().await;
    admin
        .query_drop(
            "UPDATE performance_schema.setup_consumers SET ENABLED='YES' WHERE NAME='events_statements_current'",
        )
        .await
        .unwrap();
    admin.disconnect().await.unwrap();
    assert!(result.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(activity(&provider).await.0.observation.covers_request());
}
#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn a_populated_replication_channel_and_open_transaction_preserve_scope_without_exporting_credentials() {
    use mysql_async::prelude::Queryable;
    let provider = fixture();
    let mut admin = provider.connector.connect_unconfigured().await.unwrap();
    let version = admin.query_first::<String, _>("SELECT VERSION()").await.unwrap().unwrap();
    let flavor = crate::connect::Flavor::of_version(&version);
    let (setup, cleanup) = match flavor {
        crate::connect::Flavor::Mysql if culprit_core::version_at_least(&version, [8, 0, 23]) => (
            "CHANGE REPLICATION SOURCE TO SOURCE_HOST='127.0.0.1',SOURCE_PORT=1,SOURCE_USER='fixture_user',SOURCE_PASSWORD='private-replication-fixture' FOR CHANNEL 'culprit_test'",
            "RESET REPLICA ALL FOR CHANNEL 'culprit_test'",
        ),
        crate::connect::Flavor::Mysql => (
            "CHANGE MASTER TO MASTER_HOST='127.0.0.1',MASTER_PORT=1,MASTER_USER='fixture_user',MASTER_PASSWORD='private-replication-fixture' FOR CHANNEL 'culprit_test'",
            "RESET SLAVE ALL FOR CHANNEL 'culprit_test'",
        ),
        crate::connect::Flavor::Mariadb => (
            "CHANGE MASTER 'culprit_test' TO MASTER_HOST='127.0.0.1',MASTER_PORT=1,MASTER_USER='fixture_user',MASTER_PASSWORD='private-replication-fixture'",
            "RESET SLAVE 'culprit_test' ALL",
        ),
    };
    admin.query_drop(setup).await.unwrap();
    let (description, records) = capture(&provider, SnapshotDataset::Replication).await;
    admin.query_drop(cleanup).await.unwrap();
    assert!(description.observation.covers_request());
    assert!(!records.is_empty());
    assert!(!serde_json::to_string(&records).unwrap().contains("private-replication-fixture"));
    admin.query_drop("USE culprit_smoke").await.unwrap();
    admin.query_drop("START TRANSACTION").await.unwrap();
    admin.query_drop("UPDATE smoke_rows SET value=value WHERE id=1").await.unwrap();
    let (description, records) = capture(&provider, SnapshotDataset::Cleanup).await;
    admin.query_drop("ROLLBACK").await.unwrap();
    admin.disconnect().await.unwrap();
    assert!(description.observation.covers_request());
    assert!(!records.is_empty());
    assert!(records.iter().all(|row|matches!(row,TelemetryRecord::Operational(OperationalObservation {fact:OperationalFact::CounterSet(set),..}) if set.scope==MeasurementScope::Server)));
}
