//! Owned loopback fixture: generates one real deadlock, then reads the built-in retained evidence.
use super::*;
use crate::test_support::{batch, config};
use crate::{
    conn::{Pool, SessionKind},
    read::ReadSession,
};
use culprit_core::telemetry::{CollectionBudget, DeadlockEvent, SourceShutdown};
use std::{sync::Arc, time::Duration};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn retained_event_reads_are_measured_with_large_unrelated_event_history() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "master", &password, "culprit-cost-fixture"), None));
    let mut admin = pool.connect(SessionKind::User).await.unwrap();
    let session = format!("culprit_cost_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    // A separate, disposable event session supplies files read by the real system_health wildcard template.
    // No event session is created or changed by the production collector.
    let path = format!("/var/opt/mssql/log/system_health_{session}");
    batch(&mut admin,&format!("CREATE EVENT SESSION [{session}] ON SERVER ADD EVENT sqlserver.error_reported(WHERE(error_number=50000)) ADD TARGET package0.event_file(SET filename=N'{path}.xel',max_file_size=(128),max_rollover_files=(4)) WITH(MAX_DISPATCH_LATENCY=1 SECONDS,MAX_MEMORY=65536 KB,EVENT_RETENTION_MODE=ALLOW_SINGLE_EVENT_LOSS); ALTER EVENT SESSION [{session}] ON SERVER STATE=START")).await;
    let from = (chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
    let native_sql = crate::statement_origin::monitoring_batch(&native::read_sql(native::Origin::SystemHealth, false));
    let mut expected = 0_i64;
    for added in [0, 1000, 9000] {
        if added > 0 {
            batch(&mut admin,&format!("DECLARE @i int=0,@message nvarchar(2048)=REPLICATE(N'x',1024); WHILE @i<{added} BEGIN BEGIN TRY THROW 50000,@message,1; END TRY BEGIN CATCH SET @i+=1; END CATCH END")).await;
        }
        batch(&mut admin,&format!("ALTER EVENT SESSION [{session}] ON SERVER STATE=STOP; ALTER EVENT SESSION [{session}] ON SERVER STATE=START")).await;
        let history=crate::conn::query_one(&mut admin,&format!("SELECT COUNT_BIG(*) AS events,COALESCE(SUM(CONVERT(bigint,DATALENGTH(event_data))),0) AS payload_bytes FROM sys.fn_xe_file_target_read_file(N'{path}*.xel',NULL,NULL,NULL)")).await.unwrap();
        expected += added;
        let events = crate::read::integer(&history, "events").unwrap();
        assert!(events >= expected, "the fixture must retain the generated history before measuring it");
        let generated_files = crate::conn::query(
            &mut admin,
            &format!("SELECT DISTINCT file_name FROM sys.fn_xe_file_target_read_file(N'{path}*.xel',NULL,NULL,NULL)"),
        )
        .await
        .unwrap();
        let generated_files = generated_files
            .iter()
            .map(|row| culprit_core::telemetry::digest(row.string("file_name").as_bytes()))
            .collect::<std::collections::BTreeSet<_>>();
        let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
        let until = chrono::Utc::now().to_rfc3339();
        let started = std::time::Instant::now();
        let rows =
            native::read(&mut reader, native::Origin::SystemHealth, "master", &from, &until, 501, false).await.unwrap();
        let client_ms = started.elapsed().as_millis();
        // Other tests can leave genuine deadlocks in the built-in target. Only our error-event files are excluded.
        assert!(
            rows.iter().all(|event| !generated_files.contains(&event.file_id)),
            "non-deadlock event files must not become deadlock evidence"
        );
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        let mut costs = tiberius::Query::new(
            "SELECT TOP(1) qs.last_worker_time,qs.last_elapsed_time,qs.last_logical_reads,qs.last_physical_reads FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) st WHERE RIGHT(st.text,LEN(@P1))=@P1 ORDER BY qs.last_execution_time DESC",
        );
        costs.bind(native_sql.as_str());
        let measured = costs
            .query(&mut admin)
            .await
            .unwrap()
            .into_row()
            .await
            .unwrap()
            .expect("measure the exact native template, excluding the observer query");
        eprintln!(
            "Retained-event cost: events={events}, payload_bytes={}, client_ms={client_ms}, server_worker_us={}, server_elapsed_us={}, logical_reads_pages={}, physical_reads_pages={}",
            crate::read::integer(&history, "payload_bytes").unwrap(),
            measured.get::<i64, _>("last_worker_time").unwrap(),
            measured.get::<i64, _>("last_elapsed_time").unwrap(),
            measured.get::<i64, _>("last_logical_reads").unwrap(),
            measured.get::<i64, _>("last_physical_reads").unwrap()
        );
    }
    batch(
        &mut admin,
        &format!("ALTER EVENT SESSION [{session}] ON SERVER STATE=STOP; DROP EVENT SESSION [{session}] ON SERVER"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_deadlock_graphs_keep_positions_and_share_the_interactive_parser() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_dl_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit-deadlock-fixture"), SessionKind::User)
            .await
            .unwrap();
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    // Disposable instance only: shorten the built-in target's dispatch delay for a bounded test.
    batch(&mut admin,"ALTER EVENT SESSION [system_health] ON SERVER STATE=STOP;ALTER EVENT SESSION [system_health] ON SERVER WITH(MAX_DISPATCH_LATENCY=1 SECONDS);ALTER EVENT SESSION [system_health] ON SERVER STATE=START;").await;
    let target=crate::conn::query(&mut admin,"SELECT /* culprit fixture */ CAST(t.target_data AS xml).value('(EventFileTarget/File/@name)[1]','nvarchar(4000)') AS path FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON s.address=t.event_session_address WHERE s.name=N'system_health' AND t.target_name=N'event_file'").await.unwrap();
    eprintln!("fixture target: {target:?}");

    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit-deadlock-fixture"), None));
    let mut first = pool.connect(SessionKind::User).await.unwrap();
    batch(
        &mut first,
        "CREATE TABLE dbo.fixture(id int NOT NULL PRIMARY KEY,n int);INSERT dbo.fixture VALUES(1,0),(2,0)",
    )
    .await;
    let mut second = pool.connect(SessionKind::User).await.unwrap();
    batch(&mut first, "SET LOCK_TIMEOUT 20000;BEGIN TRAN;UPDATE dbo.fixture SET n=1 WHERE id=1").await;
    batch(&mut second, "SET LOCK_TIMEOUT 20000;BEGIN TRAN;UPDATE dbo.fixture SET n=2 WHERE id=2").await;
    let from = (chrono::Utc::now() - chrono::Duration::seconds(2)).to_rfc3339();
    let first = tokio::spawn(async move {
        match first.simple_query("UPDATE dbo.fixture SET n=1 WHERE id=2;COMMIT").await {
            Ok(stream) => stream.into_results().await,
            Err(error) => Err(error),
        }
    });
    let second = tokio::spawn(async move {
        match second.simple_query("UPDATE dbo.fixture SET n=2 WHERE id=1;COMMIT").await {
            Ok(stream) => stream.into_results().await,
            Err(error) => Err(error),
        }
    });
    let (first, second) = tokio::join!(first, second);
    assert_ne!(first.unwrap().is_ok(), second.unwrap().is_ok(), "one transaction is the deadlock victim");
    let mut reader = ReadSession::new(
        pool.clone(),
        CollectionBudget { page_bytes: 8 * 1024 * 1024, total_timeout_ms: 120000, ..Default::default() },
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let (events, until) = loop {
        let until = chrono::Utc::now().to_rfc3339();
        let events = native::read(&mut reader, native::Origin::SystemHealth, &database, &from, &until, 100, false)
            .await
            .unwrap();
        if events.iter().any(|event| !event.graphs.is_empty()) {
            break (events, until);
        }
        if tokio::time::Instant::now() >= deadline {
            let ring = reader.query(native::RING, vec![], 1).await.map_err(crate::read::ReadError::into_error).unwrap();
            eprintln!("fixture ring target: {ring:?}");
            panic!("system_health did not make the fixture event available");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let event = events.iter().find(|event| !event.graphs.is_empty()).unwrap();
    let repeat =
        native::read(&mut reader, native::Origin::SystemHealth, &database, &from, &until, 100, false).await.unwrap();
    assert_eq!(
        events.iter().map(|row| &row.position).collect::<Vec<_>>(),
        repeat.iter().map(|row| &row.position).collect::<Vec<_>>()
    );
    let graph = &event.graphs[0];
    assert_eq!(graph.processes.iter().filter(|process| process.victim).count(), 1);
    let mut exported = DeadlockEvent {
        position: event.position.clone(),
        file_id: event.file_id.clone(),
        file_offset: event.offset.clone(),
        graph_ordinal: 0,
        at: event.at.clone(),
        processes: graph.processes.clone(),
        resources: graph.resources.clone(),
        pattern: graph.pattern,
    };
    exported.redact(Engine::Mssql);
    assert!(exported.is_valid());
    assert!(exported.processes.iter().all(|process| process.login.is_none() && process.host.is_none()));
    let interactive = recent(&mut reader, 1.0).await.unwrap();
    assert!(interactive.deadlocks.iter().any(|known| known.id == graph.id));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
}
