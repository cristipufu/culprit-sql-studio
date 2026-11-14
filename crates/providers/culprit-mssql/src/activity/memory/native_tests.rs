//! The observer reads metadata only. App queries and memory-demand hints run solely in this disposable fixture.
use super::*;
use crate::{
    activity::ReadOptions,
    conn::{Pool, SessionKind},
    test_support::{assert_no_execution_grant, batch, config, session_id},
};
use futures_util::{FutureExt, TryStreamExt};
use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_FIXTURE_REQUESTS: usize = 8;
// The owning smoke fixture has a 3 GiB container limit. Keep its retained native reservations below 2 GiB.
const MAX_FIXTURE_MEMORY_KIB: u64 = 2 * 1024 * 1024;
const WORKLOAD: &str = "SELECT id,payload FROM dbo.request_memory_fixture ORDER BY payload,id OPTION(MAXDOP 1,MIN_GRANT_PERCENT=70,MAX_GRANT_PERCENT=70)";

struct FixtureRequest {
    session: String,
    release: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl FixtureRequest {
    async fn start(pool: &Pool) -> Self {
        let mut client = pool.connect(SessionKind::User).await.unwrap();
        let session = session_id(&mut client).await;
        let (release, receive) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut stream = client.simple_query(WORKLOAD).await.unwrap();
            stream.try_next().await.unwrap();
            // A granted request keeps its reservation through socket backpressure; a waiter remains queued.
            let _ = receive.await;
            stream.into_results().await.unwrap();
        });
        Self { session, release: Some(release), task }
    }
}

async fn release_requests(requests: Vec<FixtureRequest>) -> Vec<String> {
    let mut tasks = vec![];
    for mut request in requests {
        let _ = request.release.take().unwrap().send(());
        tasks.push((request.session, request.task));
    }
    let mut failures = vec![];
    for (session, mut task) in tasks {
        match tokio::time::timeout(Duration::from_secs(20), &mut task).await {
            Ok(Ok(())) => (),
            Ok(Err(error)) => failures.push(format!("fixture session {session}: {error}")),
            Err(_) => {
                task.abort();
                let _ = task.await;
                failures.push(format!("fixture session {session} did not finish after releasing all reservations"));
            }
        }
    }
    failures
}

async fn observe_request(pool: Arc<Pool>, session: &str, port: u16) -> (ActivityReader, MemoryFrame) {
    let started = Instant::now();
    let native_prefix = format!("{session}:");
    loop {
        let mut reader = ActivityReader::open(pool.clone(), options(), "127.0.0.1".into(), port).unwrap();
        let frame = reader.observe_memory().await.unwrap();
        if frame.grants.iter().any(|grant| grant.identity.id.starts_with(&native_prefix)) {
            return (reader, frame);
        }
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "native fixture request {session} never reserved or waited"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn options() -> ReadOptions {
    ReadOptions {
        budget: CollectionBudget {
            max_snapshot_rows: 64,
            statement_timeout_ms: 3_000,
            total_timeout_ms: 10_000,
            ..Default::default()
        },
        scope: ActivityScope::CurrentDatabase,
        identity: IdentityDetail::Endpoint,
    }
}

async fn prove_native_grants(
    pool: Arc<Pool>,
    requests: &mut Vec<FixtureRequest>,
    fixture: &Pool,
    holder_pool: &Pool,
    port: u16,
) {
    let mut requested = 0u64;
    let (mut reader, mut frame) = loop {
        assert!(requests.len() < MAX_FIXTURE_REQUESTS, "bounded fixture did not establish a native memory waiter");
        let actor_pool = if requests.is_empty() { holder_pool } else { fixture };
        requests.push(FixtureRequest::start(actor_pool).await);
        let (mut reader, frame) = observe_request(pool.clone(), &requests.last().unwrap().session, port).await;
        let native_prefix = format!("{}:", requests.last().unwrap().session);
        let actor = frame.grants.iter().find(|grant| grant.identity.id.starts_with(&native_prefix)).unwrap();
        let native_request = actor.requested_kibibytes.as_deref().unwrap().parse::<u64>().unwrap();
        requested = requested.checked_add(native_request).unwrap();
        let granted = frame
            .grants
            .iter()
            .filter_map(|grant| grant.granted_kibibytes.as_deref())
            .map(|value| value.parse::<u64>().unwrap())
            .sum::<u64>();
        assert!(
            requested <= MAX_FIXTURE_MEMORY_KIB && granted <= MAX_FIXTURE_MEMORY_KIB,
            "owned memory limit exceeded: requested={requested} KiB, granted={granted} KiB"
        );
        eprintln!(
            "owned session {}: {:?}, native request={} KiB, total requested={requested} KiB, granted={granted} KiB",
            requests.last().unwrap().session,
            actor.state,
            native_request
        );
        let resource = actor.resource.as_ref().unwrap();
        let native_pool = resource.id.parse::<u64>().unwrap();
        let semaphore = resource.qualifiers["resource_semaphore_id"].parse::<u64>().unwrap();
        let capacity_sql = format!(
            "SELECT TOP(1) target_memory_kb,available_memory_kb,granted_memory_kb,grantee_count,waiter_count FROM sys.dm_exec_query_resource_semaphores WHERE pool_id={native_pool} AND resource_semaphore_id={semaphore} OPTION(MAXDOP 1)"
        );
        let mut baseline = pool.connect(SessionKind::User).await.unwrap();
        let capacity = crate::conn::query(&mut baseline, &capacity_sql).await.unwrap();
        let [capacity] = capacity.as_slice() else { panic!("native fixture semaphore was not visible") };
        eprintln!(
            "native semaphore {native_pool}:{semaphore}: target={} KiB, available={} KiB, granted={} KiB, holders={}, waiters={}",
            capacity.string("target_memory_kb"),
            capacity.string("available_memory_kb"),
            capacity.string("granted_memory_kb"),
            capacity.string("grantee_count"),
            capacity.string("waiter_count")
        );
        drop(baseline);
        if actor.state == ExecutionGrantState::Waiting {
            break (reader, frame);
        }
        // Do not guess whether the next actor fits. This fixed query's last observed native demand reserves the
        // remaining fixture allowance before another bounded actor is admitted.
        assert!(
            requested.checked_add(native_request).is_some_and(|next| next <= MAX_FIXTURE_MEMORY_KIB),
            "another native reservation would exceed this fixture's allowance"
        );
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    };
    let holder_id = requests.first().unwrap().session.clone();
    let waiter_id = requests.last().unwrap().session.clone();
    let mut qualification = pool.connect(SessionKind::User).await.unwrap();
    assert_no_execution_grant(&mut qualification, &sql::context(IdentityDetail::Endpoint), vec![Parameter::Integer(1)])
        .await;
    assert_no_execution_grant(&mut qualification, &fields::inventory_sql(), vec![]).await;
    let layout = fields::observed_layout(&frame.grants.first().unwrap().available_columns);
    assert_no_execution_grant(
        &mut qualification,
        &sql::observation(&layout, IdentityDetail::Endpoint),
        vec![Parameter::Integer(65)],
    )
    .await;
    let targets = (0..frame.activity.records.len()).collect::<Vec<_>>();
    let selected = super::super::selectors(&frame.activity, &targets, false).unwrap();
    assert_no_execution_grant(
        &mut qualification,
        &super::super::sql::details(false, super::super::sql::QueryPolicy::MemoryDiagnostic),
        vec![Parameter::Text(selected), Parameter::Integer(16 * 1024)],
    )
    .await;
    drop(qualification);
    reader.enrich_memory(&mut frame).await.unwrap();
    let context = frame.context.clone();
    let rows = frame.into_observations().unwrap();
    let holder = rows
        .iter()
        .find(|grant| grant.request.as_ref().is_some_and(|request| request.key.session.id == holder_id))
        .unwrap();
    let waiter = rows
        .iter()
        .find(|grant| grant.request.as_ref().is_some_and(|request| request.key.session.id == waiter_id))
        .unwrap();
    assert_eq!(holder.state, ExecutionGrantState::Granted);
    assert_eq!(waiter.state, ExecutionGrantState::Waiting);
    assert!(waiter.granted_kibibytes.is_none() || waiter.granted_kibibytes.as_deref() == Some("0"));
    assert!(holder.granted_kibibytes.as_deref().unwrap().parse::<u64>().unwrap() > 0);
    assert!(holder.resource.is_some());
    assert_eq!(holder.resource, waiter.resource, "both participants use this native Resource Governor semaphore");
    for grant in [holder, waiter] {
        assert!(grant.is_valid());
        assert_eq!(grant.request_association, RequestGrantAssociation::GenerationChecked);
        assert_eq!(grant.statement_relationship, GrantStatementRelationship::ObservedCurrentStatement);
        let request = grant.request.as_ref().unwrap();
        assert!(!request.key.session.generation.ends_with('Z'));
        assert!(!request.key.request.as_ref().unwrap().generation.ends_with('Z'));
        assert!(
            matches!(&request.statement,DefinitionPayload::Available{text,..} if text.contains("request_memory_fixture"))
        );
    }
    let native_sql = format!(
        "SELECT session_id,granted_memory_kb FROM sys.dm_exec_query_memory_grants WHERE session_id IN ({holder_id},{waiter_id})"
    );
    let mut baseline = pool.connect(SessionKind::User).await.unwrap();
    let native = crate::conn::query(&mut baseline, &native_sql).await.unwrap();
    let native_holder = native.iter().find(|row| row.string("session_id") == holder_id).unwrap();
    assert_eq!(holder.granted_kibibytes, optional_counter(native_holder, "granted_memory_kb").unwrap());
    drop(baseline);
    assert!(reader.verify_memory_context(&context).await.unwrap());
    assert_eq!(reader.query_count(), 5);
    let metrics = reader.read_metrics().unwrap();
    assert!(metrics.is_valid());
    assert!(metrics.templates.iter().all(|metric| metric.timeouts == 0 && metric.unconfirmed_cancellations == 0));
    eprintln!(
        "current-grant owned fixture: {} queries, {:.3} client milliseconds, {} participants",
        reader.query_count(),
        metrics.templates.iter().map(|metric| metric.client_elapsed_ms).sum::<f64>(),
        rows.len()
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
}

#[tokio::test]
#[ignore = "requires a disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_memory_grants_identify_current_reservation_holders_and_affected_waiters_without_plan_traversal() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_request_memory_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let fixture = Pool::new(config(port, &database, &password, "request-memory-fixture"), None);
    let mut setup = fixture.connect(SessionKind::User).await.unwrap();
    batch(&mut setup,"CREATE TABLE dbo.request_memory_fixture(id int NOT NULL PRIMARY KEY,payload varchar(2000) NOT NULL);INSERT dbo.request_memory_fixture SELECT TOP(10000) ROW_NUMBER() OVER(ORDER BY a.object_id,b.object_id),REPLICATE('x',2000) FROM sys.all_objects a CROSS JOIN sys.all_objects b").await;
    drop(setup);
    // Preserve a different Culprit session if it is a real reservation holder. Only the observer is excluded.
    let holder_pool = Pool::new(config(port, &database, &password, "culprit"), None);
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut requests = vec![];
    let proof = AssertUnwindSafe(prove_native_grants(pool.clone(), &mut requests, &fixture, &holder_pool, port))
        .catch_unwind()
        .await;
    // Release and join every actor even when a native observation or a plan assertion fails.
    let cleanup_failures = release_requests(requests).await;
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
    if let Err(failure) = proof {
        std::panic::resume_unwind(failure);
    }
    assert!(cleanup_failures.is_empty(), "{}", cleanup_failures.join("; "));
}
