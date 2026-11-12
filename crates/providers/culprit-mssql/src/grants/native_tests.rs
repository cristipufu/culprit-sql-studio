//! Native cache identity and application/export parity on an owned disposable database.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    read::ReadSession,
    test_support::{batch, config},
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_cache_grants_share_exact_rows_with_app_summaries_and_preserve_own_queries() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let master = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    let database = format!("culprit_grants_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut app = pool.connect(SessionKind::User).await.unwrap();
    batch(&mut app,"CREATE TABLE dbo.grant_fixture(id int NOT NULL,payload varchar(200) NOT NULL); INSERT dbo.grant_fixture SELECT TOP(10000) ROW_NUMBER() OVER(ORDER BY a.object_id,b.object_id),CONVERT(varchar(200),NEWID()) FROM sys.all_objects a CROSS JOIN sys.all_objects b;").await;
    batch(&mut app, "SELECT TOP(50) id,payload FROM dbo.grant_fixture ORDER BY payload DESC OPTION(MAXDOP 1)").await;
    batch(&mut app, "SELECT COUNT_BIG(*) AS grant_zero_canary FROM dbo.grant_fixture OPTION(MAXDOP 1)").await;
    crate::conn::query(
        &mut app,
        "SELECT COUNT_BIG(*) AS collector_grant_canary FROM dbo.grant_fixture OPTION(MAXDOP 1)",
    )
    .await
    .unwrap();
    batch(
        &mut app,
        "SELECT N'/* culprit */' AS application_literal_marker,SUM(id) AS n FROM dbo.grant_fixture OPTION(MAXDOP 1)",
    )
    .await;
    let budget = CollectionBudget { page_bytes: 8 * 1024 * 1024, ..Default::default() };
    let mut native = ReadSession::new(pool.clone(), budget);
    let layout = Layout::read(&mut native).await.unwrap().expect("native grant columns");
    assert!(layout.can_summarize());
    let rows = native
        .query(&select_sql(&layout, false), vec![Parameter::Integer(10_001)], 10_001)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let decoded = rows.iter().map(|row| decode(row, &layout).unwrap()).collect::<Vec<_>>();
    assert!(!decoded.is_empty());
    assert!(decoded.iter().all(StatementGrantStatistics::is_valid));
    assert!(
        decoded.iter().any(|row| row.context.origin == StatementOrigin::CulpritMonitoring
            && row.context.monitoring_template_id.is_some()),
        "monitoring cost evidence remains raw"
    );
    let zero_index = rows
        .iter()
        .position(|row| row.string("text_prefix").contains("grant_zero_canary"))
        .expect("the completed zero-grant query is cached");
    assert_eq!(decoded[zero_index].total_grant_kibibytes.as_deref(), Some("0"));
    let hashes = vec![decoded[zero_index].context.query_hash.clone().unwrap()];
    let selected = application(&mut native, ApplicationSelection::Hashes(&hashes)).await.unwrap().unwrap();
    assert!(selected.complete);
    assert_eq!(selected.grants.len(), 1);
    assert_eq!(selected.grants[0].avg_granted_kb, 0.0);
    assert_eq!(selected.grants[0].avg_used_kb, 0.0);
    let largest = application(&mut native, ApplicationSelection::Largest).await.unwrap().unwrap();
    assert!(!largest.grants.is_empty(), "the owned sort establishes a real grant");
    assert!(largest.grants.iter().any(|grant| grant.avg_granted_kb > 0.0));
    let runtime_rows = native
        .query(&crate::cached_runtime::select_sql(), vec![Parameter::Integer(10_001)], 10_001)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let runtime = runtime_rows.iter().map(crate::cached_runtime::decode).collect::<Result<Vec<_>>>().unwrap();
    assert!(runtime.iter().any(|row| row.context.origin == StatementOrigin::CulpritMonitoring));
    let raw_zero = runtime.iter().find(|row| row.context.identity == decoded[zero_index].context.identity).unwrap();
    assert!(raw_zero.measured());
    assert_eq!(raw_zero.executions, decoded[zero_index].executions);
    assert!(raw_zero.counters.contains_key(&CachedRuntimeMetric::WorkerMicroseconds));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    let options = culprit_core::SnapshotOptions { top_queries: 50, plans: 2, ..Default::default() };
    let mut workload_reader = ReadSession::application(pool.clone());
    let workload = crate::workload::from_plan_cache(&mut workload_reader, &options).await.unwrap();
    assert!(
        workload_reader.queries() <= 3,
        "small cached plans and text need only one summary and one read per payload shape"
    );
    assert_eq!(workload_reader.close().await, SourceShutdown::Quiescent);
    assert!(workload.plans_parsed > 0, "requested cached plans are bounded and parsed");
    assert!(
        workload.workload.queries.iter().any(|query| query.text.contains("application_literal_marker")),
        "the cache workload must retain a query whose SQL literal contains the reserved marker"
    );
    drop(app);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]"),
    )
    .await;
}
