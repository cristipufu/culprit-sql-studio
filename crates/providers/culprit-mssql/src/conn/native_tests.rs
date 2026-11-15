use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn a_lost_statistics_connection_never_replays_the_native_operation_automatically() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(crate::test_support::config(port, "master", &password, "culprit"), None));
    let calls = Arc::new(AtomicUsize::new(0));
    let invocation = calls.clone();
    let result: Result<()> = pool
        .observe(move |reader| {
            invocation.fetch_add(1, Ordering::SeqCst);
            // A transport error does not establish whether a submitted operation reached the server or stopped.
            Box::pin(async move {
                crate::read::one(reader, "SELECT 1 AS owned_operation")
                    .await
                    .expect("the disposable SQL Server must complete the original bounded read");
                Err(map_error(tiberius::error::Error::Io {
                    kind: std::io::ErrorKind::BrokenPipe,
                    message: "owned failure fixture".into(),
                }))
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "an unacknowledged operation must never be replayed on a new session");
}
