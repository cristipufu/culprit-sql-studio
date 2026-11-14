//! End-to-end source parity on a newly created database on disposable loopback SQL Server.
use super::*;
use crate::test_support::{batch, config};
use crate::{
    conn::{Pool, SessionKind},
    read::Parameter,
};
use culprit_core::{
    SnapshotOptions,
    changes::Period,
    telemetry::{SourceShutdown, TelemetryRecord, TelemetryStream},
};
use std::{sync::Arc, time::Duration};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_streaming_reuses_the_owned_decoder_and_drains_late_errors_or_cancellation_before_release() {
    use crate::read::{ReadError, ReadSession};
    use culprit_core::telemetry::{CollectionBudget, CollectionStop};
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let budget = CollectionBudget { page_rows: 2, ..Default::default() };
    let mut reader = ReadSession::new(pool.clone(), budget.clone());
    reader.begin_stream("SELECT n FROM (VALUES(1),(2),(3),(4),(5)) v(n) OPTION(MAXDOP 1)", vec![], 10).await.unwrap();
    let first = reader.next_batch().await.unwrap();
    assert_eq!(first.rows.len(), 2);
    assert!(!first.complete);
    assert!(reader.read_metrics().unwrap().active_tasks > 0);
    assert!(matches!(
        reader.query("SELECT 999", vec![], 1).await,
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
    ));
    assert_eq!(reader.queries(), 1, "no replacement query while the stream owns its socket");
    let second = reader.next_batch().await.unwrap();
    assert_eq!(second.rows.len(), 2);
    assert!(!second.complete);
    let last = reader.next_batch().await.unwrap();
    assert_eq!(last.rows.len(), 1);
    assert!(last.complete);
    let values: Vec<_> = first
        .rows
        .into_iter()
        .chain(second.rows)
        .chain(last.rows)
        .map(|row| crate::read::integer(&row, "n").unwrap())
        .collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);
    assert_eq!(reader.read_metrics().unwrap().active_tasks, 0);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let mut reader = ReadSession::new(pool.clone(), budget.clone());
    reader
        .begin_stream(
            "SELECT n FROM (VALUES(1),(2),(3),(4),(5)) v(n); RAISERROR('fixture-late-stream-error',16,1);",
            vec![],
            10,
        )
        .await
        .unwrap();
    while let Ok(batch) = reader.next_batch().await {
        assert!(!batch.complete, "native late errors must prevent the final successful batch");
    }
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.read_metrics().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|m| m.failures).sum::<u32>(), 1);

    let mut reader = ReadSession::new(pool.clone(), CollectionBudget { page_rows: 1, ..budget.clone() });
    reader.begin_stream("SELECT n FROM (VALUES(1),(2),(3),(4),(5)) v(n) OPTION(MAXDOP 1)", vec![], 10).await.unwrap();
    let first = reader.next_batch().await.unwrap();
    assert!(!first.complete);
    assert_eq!(tokio::time::timeout(Duration::from_secs(3), reader.close()).await.unwrap(), SourceShutdown::Quiescent);
    assert!(!reader.read_metrics().unwrap().has_unconfirmed_work());

    for sql in [
        "SELECT TOP(1000) REPLICATE(CAST('x' AS varchar(max)),512) AS payload FROM sys.all_objects OPTION(MAXDOP 1)",
        "SELECT TOP(10000) REPLICATE(CAST('x' AS varchar(max)),8000) AS payload FROM sys.all_objects a CROSS JOIN sys.all_objects b OPTION(MAXDOP 1)",
    ] {
        let mut reader = ReadSession::new(pool.clone(), CollectionBudget { page_rows: 1, ..budget.clone() });
        reader.begin_stream(sql, vec![], 10001).await.unwrap();
        let first = reader.next_batch().await.unwrap();
        assert!(!first.complete);
        let shutdown = tokio::time::timeout(Duration::from_secs(3), reader.close()).await.unwrap();
        let metrics = reader.read_metrics().unwrap();
        assert_eq!(metrics.active_tasks, 0);
        // A byte count is not a deadline guarantee. Both buffered responses can exceed bounded decoder drain
        // under load. Uncertainty must remain visible and prohibit replacement work.
        assert_eq!(metrics.has_unconfirmed_work(), shutdown == SourceShutdown::Unconfirmed);
        if shutdown == SourceShutdown::Unconfirmed {
            let queries = reader.queries();
            assert!(matches!(
                reader.query("SELECT 999", vec![], 1).await,
                Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
            ));
            assert_eq!(reader.queries(), queries);
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_query_store_fragments_definitions_and_interactive_projections_agree() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_qs_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    batch(&mut admin, &format!("ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=120; ALTER DATABASE [{database}] SET QUERY_STORE=ON (OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=ON)")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut application =
        crate::test_support::connect(&config(port, &database, &password, "query-store-fixture"), SessionKind::User)
            .await
            .unwrap();
    // Setting desired mode does not establish that Query Store has begun recording a newly created database.
    crate::test_support::query_store_ready(&mut application).await;
    // Only the disposable fixture executes application SQL and forces a Query Store flush.
    let sql = "SELECT N'private-query-store-canary' AS label, SUM(v.n) AS n FROM (VALUES(1),(2),(3)) v(n)";
    for _ in 0..4 {
        batch(&mut application, sql).await;
    }
    let literal_marker_sql =
        "SELECT N'/* culprit */' AS application_literal_marker, SUM(v.n) AS n FROM (VALUES(1),(2),(3)) v(n)";
    batch(&mut application, literal_marker_sql).await;
    let monitoring_sql = "SELECT N'collector-diagnostic-canary' AS label, SUM(v.n) AS n FROM (VALUES(1),(2),(3)) v(n)";
    batch(&mut application, &crate::statement_origin::monitoring_batch(monitoring_sql)).await;
    batch(&mut application, "EXEC sys.sp_query_store_flush_db").await;
    let mut reader = Reader::open(pool.clone());
    let summary = reader.summary(60).await.unwrap();
    assert!(!summary.is_empty());
    let ids = summary.iter().map(|row| row.identity.query_id.clone()).collect::<Vec<_>>();
    let definitions = reader.definitions(&ids, definitions::Kind::Query, 256 * 1024).await.unwrap();
    let definition = definitions
        .iter()
        .find(|row| row.payload.text().is_some_and(|text| text.contains("private-query-store-canary")))
        .expect("captured fixture query");
    assert_eq!(definition.payload.text(), Some(sql));
    let query_id = definition.id.clone();
    let query_identity = definition.query.clone().expect("original native query identity");
    let selected = summary.iter().filter(|row| row.identity.query_id == query_id).collect::<Vec<_>>();
    assert!(selected.iter().all(|row| row.identity == query_identity));
    assert_eq!(selected.iter().map(|row| row.moments.executions).sum::<u64>(), 4);
    let intervals = reader
        .read_rows("SELECT /* culprit */ runtime_stats_interval_id AS id FROM sys.query_store_runtime_stats_interval")
        .await
        .unwrap();
    let mut collector_visible = false;
    let mut runtime_count = 0u64;
    let mut duration_us = 0.0;
    let mut cpu_us = 0.0;
    let mut reads = 0.0;
    for interval in intervals {
        let rows = reader
            .read(
                &catalog_sql::runtime(false),
                vec![Parameter::Integer(1000), Parameter::Integer(crate::read::integer(&interval, "id").unwrap())],
                1000,
            )
            .await
            .unwrap();
        let exported_ids = rows.iter().filter_map(|row| row.str("query_id")).collect::<Vec<_>>();
        for definition in reader.definitions(&exported_ids, definitions::Kind::Query, 256 * 1024).await.unwrap() {
            if definition.payload.text().is_some_and(|sql| sql.contains("collector-diagnostic-canary")) {
                assert_eq!(definition.origin, culprit_core::telemetry::StatementOrigin::CulpritMonitoring);
                let rows = reader
                    .read(
                        &catalog_sql::query_definitions(false),
                        vec![
                            reader::ids_parameter(std::slice::from_ref(&definition.id)).unwrap(),
                            Parameter::Integer(256 * 1024),
                        ],
                        1,
                    )
                    .await
                    .unwrap();
                let provenance = crate::statement_origin::read(&rows[0], rows[0].str("payload").as_deref());
                assert_eq!(provenance.template_id, Some(crate::statement_origin::template_id(monitoring_sql)));
                collector_visible = true;
            }
        }
        for row in rows {
            if row.string("query_id") != query_id {
                continue;
            }
            let TelemetryRecord::QueryRuntime(raw) = decode::record(
                &row,
                TelemetryStream::QueryRuntime,
                culprit_core::telemetry::StatementReferences::default(),
            )
            .unwrap() else {
                unreachable!()
            };
            let n = raw.executions.parse::<u64>().unwrap();
            runtime_count += n;
            duration_us += raw.metric(&culprit_core::telemetry::RuntimeMetricKey::Duration).unwrap().average * n as f64;
            cpu_us += raw.metric(&culprit_core::telemetry::RuntimeMetricKey::CpuTime).unwrap().average * n as f64;
            reads += raw.metric(&culprit_core::telemetry::RuntimeMetricKey::LogicalReads).unwrap().average * n as f64;
        }
        reader
            .read(
                &catalog_sql::waits(false),
                vec![Parameter::Integer(1000), Parameter::Integer(crate::read::integer(&interval, "id").unwrap())],
                1000,
            )
            .await
            .unwrap();
    }
    assert!(collector_visible, "collector queries must remain available for cost investigation");
    assert_eq!(runtime_count, 4);
    let near = |a: f64, b: f64| assert!((a - b).abs() <= 1e-6_f64.max(a.abs() * 1e-10), "{a} != {b}");
    near(duration_us, selected.iter().map(|row| row.moments.duration_us).sum());
    near(cpu_us, selected.iter().map(|row| row.moments.cpu_us).sum());
    near(reads, selected.iter().map(|row| row.moments.logical_reads).sum());
    let plans = selected.iter().map(|row| row.plan_id.clone()).collect::<Vec<_>>();
    assert!(selected.iter().all(|row| row.costs.aborted == 0 && row.costs.failed == 0));
    assert!(selected.iter().all(|row| row.costs.log_bytes.is_some()));
    let compilations = reader.compilations(std::slice::from_ref(&query_id)).await.unwrap();
    assert_eq!(compilations.len(), 1);
    assert_eq!(compilations[0].identity, query_identity);
    assert!(compilations[0].count > 0);
    assert!(compilations[0].within_window(60));
    let memory = compilations[0].memory.expect("Query Store retains per-query compiler memory");
    assert!(memory.is_valid());
    assert!(memory.average_kb.is_some() && memory.last_kb.is_some() && memory.maximum_kb.is_some());
    let gateways = crate::compiler_gateways::read(&mut reader, 512).await.unwrap();
    assert!(!gateways.is_empty());
    assert!(gateways.iter().all(|gateway| gateway.observation().unwrap().is_valid()));
    let compile_queue =
        crate::queues::read(&mut reader, culprit_core::telemetry::QueueKind::CompilationRequests).await.unwrap();
    assert_eq!(compile_queue.count, "0");
    assert_eq!(compile_queue.minimum_wait_ms, 1000);
    reader.read_rows(&costs::database_compilation_sql()).await.unwrap();
    let snapshots =
        reader.read(&costs::compilation_snapshot_sql(), vec![Parameter::Integer(1000)], 1000).await.unwrap();
    let snapshots = snapshots.iter().map(costs::Compilation::decode).collect::<culprit_core::Result<Vec<_>>>().unwrap();
    assert!(
        snapshots.iter().any(|row| row.identity.query_id == query_id && row.count > 0 && row.observation().is_valid())
    );
    reader.read(&families::sql(), vec![Parameter::Integer(60)], 75).await.unwrap();
    let tuning = tuning::read(&mut reader).await;
    let definitions = reader.definitions(&plans, definitions::Kind::Plan, 2 * 1024 * 1024).await.unwrap();
    assert!(definitions.iter().all(|row| row.payload.text().is_some_and(|text| text.contains("ShowPlanXML"))));
    reader.waits(60, None).await.unwrap();
    reader.waits(60, Some(&plans)).await.unwrap();
    let options = SnapshotOptions { window_minutes: 60, top_queries: 10, plans: 1, ..SnapshotOptions::default() };
    let app = crate::workload::from_query_store(&mut reader, &options).await.unwrap();
    assert_eq!(app.query_identities.get(&query_id), Some(&query_identity));
    let query = app.workload.queries.iter().find(|row| row.id == query_id).expect("interactive query");
    assert!(query.text.contains("private-query-store-canary"));
    assert!(
        app.workload.queries.iter().any(|query| query.text.contains("application_literal_marker")),
        "literal marker text must not erase application work"
    );
    let now = chrono::Utc::now();
    batch(&mut application,"CREATE SEQUENCE dbo.culprit_sequence AS bigint START WITH 9007199254740993 INCREMENT BY 1;SELECT NEXT VALUE FOR dbo.culprit_sequence;").await;
    let mut operations =
        crate::read::ReadSession::new(pool.clone(), culprit_core::telemetry::CollectionBudget::default());
    for kind in crate::operations::native::Kind::ALL {
        let facts = crate::operations::native::read(&mut operations, kind, 10_000).await.unwrap();
        let observations = facts.observations(kind);
        assert!(observations.iter().all(|row| row.is_valid()));
        if kind == crate::operations::native::Kind::Sequences {
            assert!(observations.iter().any(|row|matches!(&row.fact,culprit_core::telemetry::OperationalFact::Sequence(sequence) if sequence.current=="9007199254740993")));
        }
    }
    let file_counters =
        crate::storage_io::read(&mut operations, crate::storage_io::Selection::DatabaseAndSharedTempdb, 10_000)
            .await
            .unwrap();
    assert!(file_counters.iter().all(|row| row.is_valid()));
    assert!(file_counters.iter().any(|row| row.database.as_deref() == Some(database.as_str())));
    assert!(
        file_counters
            .iter()
            .any(|row| row.database.as_deref() == Some("tempdb") && row.scope == culprit_core::FileScope::Server)
    );
    assert!(!crate::storage_io::display(&file_counters).is_empty());
    let native_counters = crate::performance::read(&mut operations, 32).await.unwrap();
    assert!(native_counters.iter().all(|row| row.is_valid()));
    assert!(!crate::performance::application(&native_counters).is_empty());
    for kind in crate::queues::KINDS {
        let queue = crate::queues::read(&mut operations, kind).await.unwrap();
        assert_eq!(queue.scope, culprit_core::MeasurementScope::Database);
        assert!(crate::queues::observation(queue).is_valid());
    }
    let store = crate::version_store::read(&mut operations).await.unwrap();
    assert!(crate::version_store::bytes(&store).is_some());
    assert!(crate::version_store::observation(store).is_valid());
    let replicas =
        crate::replication::read(&mut operations, crate::replication::Selection::AvailabilityGroup, 100).await.unwrap();
    assert!(replicas.is_empty(), "the owned fixture does not enable an availability group");
    // Validate the native projection even when the disabled-AG probe correctly avoids the DMV during collection.
    assert!(
        operations
            .query(&crate::replication::availability_sql(true), vec![], 100)
            .await
            .map_err(crate::read::ReadError::into_error)
            .unwrap()
            .is_empty()
    );
    for kind in crate::settings::Kind::ALL {
        let values = crate::settings::read(&mut operations, kind, 4096).await.unwrap();
        assert!(!values.is_empty(), "the owned native settings source must be visible: {kind:?}");
        assert!(values.iter().all(culprit_core::telemetry::SettingObservation::is_valid));
        assert!(values.iter().all(|value| value.source == kind.source().name));
        if kind == crate::settings::Kind::Scoped {
            assert!(values.iter().any(|value| value.name == "MAXDOP" && value.value.is_some()));
        }
        if kind == crate::settings::Kind::Server {
            assert!(values.iter().all(|value| value.value.is_some() && value.value_in_use.is_some()));
        }
    }
    let topology = crate::processors::read(&mut operations).await.unwrap();
    assert!(topology.application().0.is_some_and(|processors| processors > 0.0));
    assert!(topology.application().1.is_some_and(|nodes| nodes > 0.0));
    assert_eq!(topology.observations().len(), 3);
    assert!(topology.observations().iter().all(|row| row.is_valid() && matches!(&row.fact, culprit_core::telemetry::OperationalFact::Counter(value) if value.scope == culprit_core::MeasurementScope::Server)));
    let cache = crate::cache_summary::read(&mut operations).await.unwrap();
    assert!(cache.application().total_plans > 0.0, "the fixture has compiled its own native workload");
    assert_eq!(cache.observations().len(), 6);
    batch(
        &mut application,
        &format!("EXEC sys.sp_query_store_set_hints @query_id={query_id}, @query_hints=N'OPTION(MAXDOP 1)'"),
    )
    .await;
    let tuning_layout = crate::query_store::tuning::Layout::read(&mut operations).await.unwrap();
    for kind in crate::query_store::tuning::Kind::ALL {
        let facts = crate::query_store::tuning::read_kind(&mut operations, kind, &tuning_layout, 512).await.unwrap();
        if let Some(facts) = facts {
            assert!(facts.iter().all(culprit_core::telemetry::TuningFact::is_valid));
            if matches!(kind, crate::query_store::tuning::Kind::Hints) {
                assert!(facts.iter().any(|fact|matches!(fact,culprit_core::telemetry::TuningFact::QueryHint(value)
                    if value.query_id==query_id && matches!(&value.hint,culprit_core::telemetry::DefinitionPayload::Available{text,..} if text.contains("MAXDOP")))));
            }
        }
    }
    assert!(cache.observations().iter().all(culprit_core::telemetry::OperationalObservation::is_valid));
    assert_eq!(operations.close().await, SourceShutdown::Quiescent);
    let mut wait_reader =
        crate::read::ReadSession::new(pool.clone(), culprit_core::telemetry::CollectionBudget::default());
    let native_waits = crate::waits::read(&mut wait_reader, false, crate::waits::MAX_WAIT_TYPES)
        .await
        .map_err(crate::read::ReadError::into_error)
        .unwrap();
    assert!(!native_waits.is_empty());
    assert!(native_waits.iter().all(|row| row.is_valid()
        && row.scope == culprit_core::MeasurementScope::Server
        && row.reset == culprit_core::ResetMarker::Unknown));
    let sampled = crate::pressure::sample(&mut wait_reader, false, 1).await.unwrap();
    assert!(sampled.seconds >= 1.0);
    assert!(sampled.categories.iter().all(|row| row.wait_ms >= 0.0));
    assert_eq!(wait_reader.close().await, SourceShutdown::Quiescent);
    let baseline = Period {
        start: (now - chrono::Duration::hours(2)).to_rfc3339(),
        end: (now - chrono::Duration::hours(1)).to_rfc3339(),
    };
    let recent = Period { start: baseline.end.clone(), end: now.to_rfc3339() };
    crate::periods::collect(&mut reader, &baseline, &recent).await.unwrap();
    reader.definitions(std::slice::from_ref(&query_id), definitions::Kind::Query, 256 * 1024).await.unwrap();
    let shutdown = reader.close().await;
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    let mut tuning_coverage = culprit_core::Coverage::default();
    assert!(tuning.finish(&mut tuning_coverage, "", shutdown, true).is_some());
    assert!(tuning_coverage.outcomes().iter().all(|outcome| outcome.state != culprit_core::SourceState::Unavailable));
    drop(reader);
    drop(application);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn owned_read_metrics_cover_bound_commands_and_timeout_without_persisting_arguments() {
    use crate::read::{ReadError, ReadSession};
    use culprit_core::telemetry::{CollectionBudget, CollectionStop};
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    // master has no Query Store requirement: client work remains observable when native history is unavailable.
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let warmed = pool.get().await.unwrap();
    pool.put(warmed);
    // Positive commands use the production collection budget. The delayed command, rather than host speed,
    // establishes the intentional timeout; a 200 ms deadline also races normal startup/compilation work.
    let budget = CollectionBudget::default();
    let mut reader = ReadSession::new(pool, budget.clone());
    let sql = "SELECT @P1 AS label";
    for argument in ["private-read-argument-one", "private-read-argument-two"] {
        reader.query(sql, vec![Parameter::Text(argument.into())], 1).await.map_err(ReadError::into_error).unwrap();
    }
    let error = reader.query("WAITFOR DELAY '00:00:10';SELECT 1 AS value", vec![], 1).await.err().unwrap().into_error();
    assert!(matches!(
        CollectionStop::from_error(&error),
        CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed
    ));
    reader.close().await;
    let metrics = reader.read_metrics().unwrap();
    assert!(metrics.is_valid());
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    let successful = metrics
        .templates
        .iter()
        .find(|metric| metric.template_id == crate::statement_origin::template_id(sql))
        .unwrap();
    assert_eq!(successful.completed_tasks, 2);
    assert_eq!(successful.rows_decoded, "2");
    assert_eq!(successful.failures, 0);
    let timed = metrics.templates.iter().find(|metric| metric.timeouts > 0).unwrap();
    assert_eq!(timed.timeouts, 1);
    assert_eq!(timed.failures, 1);
    assert!(timed.client_elapsed_ms >= f64::from(budget.statement_timeout_ms));
    let json = serde_json::to_string(&metrics).unwrap();
    assert!(!json.contains("private-read-argument"));
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn interrupted_owned_read_cannot_be_replaced_before_the_old_task_is_settled() {
    use crate::read::{ReadError, ReadSession};
    use culprit_core::telemetry::{CollectionBudget, CollectionStop};
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let warmed = pool.get().await.unwrap();
    pool.put(warmed);
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    {
        let read = reader.query("WAITFOR DELAY '00:00:02';SELECT 1 AS value", vec![], 1);
        tokio::pin!(read);
        tokio::select! {
            _=tokio::time::sleep(Duration::from_millis(20))=>{},
            _=&mut read=>panic!("the delayed read cannot have completed"),
        }
    }
    assert!(matches!(
        reader.query("SELECT 2 AS value", vec![], 1).await,
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
    ));
    assert_eq!(reader.queries(), 1);
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|row| row.cancellations).sum::<u32>(), 1);
    if shutdown == SourceShutdown::Unconfirmed {
        assert!(matches!(
            reader.query("SELECT 3 AS value", vec![], 1).await,
            Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
        ));
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn session_setup_is_tagged_on_the_wire_and_retains_its_safety_settings() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut observer =
        crate::test_support::connect(&config(port, "master", &password, "culprit-setup-observer"), SessionKind::User)
            .await
            .unwrap();
    for (kind, name, isolation) in
        [(SessionKind::Stats, "culprit-setup-stats-fixture", 1), (SessionKind::User, "culprit-setup-user-fixture", 2)]
    {
        let session = crate::test_support::connect(&config(port, "master", &password, name), kind).await.unwrap();
        // Observe from another connection: asking the tested session would replace its last input buffer.
        let rows = crate::conn::query(
            &mut observer,
            &format!(
                "SELECT s.transaction_isolation_level,s.deadlock_priority,s.lock_timeout,
             CONVERT(nvarchar(4000),i.event_info) AS sent_sql FROM sys.dm_exec_sessions s
             CROSS APPLY sys.dm_exec_input_buffer(s.session_id,NULL) i WHERE s.program_name=N'{name}'"
            ),
        )
        .await
        .unwrap();
        let [row] = rows.as_slice() else { panic!("one owned setup session") };
        let sql = row.string("sent_sql");
        let comments = culprit_core::sql::block_comments(&sql, culprit_core::Engine::Mssql).unwrap().unwrap();
        assert!(
            comments.contains(&crate::statement_origin::OWN_QUERY_MARKER),
            "the server received untagged session setup"
        );
        assert!(!comments.contains(&crate::statement_origin::MONITORING_QUERY_MARKER));
        assert_eq!(row.f64("transaction_isolation_level"), Some(isolation as f64));
        assert_eq!(row.f64("deadlock_priority"), Some(-5.0));
        assert_eq!(row.f64("lock_timeout"), Some(2000.0));
        drop(session);
    }
}
