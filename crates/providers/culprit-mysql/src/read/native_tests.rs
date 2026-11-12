//! These tests run only inside dev/smoke-providers.py's owned loopback fixture.
use super::*;
use culprit_core::{ConnectionSpec, Engine};

fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(std::env::var("CULPRIT_MYSQL_TEST_PORT").expect("use the owned MySQL fixture").parse().unwrap()),
            user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or("root".into())),
            password: Some(std::env::var("CULPRIT_MYSQL_TEST_PASSWORD").expect("use the owned MySQL fixture")),
            ..Default::default()
        })
        .unwrap(),
    )
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn owned_reads_share_bound_values_exact_counters_and_completed_setup_accounting() {
    let connector = connector();
    let mut reader = ReadSession::new(connector.clone(), CollectionBudget::default(), Scope::Server);
    assert_eq!(reader.query_count(), 0);
    let rows = reader
        .read("SELECT CAST(? AS UNSIGNED) AS counter, CAST(NULL AS CHAR) AS unknown", vec![Value::UInt(u64::MAX)], 2)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].unsigned("counter").unwrap(), Some(u64::MAX));
    assert_eq!(rows[0].native_text("unknown").unwrap(), None);
    let again = reader.read("SELECT 42 AS answer", vec![], 1).await.unwrap();
    assert_eq!(again[0].unsigned("answer").unwrap(), Some(42));
    let metrics = reader.read_metrics().unwrap();
    assert!(metrics.is_valid());
    assert!(!metrics.has_unconfirmed_work());
    assert!(reader.query_count() >= 8);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn a_native_reader_retires_under_backpressure_without_waiting_for_the_consumer() {
    let connector = connector();
    let mut reader = ReadSession::new(
        connector.clone(),
        CollectionBudget { page_rows: 1, statement_timeout_ms: 200, ..Default::default() },
        Scope::Server,
    );
    reader.begin_stream("WITH RECURSIVE n AS (SELECT 1 AS value UNION ALL SELECT value+1 FROM n WHERE value<100) SELECT value FROM n",
        vec![], 101).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(reader.streaming.is_some(), "the consumer must still own its receiver");
    assert!(reader.active.as_ref().unwrap().is_finished(), "the native owner remained blocked on page delivery");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn a_native_timeout_discards_the_interrupted_client_and_preserves_known_shutdown() {
    let connector = connector();
    let mut reader = ReadSession::new(
        connector.clone(),
        CollectionBudget { statement_timeout_ms: 200, ..Default::default() },
        Scope::Server,
    );
    // A standalone SLEEP returns 1, rather than an error, when MySQL interrupts it. Put the delay in a predicate
    // so this exercises an interrupted statement: https://dev.mysql.com/doc/refman/8.0/en/miscellaneous-functions.html
    let error = reader.read("SELECT 1 AS value FROM DUAL WHERE SLEEP(30)=0", vec![], 1).await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::Timeout);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires the owned MySQL/MariaDB fixture"]
async fn a_client_parameter_mismatch_dispatches_no_execution_and_keeps_the_protocol_synchronized() {
    let connector = connector();
    let mut reader = ReadSession::new(connector.clone(), CollectionBudget::default(), Scope::Server);
    assert!(reader.read("SELECT ? AS answer", vec![], 1).await.is_err());
    assert_eq!(reader.read("SELECT 42 AS answer", vec![], 1).await.unwrap()[0].unsigned("answer").unwrap(), Some(42));
    assert!(!reader.read_metrics().unwrap().has_unconfirmed_work());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
}
