//! Native relation qualification uses only an owned disposable loopback database.
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
async fn native_heap_forwarding_and_partitioned_waits_feed_the_same_scan_and_export_decoder() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let master = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    let database = format!("culprit_indexes_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut app = pool.connect(SessionKind::User).await.unwrap();
    batch(&mut app,r#"CREATE PARTITION FUNCTION culprit_parts(int) AS RANGE RIGHT FOR VALUES(50);
 CREATE PARTITION SCHEME culprit_parts_scheme AS PARTITION culprit_parts ALL TO ([PRIMARY]);
 CREATE TABLE dbo.index_fixture(id int IDENTITY(1,1) NOT NULL,bucket int NOT NULL,payload varchar(100) NOT NULL) ON culprit_parts_scheme(bucket);
 CREATE CLUSTERED INDEX ix_append ON dbo.index_fixture(id,bucket) WITH(OPTIMIZE_FOR_SEQUENTIAL_KEY=ON) ON culprit_parts_scheme(bucket);
 CREATE INDEX ix_payload ON dbo.index_fixture(payload,bucket) WITH(OPTIMIZE_FOR_SEQUENTIAL_KEY=OFF) ON culprit_parts_scheme(bucket);
 INSERT dbo.index_fixture(bucket,payload) VALUES(10,'a'),(60,'b');
 CREATE TABLE dbo.heap_fixture(id int NOT NULL,payload varchar(4000) NOT NULL);
 CREATE INDEX ix_heap ON dbo.heap_fixture(id);
 INSERT dbo.heap_fixture SELECT TOP(2000) ROW_NUMBER() OVER(ORDER BY a.object_id,b.object_id),REPLICATE('x',10) FROM sys.all_objects a CROSS JOIN sys.all_objects b;
 UPDATE dbo.heap_fixture SET payload=REPLICATE('y',3500);
 SELECT SUM(DATALENGTH(payload)) AS n FROM dbo.heap_fixture WITH(INDEX(ix_heap),FORCESEEK) WHERE id BETWEEN 1 AND 2000 OPTION(MAXDOP 1);"#).await;
    batch(&mut app, "BEGIN TRAN; UPDATE f SET payload='held' FROM dbo.index_fixture f WITH(ROWLOCK,INDEX(ix_append)) WHERE id=1 AND bucket=10;").await;
    let mut waiting = pool.connect(SessionKind::User).await.unwrap();
    let waiting_id = crate::test_support::session_id(&mut waiting).await;
    let waiter = tokio::spawn(async move {
        batch(
            &mut waiting,
            "UPDATE f SET payload='done' FROM dbo.index_fixture f WITH(ROWLOCK,INDEX(ix_append)) WHERE id=1 AND bucket=10",
        )
        .await;
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let request = crate::conn::query(
            &mut admin,
            &format!("SELECT wait_type FROM sys.dm_exec_requests WHERE session_id={waiting_id}"),
        )
        .await
        .unwrap();
        if request.first().is_some_and(|row| row.string("wait_type").starts_with("LCK_M_")) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "owned statement must actually reach the lock wait");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    batch(&mut app, "ROLLBACK;").await;
    waiter.await.unwrap();
    let mut reader =
        ReadSession::new(pool.clone(), CollectionBudget { page_bytes: 8 * 1024 * 1024, ..Default::default() });
    let layout = Layout::read(&mut reader).await.unwrap();
    let rows = reader
        .query(&select_sql(&layout), vec![Parameter::Integer(10_001)], 10_001)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let facts = rows.iter().map(|row| decode(row, &layout).unwrap()).collect::<Vec<_>>();
    assert!(facts.iter().all(MssqlIndexOperations::is_valid));
    let summary = projection::summarize(&facts).unwrap();
    assert!(summary.forwarded[&("dbo".into(), "heap_fixture".into())] > 0.0);
    let contention = &summary.contention[&("dbo".into(), "index_fixture".into(), "ix_append".into())];
    assert!(contention.row_lock_waits > 0.0, "native rows: {facts:#?}");
    assert!(contention.row_lock_wait_ms > 0.0);
    assert!(contention.sequential_key);
    assert_eq!(contention.optimize_for_sequential_key, Some(true));
    let index = facts.iter().filter(|row| row.index.as_deref() == Some("ix_append")).collect::<Vec<_>>();
    assert_eq!(index.len(), 2);
    assert_ne!(index[0].identity.partition_number, index[1].identity.partition_number);
    assert!(
        facts
            .iter()
            .filter(|row| row.index.as_deref() == Some("ix_payload"))
            .all(|row| row.optimize_for_sequential_key == Some(false))
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(app);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]"),
    )
    .await;
}
