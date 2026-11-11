//! Native metadata discovery and progress reads on disposable loopback SQL Server only.
use super::*;
use crate::{conn::SessionKind, test_support};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn available_profile_metadata_is_detected_before_an_idle_session_read() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(test_support::config(port, "master", &password, "culprit"), None));
    let layout = pool.observe(|reader| Box::pin(Layout::read(reader))).await.unwrap();
    assert_eq!(layout.last_actual, Availability::Available);
    assert_eq!(layout.live_xml, Availability::Available);
    assert_eq!(layout.profiles, Availability::Available);

    let mut application = pool.connect(SessionKind::User).await.unwrap();
    let session = test_support::session_id(&mut application).await;
    let progress = query_progress(&pool, &session).await.unwrap();
    assert!(!progress.running);
    assert!(progress.plan.is_none());
    assert!(progress.nodes.is_empty());
    assert_eq!(pool.source_shutdown(), culprit_core::telemetry::SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn retained_actual_plan_never_comes_from_another_database_with_matching_hashes() {
    use crate::{
        read::{Parameter, ReadError, ReadSession, integer, text},
        test_support::{batch, config, query_store_ready},
    };
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let databases = [format!("culprit_actual_a_{stamp}"), format!("culprit_actual_b_{stamp}")];
    let master = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    for database in &databases {
        batch(&mut admin,&format!("CREATE DATABASE [{database}]; ALTER DATABASE [{database}] SET QUERY_STORE=ON (OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL);")).await;
    }
    let first = Arc::new(Pool::new(config(port, &databases[0], &password, "fixture-a"), None));
    let second = Arc::new(Pool::new(config(port, &databases[1], &password, "fixture-b"), None));
    let mut app_a = first.connect(SessionKind::User).await.unwrap();
    let mut app_b = second.connect(SessionKind::User).await.unwrap();
    for app in [&mut app_a, &mut app_b] {
        batch(app, "ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS=ON;").await;
        query_store_ready(app).await;
    }
    const TARGET: &str =
        "SELECT SUM(v.n) AS retained_scope_canary FROM (VALUES(1),(2),(3),(4),(5)) v(n) OPTION(MAXDOP 1)";
    batch(&mut app_a, TARGET).await;
    batch(&mut app_a, "EXEC sys.sp_query_store_flush_db").await;
    let mut native_a = ReadSession::application(first.clone());
    let selection = "SELECT q.query_id,CONVERT(varchar(18),q.query_hash,1) AS query_hash,CONVERT(varchar(18),p.query_plan_hash,1) AS plan_hash,DB_ID() AS database_id FROM sys.query_store_query q JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id JOIN sys.query_store_plan p ON p.query_id=q.query_id WHERE t.query_sql_text=@P1 OPTION(MAXDOP 1)";
    let rows = native_a
        .query(selection, vec![Parameter::Text(TARGET.into())], 2)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let row = rows.first().expect("owned query was captured");
    let query_id = integer(row, "query_id").unwrap();
    let expected_database = integer(row, "database_id").unwrap();
    let query_hash = text(row, "query_hash").unwrap();
    let plan_hash = text(row, "plan_hash").unwrap();
    // The foreign database runs later, making it the old hash-only lookup's preferred result.
    for _ in 0..3 {
        batch(&mut app_b, TARGET).await;
    }
    batch(&mut app_b, "EXEC sys.sp_query_store_flush_db").await;
    let mut native_b = ReadSession::application(second.clone());
    let rows = native_b
        .query(selection, vec![Parameter::Text(TARGET.into())], 2)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let foreign = rows.first().expect("foreign control was captured");
    assert_eq!(text(foreign, "query_hash").unwrap(), query_hash);
    assert_eq!(
        text(foreign, "plan_hash").unwrap(),
        plan_hash,
        "the control must exercise a genuine hash collision across databases"
    );
    let foreign_database = integer(foreign, "database_id").unwrap();
    let old=native_a.query("SELECT TOP(1) CAST(pa.value AS int) AS database_id FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_plan_attributes(qs.plan_handle) pa WHERE pa.attribute='dbid' AND qs.query_hash=CONVERT(binary(8),@P1,1) AND qs.query_plan_hash=CONVERT(binary(8),@P2,1) ORDER BY qs.last_execution_time DESC OPTION(MAXDOP 1)",vec![Parameter::Text(query_hash),Parameter::Text(plan_hash)],1).await.map_err(ReadError::into_error).unwrap();
    assert_eq!(
        integer(&old[0], "database_id").unwrap(),
        foreign_database,
        "old lookup must select the wrong database in this fixture"
    );
    let read = retained::read(&mut native_a, query_id, None, 2 * 1024 * 1024).await.unwrap();
    let plan = read.plan.expect("current database retains the exact linked statement");
    assert_eq!(i64::from(plan.context.database_id), expected_database);
    assert_eq!(plan.executions, "1");
    assert!(plan.payload.text().is_some());
    assert_eq!(native_a.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
    assert_eq!(native_b.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
    drop(app_a);
    drop(app_b);
    drop(first);
    drop(second);
    for database in databases {
        batch(
            &mut admin,
            &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]"),
        )
        .await;
    }
}
