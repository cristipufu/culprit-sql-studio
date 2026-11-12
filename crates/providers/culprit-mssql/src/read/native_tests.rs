//! Cancellation is observed from another session on an owned loopback SQL Server, never a production endpoint.
use super::*;
use crate::{conn::SessionKind, test_support::config};
use culprit_core::telemetry::SourceShutdown;
mod protocol;

struct ObservedSession {
    id: i64,
    generation: String,
}
struct RequestState {
    active: bool,
    cpu_ms: i32,
    wait_type: Option<String>,
}

async fn fixture() -> (ReadSession, MsClient, ObservedSession) {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let observer = pool.connect(SessionKind::User).await.unwrap();
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let identity = identity(&mut reader).await;
    reader.budget.statement_timeout_ms = 1500;
    (reader, observer, identity)
}

async fn identity(reader: &mut ReadSession) -> ObservedSession {
    let rows = reader.query("SELECT CONVERT(bigint,@@SPID) AS id,CONVERT(varchar(33),CONVERT(datetime2(7),login_time),126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID",vec![],1).await.unwrap();
    ObservedSession { id: integer(&rows[0], "id").unwrap(), generation: rows[0].string("generation") }
}

async fn state(observer: &mut MsClient, session: &ObservedSession) -> Option<RequestState> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut query = Query::new("SELECT CAST(CASE WHEN r.session_id IS NULL THEN 0 ELSE 1 END AS bit) AS active,COALESCE(r.cpu_time,0) AS cpu_ms,r.wait_type FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id WHERE s.session_id=@P1 AND CONVERT(datetime2(7),s.login_time)=CONVERT(datetime2(7),@P2,126)");
        query.bind(session.id);
        query.bind(session.generation.as_str());
        query.query(observer).await.unwrap().into_row().await.unwrap().map(|row| RequestState {
            active: row.get::<bool,_>("active").unwrap(),
            cpu_ms: row.get::<i32,_>("cpu_ms").unwrap(),
            wait_type: row.get::<&str,_>("wait_type").map(str::to_owned),
        })
    }).await.expect("the independent native observer must remain responsive")
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn a_schema_lock_bounds_metadata_waiting_and_does_not_leave_a_request_running() {
    let (mut reader, mut observer, session) = fixture().await;
    reader.budget.statement_timeout_ms = 5000;
    let table = format!("culprit_safety_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    crate::test_support::batch(
        &mut observer,
        &format!("CREATE TABLE dbo.[{table}](id int); BEGIN TRAN; ALTER TABLE dbo.[{table}] ADD note int"),
    )
    .await;
    let mut monitor = reader.pool.connect(SessionKind::User).await.unwrap();
    let started = Instant::now();
    let sql = format!("SELECT OBJECT_ID(N'dbo.{table}') AS id");
    {
        let work = reader.query(&sql, vec![], 1);
        tokio::pin!(work);
        loop {
            tokio::select! {
                result=&mut work=>panic!("metadata fixture ended before a native schema wait was observed: {result:?}"),
                observed=state(&mut monitor,&session)=>{
                    if observed.and_then(|state|state.wait_type).as_deref()==Some("LCK_M_SCH_S") {break;}
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(work.await.is_err(), "the statistics session must not wait indefinitely on schema modification");
    }
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "LOCK_TIMEOUT must end the wait before the client statement deadline"
    );
    let shutdown = reader.close().await;
    assert_quiet(&mut monitor, &session).await;
    if shutdown == SourceShutdown::Unconfirmed {
        assert!(matches!(reader.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
    }
    crate::test_support::batch(&mut observer, "ROLLBACK").await;
    crate::test_support::batch(&mut observer, &format!("DROP TABLE dbo.[{table}]")).await;
}

async fn assert_quiet(observer: &mut MsClient, session: &ObservedSession) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state(observer, session).await.is_some_and(|state| state.active) {
        assert!(Instant::now() < deadline, "the original server request survived cancellation");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn fully_received_server_errors_preserve_source_failure_without_claiming_unknown_work() {
    for sql in [
        "SELECT * FROM dbo.culprit_missing_safety_fixture",
        "SELECT 1 AS n; RAISERROR('owned completed error fixture',16,1)",
    ] {
        let (mut reader, mut observer, session) = fixture().await;
        let result = reader.query(sql, vec![], 1).await;
        let shutdown = reader.close().await;
        assert_quiet(&mut observer, &session).await;
        assert!(
            matches!(result, Err(ReadError::Source(_))),
            "a complete server response must preserve its typed source error: {result:?}"
        );
        assert_eq!(shutdown, SourceShutdown::Quiescent);
        assert!(reader.query("SELECT 1 AS n", vec![], 1).await.is_ok());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn an_error_token_before_unfinished_work_is_not_a_quiescence_acknowledgement() {
    let (mut reader, mut observer, session) = fixture().await;
    let result = {
        let work =
            reader.query("RAISERROR('owned unfinished error fixture',16,1); WAITFOR DELAY '00:00:15'", vec![], 1);
        tokio::pin!(work);
        loop {
            tokio::select! {
                result=&mut work=>panic!("the early error must not conceal the unfinished server request: {result:?}"),
                observed=state(&mut observer,&session)=>{
                    if observed.is_some_and(|state|state.active && state.wait_type.as_deref()==Some("WAITFOR")) {break;}
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        work.await
    };
    let shutdown = reader.close().await;
    assert_quiet(&mut observer, &session).await;
    match result {
        Err(ReadError::Stopped(CollectionStop::TimedOut)) => assert_eq!(shutdown, SourceShutdown::Quiescent),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)) => {
            assert_eq!(shutdown, SourceShutdown::Unconfirmed);
            assert!(matches!(reader.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
        }
        result => panic!("an early SQL error does not prove later work has stopped: {result:?}"),
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn cpu_timeout_stops_the_original_request_on_the_server() {
    let (mut reader, mut observer, session) = fixture().await;
    let result = {
        let work = reader.query("SELECT SUM(CONVERT(bigint,CHECKSUM(NEWID()))) AS n FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c OPTION(MAXDOP 1)",vec![],1);
        tokio::pin!(work);
        loop {
            tokio::select! {
                result=&mut work=>panic!("CPU fixture ended before independent observation: {result:?}"),
                observed=state(&mut observer,&session)=>{
                    if observed.is_some_and(|state|state.active && state.cpu_ms>0) {break;}
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        work.await
    };
    let shutdown = reader.close().await;
    assert_quiet(&mut observer, &session).await;
    eprintln!("CPU timeout observed server-side quiescence; driver result={result:?}, shutdown={shutdown:?}");
    match result {
        Err(ReadError::Stopped(CollectionStop::TimedOut)) => assert_eq!(shutdown, SourceShutdown::Quiescent),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)) => {
            // An observed quiet fixture does not repair a missing driver acknowledgement in production.
            assert_eq!(shutdown, SourceShutdown::Unconfirmed);
            let count = reader.queries();
            assert!(matches!(
                reader.query("SELECT 1", vec![], 1).await,
                Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
            ));
            assert_eq!(reader.queries(), count);
        }
        result => panic!("the CPU deadline must be reported without accepting query output: {result:?}"),
    }
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn backpressured_stream_cancellation_stops_native_work_and_preserves_uncertainty() {
    let (mut reader, mut observer, session) = fixture().await;
    reader.budget.page_rows = 1;
    reader.begin_stream("SELECT TOP(10000) REPLICATE(CONVERT(varchar(max),'x'),1024) AS payload FROM sys.all_objects a CROSS JOIN sys.all_objects b; WAITFOR DELAY '00:00:15'",vec![],10000).await.unwrap();
    let first = reader.next_batch().await.unwrap();
    assert!(!first.complete);
    assert!(state(&mut observer, &session).await.is_some_and(|state| state.active));
    // Keep the bounded output channel full while the native deadline expires.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let shutdown = reader.close().await;
    assert_quiet(&mut observer, &session).await;
    if shutdown == SourceShutdown::Unconfirmed {
        let count = reader.queries();
        assert!(matches!(
            reader.query("SELECT 1", vec![], 1).await,
            Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
        ));
        assert_eq!(reader.queries(), count, "a closed socket alone cannot admit replacement SQL");
    }
}
