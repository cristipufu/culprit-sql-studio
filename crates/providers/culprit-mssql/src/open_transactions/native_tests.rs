//! Real logged work, read-only transactions and row-version participants on disposable loopback databases.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    test_support::{batch, config, session_id},
};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_cleanup_horizon_distinguishes_read_only_logged_and_snapshot_work_in_this_database() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_cleanup_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let other = format!("{database}_other");
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit-cleanup-fixture"), SessionKind::User)
            .await
            .unwrap();
    batch(&mut admin, &format!("CREATE DATABASE [{database}];CREATE DATABASE [{other}]")).await;
    for name in [&database, &other] {
        batch(
            &mut admin,
            &format!("USE [{name}];CREATE TABLE dbo.fixture(id int PRIMARY KEY,n int);INSERT dbo.fixture VALUES(1,0);UPDATE STATISTICS dbo.fixture WITH FULLSCAN"),
        )
        .await;
    }
    batch(&mut admin, "USE master").await;
    batch(&mut admin, &format!("ALTER DATABASE [{other}] SET ALLOW_SNAPSHOT_ISOLATION ON")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
    let mut unrelated =
        crate::test_support::connect(&config(port, &other, &password, "cleanup-other-db"), SessionKind::User)
            .await
            .unwrap();
    batch(&mut unrelated, "BEGIN TRAN;UPDATE dbo.fixture SET n=1 WHERE id=1").await;
    let mut reader_session =
        crate::test_support::connect(&config(port, &database, &password, "cleanup-read-only"), SessionKind::User)
            .await
            .unwrap();
    // Finish deferred compilation/statistics work before testing a transaction with only reads.
    batch(&mut reader_session, "SELECT n FROM dbo.fixture WHERE id=1").await;
    batch(&mut reader_session, "BEGIN TRAN;SELECT n FROM dbo.fixture WHERE id=1").await;
    let read_only_id = session_id(&mut reader_session).await;
    let control=reader.query("SELECT dt.database_transaction_type AS native_type,dt.database_transaction_state AS native_state,dt.database_transaction_log_record_count AS log_records,dt.database_transaction_log_bytes_used AS log_bytes,CONVERT(varchar(25),dt.database_transaction_begin_lsn) AS begin_lsn FROM sys.dm_tran_database_transactions dt JOIN sys.dm_tran_session_transactions st ON st.transaction_id=dt.transaction_id WHERE dt.database_id=DB_ID() AND st.session_id=@P1",vec![Parameter::Integer(read_only_id.parse().unwrap())],2).await.map_err(ReadError::into_error).unwrap();
    assert_eq!(control.len(), 1, "the read-only transaction is present in the native database transaction catalog");
    eprintln!("read-only transaction native control: {control:?}");
    assert_eq!(control[0].f64("log_records"), Some(0.0));
    assert!(
        read(&mut reader, Selection::Application).await.unwrap().oldest_confirmed.is_none(),
        "a read-only transaction without row versions and another database's logged work cannot hold this database's cleanup"
    );
    let unrelated_id = session_id(&mut unrelated).await;
    batch(&mut unrelated, &format!("USE [{database}];SELECT n FROM dbo.fixture WHERE id=1")).await;
    let ambiguous = read(&mut reader, Selection::Application).await.unwrap();
    assert!(ambiguous.oldest_confirmed.is_none());
    assert_eq!(
        ambiguous.oldest_unattributed_versioning.as_ref().unwrap().session.id,
        unrelated_id,
        "a transaction can have native XSN membership from another database while merely reading this one"
    );
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, Ok(ambiguous), None).is_none());
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Partial);
    let mut writer =
        crate::test_support::connect(&config(port, &database, &password, "cleanup-writer"), SessionKind::User)
            .await
            .unwrap();
    let writer_id = session_id(&mut writer).await;
    batch(&mut writer, "BEGIN TRAN;UPDATE dbo.fixture SET n=1 WHERE id=1").await;
    let logged = read(&mut reader, Selection::Application).await.unwrap();
    let raw = logged.oldest_confirmed.as_ref().unwrap();
    assert_eq!(raw.session.id, writer_id);
    assert_ne!(raw.snapshot_isolation, Some(true));
    assert!(raw.log_record_count.as_ref().unwrap().parse::<u64>().unwrap() > 0);
    assert_eq!(application(&logged, Some(1024.0)).unwrap().wrote, Some(true));
    assert_eq!(application(&logged, Some(1024.0)).unwrap().held_bytes, None);
    batch(&mut writer, "ROLLBACK").await;
    batch(&mut reader_session, "COMMIT").await;
    batch(&mut unrelated, "ROLLBACK").await;
    batch(&mut admin, &format!("ALTER DATABASE [{database}] SET ALLOW_SNAPSHOT_ISOLATION ON")).await;
    batch(&mut writer, "BEGIN TRAN;UPDATE dbo.fixture SET n=2 WHERE id=1").await;
    let version_writer = read(&mut reader, Selection::Application).await.unwrap().oldest_confirmed.unwrap();
    assert_eq!(version_writer.session.id, writer_id);
    assert!(version_writer.row_version_participant);
    assert_eq!(version_writer.snapshot_isolation, Some(false), "an XSN does not imply snapshot isolation");
    batch(&mut writer, "ROLLBACK").await;
    batch(
        &mut reader_session,
        "SET TRANSACTION ISOLATION LEVEL SNAPSHOT;BEGIN TRAN;SELECT n FROM dbo.fixture WHERE id=1",
    )
    .await;
    let snapshot = read(&mut reader, Selection::Application).await.unwrap().oldest_confirmed.unwrap();
    assert_eq!(snapshot.session.id, read_only_id);
    assert!(snapshot.row_version_participant);
    assert_eq!(snapshot.snapshot_isolation, Some(true));
    assert_eq!(snapshot.log_record_count.as_deref(), Some("0"));
    batch(&mut reader_session, "COMMIT").await;
    let mut own = pool.connect(SessionKind::User).await.unwrap();
    let own_id = session_id(&mut own).await;
    batch(&mut own, "BEGIN TRAN;UPDATE dbo.fixture SET n=3 WHERE id=1").await;
    assert!(read(&mut reader, Selection::Application).await.unwrap().oldest_confirmed.is_none());
    let own_raw = read(&mut reader, Selection::AllOtherUserSessions).await.unwrap();
    assert_eq!(
        own_raw.oldest_confirmed.unwrap().session.id,
        own_id,
        "raw overhead evidence includes another Culprit session"
    );
    batch(&mut own, "ROLLBACK").await;
    assert!(read(&mut reader, Selection::AllOtherUserSessions).await.unwrap().oldest_confirmed.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
    drop(own);
    drop(writer);
    drop(reader_session);
    drop(unrelated);
    drop(pool);
    batch(&mut admin, &format!("DROP DATABASE [{database}];DROP DATABASE [{other}]")).await;
}
