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

async fn native_runtime_bins(admin: &mut mysql_async::Conn, digest: &str) -> Vec<(u64, u64, u64)> {
    use mysql_async::prelude::Queryable;
    admin
        .exec(
            "/* culprit */ SELECT BUCKET_TIMER_LOW,BUCKET_TIMER_HIGH,COUNT_BUCKET
         FROM performance_schema.events_statements_histogram_by_digest
         WHERE SCHEMA_NAME=? AND DIGEST=? AND COUNT_BUCKET>0 ORDER BY BUCKET_NUMBER LIMIT 513",
            ("culprit_smoke", digest),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires the owned MySQL 8.0.31+ fixture"]
async fn runtime_distribution_source_preserves_real_native_populations_without_borrowing_counter_cohorts() {
    use mysql_async::prelude::Queryable;
    use std::time::Instant;

    const TABLE: &str = "culprit_runtime_distribution_probe";
    const CALLS: u64 = 12;
    let provider = fixture();
    let mut admin = provider.connector.connect(crate::connect::Scope::Database).await.unwrap();
    if provider.connector.flavor() != crate::connect::Flavor::Mysql
        || !culprit_core::version_at_least(provider.connector.version().unwrap(), [8, 0, 31])
    {
        eprintln!("runtime-distribution proof requires owned MySQL 8.0.31+; this fixture is not qualified");
        admin.disconnect().await.unwrap();
        return;
    }
    admin.query_drop("/* culprit */ SET SESSION TRANSACTION READ WRITE").await.unwrap();
    let fixture_started = Instant::now();
    admin
        .query_drop(format!("/* culprit */ CREATE TABLE {TABLE}(id INT PRIMARY KEY, value INT NOT NULL)"))
        .await
        .unwrap();
    let values = (1..=64).map(|id| format!("({id},{})", id * 3)).collect::<Vec<_>>().join(",");
    admin.query_drop(format!("/* culprit */ INSERT INTO {TABLE} VALUES {values}")).await.unwrap();
    let setup_elapsed = fixture_started.elapsed();
    let workload_started = Instant::now();
    for _ in 0..CALLS {
        let value: Option<u64> =
            admin.query_first(format!("SELECT SUM(value) FROM {TABLE} WHERE id<=32")).await.unwrap();
        assert_eq!(value, Some(1584));
    }
    let workload_elapsed = workload_started.elapsed();

    let observer_started = Instant::now();
    let observed: Vec<(String, u64, Option<u64>, Option<u64>)> = admin
        .exec(
            "/* culprit */ SELECT DIGEST,COUNT_STAR,QUANTILE_95,QUANTILE_99
         FROM performance_schema.events_statements_summary_by_digest
         WHERE SCHEMA_NAME=? AND DIGEST_TEXT LIKE ? AND DIGEST_TEXT LIKE 'SELECT%' LIMIT 2",
            ("culprit_smoke", format!("%{TABLE}%")),
        )
        .await
        .unwrap();
    assert_eq!(observed.len(), 1, "one synthetic statement digest");
    let (digest, calls, p95, p99) = &observed[0];
    assert_eq!(*calls, CALLS);
    assert!(p95.is_some() && p99.is_some());
    let native_bins = native_runtime_bins(&mut admin, digest).await;
    assert!(!native_bins.is_empty() && native_bins.len() <= 512);
    assert_eq!(native_bins.iter().map(|(_, _, count)| count).sum::<u64>(), CALLS);
    let before_observer_elapsed = observer_started.elapsed();

    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::RuntimeDistributions },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    };
    let mut source = provider.telemetry_source(&request).unwrap();
    assert_eq!(source.query_count(), 0, "construction performs no native work");
    let source_started = Instant::now();
    let description = source.describe().await.unwrap();
    let initial_queries = source.query_count();
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("frozen native populations")
        };
        assert_eq!(source.query_count(), initial_queries, "paging must not query the source");
        assert!(page.len() <= 1);
        records.extend(page);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    let observer_queries = source.query_count();
    let metrics = source.read_metrics();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    let source_elapsed = source_started.elapsed();

    let after_started = Instant::now();
    let after_bins = native_runtime_bins(&mut admin, digest).await;
    let after_summary: Option<(u64, Option<u64>, Option<u64>)> = admin
        .exec_first(
            "/* culprit */ SELECT COUNT_STAR,QUANTILE_95,QUANTILE_99
         FROM performance_schema.events_statements_summary_by_digest WHERE SCHEMA_NAME=? AND DIGEST=?",
            ("culprit_smoke", digest),
        )
        .await
        .unwrap();
    let after_observer_elapsed = after_started.elapsed();
    admin.query_drop(format!("/* culprit */ DROP TABLE {TABLE}")).await.unwrap();
    admin.disconnect().await.unwrap();
    assert_eq!(native_bins, after_bins, "only this owned statement is quiescent during the comparison");
    assert_eq!(after_summary, Some((*calls, *p95, *p99)));
    let native = description.observation.snapshot().unwrap();
    let bounds = ObservationBounds::new(&native.observed_from, &native.observed_until).unwrap();
    assert!(native.sources.iter().all(|coverage| !coverage.complete_scope));
    let points = records
        .iter()
        .filter_map(|record| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::RuntimeDistribution(point),
                ..
            }) = record
            else {
                panic!("typed native population")
            };
            (point.identity.id == *digest).then_some(point.as_ref())
        })
        .collect::<Vec<_>>();
    assert_eq!(points.len(), 2, "summary and histogram remain independent native points");
    for point in &points {
        assert_eq!(point.scope, MeasurementScope::Database);
        assert_eq!(point.identity.qualifiers.get("schema").map(String::as_str), Some("culprit_smoke"));
        assert!(bounds.contains(&point.observed_at));
        assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
    }
    let summary =
        points.iter().find(|point| point.distribution.source == native::runtime_distributions::SUMMARY).unwrap();
    let histogram =
        points.iter().find(|point| point.distribution.source == native::runtime_distributions::HISTOGRAM).unwrap();
    assert_eq!(summary.distribution.population.sample_count(), None);
    assert_ne!(summary.observed_at, histogram.observed_at, "each SELECT supplies its own native UTC clock");
    for (value, observed) in [
        (p95.unwrap(), summary.distribution.p95.as_ref().unwrap()),
        (p99.unwrap(), summary.distribution.p99.as_ref().unwrap()),
    ] {
        assert_eq!(
            observed,
            &culprit_core::RuntimeQuantile::HistogramUpperBound {
                lower: None,
                upper: culprit_core::RuntimeDuration::Picoseconds { value: value.to_string() },
            }
        );
    }
    let captured_bins = histogram
        .distribution
        .histogram
        .iter()
        .map(|bin| {
            let (
                culprit_core::RuntimeDuration::Picoseconds { value: low },
                culprit_core::RuntimeDuration::Picoseconds { value: high },
            ) = (&bin.lower, &bin.upper)
            else {
                panic!("exact native picoseconds")
            };
            (low.parse::<u64>().unwrap(), high.parse::<u64>().unwrap(), bin.samples.parse::<u64>().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(captured_bins, native_bins);
    assert_eq!(histogram.distribution.population.sample_count(), Some(CALLS));
    eprintln!(
        "runtime-distribution proof fixture_rows=64 fixture_calls={CALLS} setup_ms={:.3} workload_ms={:.3} independent_before_ms={:.3} independent_after_ms={:.3} source_ms={:.3} source_queries={observer_queries} native_bins={} metrics={}; bounded local evidence is not production work-cost qualification",
        setup_elapsed.as_secs_f64() * 1000.0,
        workload_elapsed.as_secs_f64() * 1000.0,
        before_observer_elapsed.as_secs_f64() * 1000.0,
        after_observer_elapsed.as_secs_f64() * 1000.0,
        source_elapsed.as_secs_f64() * 1000.0,
        native_bins.len(),
        serde_json::to_string(&metrics).unwrap()
    );
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
    let (description, records, _) = capture_requested(provider, &request).await;
    (description, records)
}

async fn capture_requested(
    provider: &crate::MysqlProvider,
    request: &CollectionRequest,
) -> (CollectionDescription, Vec<TelemetryRecord>, u32) {
    let TelemetryCollection::Snapshot(dataset) = request.collection() else { panic!("native snapshot request") };
    let mut source = provider.telemetry_source(request).unwrap();
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
    let queries = source.query_count();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    (completion, all, queries)
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
        selection: CollectionSelection::Activity,
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
    assert_eq!(frame.population, ActivityPopulation::DefaultDatabaseConnections);
    assert!(frame.coverage.complete_scope);
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
async fn metadata_baseline_retains_waiter_generation_and_idle_context_without_a_lock_walk() {
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
    assert!(description.observation.activity().unwrap().coverage.complete_scope);
    let waiter = rows
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Activity(row) if row.key.session.id == waiter_id => Some(row),
            _ => None,
        })
        .expect("the blocked request was not selected");
    assert!(waiter.key.request.is_some());
    assert!(waiter.statement_identity.is_some());
    assert_eq!(waiter.blocking_status, ActivityBlockingStatus::Unknown);
    assert!(waiter.wait.as_ref().is_none_or(|wait| wait.owners.is_empty() && wait.identified_resources.is_empty()));
    let live_waiter = live.sessions.iter().find(|row| row.id == waiter_id).unwrap();
    assert_eq!(live_waiter.blocked_by.as_deref(), Some(owner_id.as_str()));
    assert!(
        live_waiter.wait_resources.as_ref().is_some_and(|resources| !resources.is_empty()),
        "the existing separate Live path still collects native row-lock evidence"
    );
    let owner = rows
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Activity(row) if row.idle && row.key.session.id == owner_id => Some(row),
            _ => None,
        })
        .expect("the idle connection is retained without pretending its transaction count was queried");
    assert!(owner.open_transactions.is_none());
    assert!(matches!(waiter.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }));
    assert!(rows.iter().all(|record| matches!(record, TelemetryRecord::Activity(_))));
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
            &culprit_core::SnapshotOptions {
                plans: 2,
                collect_shared_resources: true,
                collect_request_memory: true,
                ..Default::default()
            },
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
    let unsupported = snapshot
        .sources
        .iter()
        .find(|source| source.source == culprit_core::source_id::CURRENT_REQUEST_MEMORY)
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
    for dataset in [SnapshotDataset::PlanCacheSummary, SnapshotDataset::TaskSpace] {
        let unsupported = CollectionRequest {
            subject: culprit_core::telemetry::CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset },
            ..request()
        };
        assert_eq!(provider.telemetry_source(&unsupported).err().unwrap().code, ErrorCode::Unsupported);
    }
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
        selection: CollectionSelection::Activity,
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
    let description = result.unwrap();
    let frame = description.observation.activity().unwrap();
    assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
    assert!(!frame.coverage.complete_scope);
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

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn retained_error_log_metadata_is_shared_bounded_and_message_free() {
    use crate::{native::error_log, read::QueryRead};
    let provider = fixture();
    let mut reader = ReadSession::new(provider.connector.clone(), crate::native_failures::budget(), Scope::Server);
    let context = error_log::context::read(&mut reader).await;
    let (context, _) = match context {
        Ok(context) => context,
        Err(error) => {
            // MariaDB and older MySQL must stop after the native platform guard, before MySQL-only source SQL.
            assert_eq!(reader.query_count(), 8, "seven owned setup statements plus one platform guard");
            assert_eq!(reader.close().await, SourceShutdown::Quiescent);
            assert_eq!(error.code, ErrorCode::Unsupported);
            let request = ResourceDiscoveryRequest { kind: SharedResourceKind::Instance, budget: Default::default() };
            assert_eq!(provider.telemetry_resources(&request).await.err().unwrap().code, ErrorCode::Unsupported);
            return;
        }
    };
    let identity = native::one(&mut reader, "SELECT @@GLOBAL.server_uuid AS uuid", vec![]).await.unwrap();
    assert_eq!(context.instance.resource.native.id, native::text(&identity, "uuid").unwrap().to_ascii_lowercase());
    assert_eq!(context.instance.resource.kind, SharedResourceKind::Instance);
    assert_eq!(
        context.instance.resource.namespace.qualifiers.get("server_uuid"),
        Some(&context.instance.resource.native.id)
    );

    let rows = reader
        .read(
            &error_log::query(),
            vec![mysql_async::Value::UInt((error_log::MAX_ROWS + 1) as u64)],
            error_log::MAX_ROWS + 1,
        )
        .await
        .unwrap();
    assert!(rows.len() >= 2, "the owned default MySQL image should retain its startup diagnostics");
    for row in &rows {
        assert!(!row.has_column("data"), "the actual SQL result must omit free-form native messages");
        assert!(!row.has_column("message"));
        assert!(row.has_column("error_code") && row.has_column("priority") && row.has_column("subsystem"));
    }
    let first = error_log::decode(&rows[0], 0).unwrap();
    let micros = native::integer(&rows[0], "logged_microseconds").unwrap();
    let expected_clock = native::one(
        &mut reader,
        "SELECT DATE_FORMAT(FROM_UNIXTIME(?), '%Y-%m-%dT%H:%i:%s.%fZ') AS native_utc",
        vec![mysql_async::Value::from(format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000))],
    )
    .await
    .unwrap();
    assert_eq!(first.logged_at, native::text(&expected_clock, "native_utc").unwrap());
    assert!(first.message.is_none());
    assert!(!reader.read_metrics().unwrap().has_unconfirmed_work());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let discovered = provider
        .telemetry_resources(&ResourceDiscoveryRequest {
            kind: SharedResourceKind::Instance,
            budget: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].identity, context.instance.resource);
    assert!(discovered[0].collections.contains(&TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures)));
    assert_eq!(
        discovered[0].collections.contains(&TelemetryCollection::Snapshot(SnapshotDataset::SessionMemory)),
        culprit_core::version_at_least(provider.connector.version().expect("qualified native version"), [8, 0, 31]),
    );
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: context.instance.resource },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::NativeFailures },
        budget: CollectionBudget { page_rows: error_log::MAX_ROWS as u32, ..Default::default() },
    };
    let (description, records, queries) = capture_requested(&provider, &request).await;
    assert!(queries <= crate::native_failures::budget().max_queries);
    let SourceObservation::Snapshot(native) = description.observation else { panic!("native snapshot") };
    assert!(!records.is_empty());
    assert!(records.len() <= error_log::MAX_ROWS);
    assert_eq!(
        native.sources[0].state,
        if native.sources[0].complete_scope {
            culprit_core::SourceState::Collected
        } else {
            culprit_core::SourceState::Partial
        }
    );
    for record in records {
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MysqlErrorLog(row), .. }) =
            record
        else {
            panic!("native error-log fact")
        };
        assert!(row.is_valid() && row.message.is_none());
    }
    let limited_request = CollectionRequest {
        budget: CollectionBudget { max_snapshot_rows: 1, ..request.budget.clone() },
        ..request.clone()
    };
    let (description, records, _) = capture_requested(&provider, &limited_request).await;
    let SourceObservation::Snapshot(native) = description.observation else { panic!("bounded snapshot") };
    assert_eq!(native.sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(records.len(), 1);

    let mut limited = provider.spec.clone();
    limited.user = Some(std::env::var("CULPRIT_SMOKE_LIMITED_USER").expect("owned limited monitoring fixture"));
    limited.password = Some(std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").expect("owned limited monitoring fixture"));
    let limited = crate::MysqlProvider::new(&limited).unwrap();
    let mut source = limited.telemetry_source(&request).unwrap();
    let error = source.describe().await.expect_err("unreadable source cannot become an empty native buffer");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn session_allocations_are_shared_bounded_and_context_only() {
    use crate::native::session_memory as memory;
    use mysql_async::prelude::Queryable;
    use std::{
        future::{Future, poll_fn},
        panic::AssertUnwindSafe,
        task::Poll,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    let provider = fixture();
    let mut reader = ReadSession::new(provider.connector.clone(), crate::session_memory::budget(), Scope::Server);
    let context = memory::context(&mut reader).await;
    let (context, _) = match context {
        Ok(context) => context,
        Err(error) => {
            assert_eq!(reader.query_count(), 8, "seven owned setup statements plus the native platform guard");
            assert_eq!(reader.close().await, SourceShutdown::Quiescent);
            assert_eq!(error.code, ErrorCode::Unsupported);
            return;
        }
    };
    assert_eq!(context.instance.resource.kind, SharedResourceKind::Instance);
    let observed = memory::read(&mut reader, &context, memory::MAX_ROWS).await;
    let metrics = reader.read_metrics();
    let shutdown = reader.close().await;
    let observed = observed.unwrap();
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(metrics.unwrap().is_valid());
    assert!(observed.sessions.iter().all(|row| row.session.id != context.instance.connection_id.to_string()));

    // The nonce lock keeps this owned actor current until capture finishes. Its finite native wait is a cleanup
    // backstop, not an assumption that a sleep happens to overlap a slower capture on an emulated server.
    let nonce = format!(
        "culprit_memory_{}_{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros()
    );
    let mut holder = provider.connector.connect_unconfigured().await.unwrap();
    let mut actor = provider.connector.connect_unconfigured().await.unwrap();
    let actor_id = actor.id().to_string();
    let acquired = tokio::time::timeout(
        Duration::from_secs(2),
        holder.exec_first::<u64, _, _>("/* culprit */ SELECT GET_LOCK(?,0)", (&nonce,)),
    )
    .await;
    if !matches!(acquired, Ok(Ok(Some(1)))) {
        // Drop closes a possibly uncertain holder socket; no followup SQL is sent after an uncertain response.
        drop(holder);
        let _ = tokio::time::timeout(Duration::from_secs(2), actor.disconnect()).await;
        panic!("the owned fixture could not acquire its nonce lock");
    }
    let actor_nonce = nonce.clone();
    let actor_task = tokio::spawn(async move {
        let result = actor.exec_first::<u64, _, _>("/* culprit */ SELECT GET_LOCK(?,30)", (&actor_nonce,)).await;
        let closed = actor.disconnect().await;
        (result, closed)
    });
    let mut ready_reader = ReadSession::new(
        provider.connector.clone(),
        CollectionBudget {
            statement_timeout_ms: 1_000,
            total_timeout_ms: 5_000,
            max_queries: 64,
            ..Default::default()
        },
        Scope::Server,
    );
    let mut capture_reader =
        ReadSession::new(provider.connector.clone(), crate::session_memory::budget(), Scope::Server);
    let mut capture_work = Box::pin(async {
        session_memory_actor_ready(&mut ready_reader, &actor_id, &nonce).await?;
        if ready_reader.close().await == SourceShutdown::Unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        crate::session_memory::capture(&mut capture_reader, memory::MAX_ROWS, Some(&context.instance.resource)).await
    });
    // Catch a test/provider panic while retaining the native readers, so close still owns any in-flight query.
    let captured = poll_fn(|cx| match std::panic::catch_unwind(AssertUnwindSafe(|| capture_work.as_mut().poll(cx))) {
        Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    })
    .await;
    drop(capture_work);
    let queries = capture_reader.query_count();
    let capture_metrics = capture_reader.read_metrics();
    let readiness_shutdown = ready_reader.close().await;
    let capture_shutdown = capture_reader.close().await;
    // Cleanup precedes every assertion, source error and resumed panic. Disconnecting the holder releases the
    // nonce lock, so the actor completes and disconnects itself; no named lock or fixture session is retained.
    let released = tokio::time::timeout(Duration::from_secs(2), holder.disconnect()).await;
    let mut actor_task = actor_task;
    let actor_result = tokio::time::timeout(Duration::from_secs(35), &mut actor_task).await;
    if actor_result.is_err() {
        actor_task.abort();
        let _ = actor_task.await;
    }
    let captured = match captured {
        Ok(captured) => captured,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    released.expect("owned holder disconnect deadline").expect("owned holder disconnected");
    let actor_result = actor_result.expect("owned actor exceeded its finite native lock deadline").unwrap();
    assert_eq!(actor_result.0.unwrap(), Some(1));
    actor_result.1.unwrap();
    assert_eq!(readiness_shutdown, SourceShutdown::Quiescent);
    assert_eq!(capture_shutdown, SourceShutdown::Quiescent);
    let captured = captured.unwrap();
    assert!(captured.snapshot.is_valid());
    assert!(queries <= crate::session_memory::budget().max_queries);
    assert!(!capture_metrics.unwrap().has_unconfirmed_work());
    let participant = captured
        .snapshot
        .observations
        .iter()
        .find(|row| row.session.id == actor_id)
        .expect("owned actor session was retained");
    assert_eq!(participant.identity.id, participant.session.generation);
    assert_eq!(participant.identity.qualifiers["server_uuid"], captured.snapshot.resource.native.id);
    assert!(participant.tracking_enabled);
    assert!(participant.current_bytes.is_some());
    let event = participant.current_context.as_ref().expect("the owned lock actor was current throughout capture");
    assert!(!event.request.id.is_empty());
    assert!(!event.request.generation.is_empty());
    assert!(canonical_time(&event.observed_at).is_ok());

    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: captured.snapshot.resource },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::SessionMemory },
        budget: crate::session_memory::budget(),
    };
    let (description, records, queries) = capture_requested(&provider, &request).await;
    assert!(description.matches_request(&request));
    assert!(queries <= request.budget.max_queries);
    assert!(records.iter().all(|record| matches!(record,
        TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::SessionMemory(row), .. })
        if row.is_valid() && row.source == crate::sources::SESSION_MEMORY.name)));
    assert_eq!(provider.connector.source_shutdown(), SourceShutdown::Quiescent);
}

async fn session_memory_actor_ready(reader: &mut ReadSession, actor_id: &str, nonce: &str) -> Result<()> {
    use crate::read::QueryRead;
    use mysql_async::Value;
    use std::time::Duration;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = reader
            .read(
                "SELECT COUNT(*) AS waiting FROM performance_schema.metadata_locks l
            JOIN performance_schema.threads t ON t.THREAD_ID=l.OWNER_THREAD_ID
            WHERE t.PROCESSLIST_ID=? AND l.OBJECT_TYPE='USER LEVEL LOCK'
              AND l.OBJECT_NAME=? AND l.LOCK_STATUS='PENDING'",
                vec![Value::from(actor_id), Value::from(nonce)],
                1,
            )
            .await?;
        if rows.first().is_some_and(|row| matches!(row.unsigned("waiting"), Ok(Some(1)))) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::new(ErrorCode::Timeout, "the owned actor never established its pending nonce lock"));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
