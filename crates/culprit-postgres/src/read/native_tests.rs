//! Read safety on the existing owned loopback PostgreSQL fixture, never on a user-supplied database.
use super::*;
use culprit_core::{ConnectionSpec, ErrorCode};

fn connector() -> Connector {
    let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").expect("owned provider fixture database");
    assert!(matches!(database.as_str(), "culprit_smoke" | "culprit_timescale"));
    let spec = ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_PREVIEW_PG_PORT").unwrap().parse().unwrap()),
        database: Some(database),
        user: Some("postgres".into()),
        password: Some(std::env::var("CULPRIT_PREVIEW_PG_PASSWORD").unwrap()),
        options: [("sslmode".into(), "disable".into())].into(),
        ..Default::default()
    };
    Connector::new(&spec).unwrap()
}
fn session(budget: CollectionBudget) -> ReadSession {
    ReadSession::new(connector(), budget)
}

#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_setup_and_results_keep_read_only_budgets_exact_values_and_unknowns() {
    let mut reader = session(CollectionBudget::default());
    let row = reader.query("SELECT current_setting('default_transaction_read_only') AS ro, current_setting('max_parallel_workers_per_gather') AS parallel, current_setting('work_mem') AS memory, 9007199254740993::bigint AS exact, NULL::bigint AS unknown", 1).await.unwrap().remove(0);
    assert_eq!(row.string("ro"), "on");
    assert_eq!(row.string("parallel"), "0");
    assert_eq!(row.string("memory"), "4MB");
    assert_eq!(row.string("exact"), "9007199254740993");
    assert_eq!(row.value("unknown"), Some(None));
    assert_eq!(reader.queries(), 2, "setup is an owned, counted native task too");
    let error = reader
        .query("INSERT INTO public.smoke_rows(id,value) VALUES (-900123,42)", 0)
        .await
        .expect_err("writes must be refused");
    assert_ne!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(
        reader.query("SELECT count(*) AS n FROM public.smoke_rows WHERE id=-900123", 1).await.unwrap()[0].string("n"),
        "0"
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.metrics().unwrap();
    assert!(metrics.is_valid());
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|metric| metric.failures).sum::<u32>(), 1);
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_row_and_byte_limits_never_accept_a_truncated_response_as_a_complete_read() {
    for (sql, rows, reason) in [
        ("SELECT i, repeat('x', 1024) AS text FROM pg_catalog.generate_series(1,10000) i", 2, CollectionStop::RowLimit),
        ("SELECT repeat('x', 5000) AS text", 10, CollectionStop::ByteLimit),
    ] {
        let mut reader = session(CollectionBudget { page_bytes: 4096, ..Default::default() });
        let error = reader.query(sql, rows).await.expect_err("bounded read");
        assert_eq!(CollectionStop::from_error(&error), reason);
        assert_eq!(reader.query("SELECT 42 AS n", 1).await.unwrap()[0].string("n"), "42");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert!(!reader.metrics().unwrap().has_unconfirmed_work());
    }
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_errors_and_late_result_errors_drain_ready_before_follow_up_reads() {
    let mut reader = session(CollectionBudget::default());
    for sql in ["SELECT 1/0", "SELECT 1 AS prefix; SELECT 1/0"] {
        let error = reader.query(sql, 10).await.expect_err("no successful prefix may hide a late error");
        assert_ne!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        assert_eq!(reader.query("SELECT 7 AS n", 1).await.unwrap()[0].string("n"), "7");
    }
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_timeout_is_confirmed_by_the_original_response_not_by_sending_cancel() {
    let mut reader =
        session(CollectionBudget { statement_timeout_ms: 500, total_timeout_ms: 10000, ..Default::default() });
    let started = Instant::now();
    let error = reader.query("SELECT pg_catalog.pg_sleep(30)", 1).await.err().unwrap();
    assert!(error.code == ErrorCode::Timeout || CollectionStop::from_error(&error) == CollectionStop::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.metrics().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|metric| metric.timeouts).sum::<u32>(), 1);
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_dropped_callers_cannot_admit_replacement_reads_before_owned_cleanup() {
    let mut reader =
        session(CollectionBudget { statement_timeout_ms: 10000, total_timeout_ms: 30000, ..Default::default() });
    reader.query("SELECT 1", 1).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), reader.query("SELECT pg_catalog.pg_sleep(30)", 1))
            .await
            .is_err()
    );
    let error = reader.query("SELECT 42", 1).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.metrics().unwrap();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|metric| metric.cancellations).sum::<u32>(), 1);
    assert!(!reader.fence.is_uncertain());
}
#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn native_disconnect_without_ready_poisoning_blocks_the_next_source_without_a_new_connection() {
    let connection = connector();
    let mut reader = ReadSession::new(connection.clone(), CollectionBudget::default());
    let pid = reader.query("SELECT pg_catalog.pg_backend_pid() AS pid", 1).await.unwrap()[0]
        .string("pid")
        .parse::<i32>()
        .unwrap();
    let admin = connection.connect(crate::connect::INTERACTIVE).await.unwrap();
    let read = tokio::spawn(async move {
        let result = reader.query("SELECT pg_catalog.pg_sleep(30)", 1).await;
        (reader, result)
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    admin.simple_query(&crate::rows::tagged(&format!("SELECT pg_catalog.pg_terminate_backend({pid})"))).await.unwrap();
    let (mut reader, result) = read.await.unwrap();
    assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    let provider =
        crate::PostgresProvider { connector: connection.clone(), spec: culprit_core::ConnectionSpec::default() };
    assert_eq!(culprit_core::Provider::source_shutdown(&provider), SourceShutdown::Unconfirmed);
    let mut next = ReadSession::new(connection, CollectionBudget::default());
    assert_eq!(
        CollectionStop::from_error(&next.query("SELECT 42", 1).await.err().unwrap()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(next.queries(), 0);
}
