//! Native ADR off/on and exact cleanup timestamps on an owned loopback database.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    read::ReadSession,
    test_support::{batch, config},
};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_persistent_versions_distinguish_disabled_adr_and_preserve_off_row_statistics() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_pvs_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit-pvs-fixture"), SessionKind::User)
            .await
            .unwrap();
    batch(
        &mut admin,
        &format!("CREATE DATABASE [{database}];ALTER DATABASE [{database}] SET ACCELERATED_DATABASE_RECOVERY=OFF"),
    )
    .await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    // Establish fixture readiness separately from a bounded observation on the freshly created database.
    let warmed = pool.get().await.unwrap();
    pool.put(warmed);
    let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
    let disabled = read(&mut reader).await;
    assert!(
        disabled.as_ref().is_ok_and(Option::is_none),
        "disabled ADR read: {disabled:?}; metrics: {:?}",
        reader.read_metrics()
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
    drop(pool);
    eprintln!("PVS fixture: disabled ADR verified; all fixture database connections closed");
    batch(&mut admin, &format!("ALTER DATABASE [{database}] SET ACCELERATED_DATABASE_RECOVERY=ON")).await;
    eprintln!("PVS fixture: ADR enabled");
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut workload = pool.connect(SessionKind::User).await.unwrap();
    batch(&mut workload,"CREATE TABLE dbo.fixture(id int PRIMARY KEY,n char(4000));INSERT dbo.fixture SELECT TOP (512) ROW_NUMBER() OVER(ORDER BY object_id),REPLICATE('a',4000) FROM sys.all_objects;BEGIN TRAN;UPDATE dbo.fixture SET n=REPLICATE('b',4000)").await;
    eprintln!("PVS fixture: workload ready");
    let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
    let sample = read(&mut reader).await.unwrap().unwrap();
    eprintln!("PVS fixture: statistics collected");
    assert!(sample.is_valid());
    assert!(sample.off_row_kibibytes.is_some());
    assert!(sample.available_columns.iter().any(|name| name == "persistent_version_store_size_kb"));
    let app = signal(&sample).unwrap();
    assert_eq!(app.status, culprit_core::SignalStatus::Info);
    assert_eq!(app.unit, culprit_core::SignalUnit::Bytes);
    assert!(observation(sample).is_valid());
    // Prove the native timestamp decoder independently of when the asynchronous PVS cleaner happens to run.
    let exact = reader
        .query("SELECT CAST('2020-01-01T07:58:30.1533333' AS datetime2(7)) AS value", vec![], 1)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    assert_eq!(crate::read::text(&exact[0], "value").unwrap(), "2020-01-01T07:58:30.1533333");
    batch(&mut workload, "ROLLBACK").await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
    drop(workload);
    drop(pool);
    batch(&mut admin, &format!("DROP DATABASE [{database}]")).await;
}
